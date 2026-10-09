// Proxy URL parsing + SOCKS5 UDP_ASSOCIATE probe. Mirrors the launcher's
// pre-launch UDP check that gates whether to enable QUIC and which
// WebRTC policy to apply.
import { createConnection, type Socket } from "node:net";
import { createSocket } from "node:dgram";
import { randomBytes } from "node:crypto";

// Cloudflare anycast + Google + Nextcloud — IP literals so the relay
// doesn't have to resolve a hostname.  We try them in order and take
// the first STUN Binding Success Response that comes back.
const STUN_TARGETS: ReadonlyArray<readonly [string, number]> = [
  ["162.159.207.55", 3478],   // stun.cloudflare.com
  ["74.125.250.129", 19302],  // one of stun.l.google.com
  ["18.197.99.114",  3478],   // stun.nextcloud.com
];
const STUN_MAGIC = Buffer.from([0x21, 0x12, 0xa4, 0x42]);

export interface ParsedProxy {
  scheme: "socks5" | "http" | "https";
  host: string;
  port: number;
  username?: string;
  password?: string;
}

export function parseProxy(url: string): ParsedProxy {
  const u = new URL(url);
  const scheme = (u.protocol.replace(":", "") || "socks5").toLowerCase();
  if (scheme !== "socks5" && scheme !== "http" && scheme !== "https") {
    throw new Error(`Unsupported proxy scheme: ${scheme}`);
  }
  if (!u.hostname || !u.port) {
    throw new Error(`Proxy URL must include host and port: ${url}`);
  }
  return {
    scheme: scheme as ParsedProxy["scheme"],
    host: u.hostname,
    port: Number(u.port),
    username: u.username ? decodeURIComponent(u.username) : undefined,
    password: u.password ? decodeURIComponent(u.password) : undefined,
  };
}

/** Octets the engine forwards literally. Everything else is `%HH` — the
 *  engine does not percent-decode `--proxy-server` userinfo (#75), so
 *  `encodeURIComponent` would turn `;` / `,` into a username the proxy rejects. */
const LITERAL_USERINFO = /[A-Za-z0-9\-._~!$&'()*+,;=]/;

export function encodeProxyUserinfo(raw: string): string {
  let out = "";
  for (const b of Buffer.from(raw, "utf8")) {
    const ch = String.fromCharCode(b);
    out += LITERAL_USERINFO.test(ch) ? ch : `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

/** Format as the ShardX engine's `--proxy-server` argument. The ShardX
 *  fork honours inline credentials in `--proxy-server` (stock Chromium
 *  does not). Mirrors the launcher's `ProxyEntry::to_proxy_server_arg`. */
export function proxyToArg(p: ParsedProxy): string {
  const hostPort = `${p.host}:${p.port}`;
  if (p.username || p.password) {
    return `${p.scheme}://${encodeProxyUserinfo(p.username ?? "")}:${encodeProxyUserinfo(p.password ?? "")}@${hostPort}`;
  }
  return `${p.scheme}://${hostPort}`;
}

/** Return UDP RTT (ms) through the SOCKS5 relay, or null if unavailable. */
export async function probeUdp(p: ParsedProxy, timeoutMs = 6000): Promise<number | null> {
  if (p.scheme !== "socks5") return null;
  const started = Date.now();

  return await new Promise<number | null>((resolve) => {
    let tcp: Socket | null = null;
    let finished = false;
    const done = (val: number | null) => {
      if (finished) return;
      finished = true;
      try { tcp?.destroy(); } catch { /* ignore */ }
      resolve(val);
    };
    const overall = setTimeout(() => done(null), timeoutMs);

    tcp = createConnection({ host: p.host, port: p.port }, () => {
      // Greet: ver=5, nmethods=2, methods=[0x00 no-auth, 0x02 user/pass]
      tcp!.write(Buffer.from([0x05, 0x02, 0x00, 0x02]));
    });
    tcp.setTimeout(timeoutMs);
    tcp.on("timeout",  () => done(null));
    tcp.on("error",    () => done(null));

    let state: "greet" | "auth" | "assoc" = "greet";
    let buf = Buffer.alloc(0);

    tcp.on("data", async (chunk) => {
      buf = Buffer.concat([buf, chunk]);

      if (state === "greet" && buf.length >= 2) {
        const method = buf[1];
        buf = buf.subarray(2);
        if (method === 0x02) {
          if (!p.username || !p.password) return done(null);
          const u = Buffer.from(p.username);
          const pw = Buffer.from(p.password);
          tcp!.write(Buffer.concat([
            Buffer.from([0x01, u.length]), u,
            Buffer.from([pw.length]), pw,
          ]));
          state = "auth";
        } else if (method === 0x00) {
          tcp!.write(Buffer.from([0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0]));
          state = "assoc";
        } else { return done(null); }
      }

      if (state === "auth" && buf.length >= 2) {
        if (buf[1] !== 0x00) return done(null);
        buf = buf.subarray(2);
        tcp!.write(Buffer.from([0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0]));
        state = "assoc";
      }

      if (state === "assoc" && buf.length >= 10) {
        if (buf[1] !== 0x00) return done(null);
        const atyp = buf[3];
        let relayHost: string;
        let relayPort: number;
        if (atyp === 0x01) {
          relayHost = `${buf[4]}.${buf[5]}.${buf[6]}.${buf[7]}`;
          relayPort = buf.readUInt16BE(8);
        } else if (atyp === 0x04 && buf.length >= 22) {
          relayHost = Array.from(buf.subarray(4, 20))
            .map((b) => b.toString(16).padStart(2, "0"))
            .reduce((acc, h, i) => acc + (i > 0 && i % 2 === 0 ? ":" : "") + h, "");
          relayPort = buf.readUInt16BE(20);
        } else { return done(null); }
        if (relayHost === "0.0.0.0" || relayHost === "::") relayHost = p.host;

        // Send a wrapped STUN Binding Request (RFC 5389) through the relay
        // to a public STUN server. Same UDP shape WebRTC actually uses.
        const udp = createSocket("udp4");
        let stunIdx = 0;
        let currentTxn: Buffer = Buffer.alloc(0);
        const udpTimer = setTimeout(() => { try { udp.close(); } catch {} done(null); }, timeoutMs);

        const sendNextStun = () => {
          if (stunIdx >= STUN_TARGETS.length) {
            clearTimeout(udpTimer);
            try { udp.close(); } catch {}
            done(null);
            return;
          }
          const [stunIp, stunPort] = STUN_TARGETS[stunIdx++];
          currentTxn = randomBytes(12);
          const stunReq = Buffer.concat([
            Buffer.from([0x00, 0x01, 0x00, 0x00]),   // type=Binding Request, length=0
            STUN_MAGIC,
            currentTxn,
          ]);
          const ipParts = stunIp.split(".").map(Number);
          const portBuf = Buffer.alloc(2); portBuf.writeUInt16BE(stunPort, 0);
          const wrap = Buffer.concat([
            Buffer.from([0x00, 0x00, 0x00, 0x01, ...ipParts]),  // RSV(2) FRAG(1) ATYP=IPv4
            portBuf,
            stunReq,
          ]);
          udp.send(wrap, relayPort, relayHost, (err) => {
            if (err) { clearTimeout(udpTimer); try { udp.close(); } catch {} done(null); }
          });
        };

        udp.on("error", () => { clearTimeout(udpTimer); try { udp.close(); } catch {} done(null); });
        udp.on("message", (msg) => {
          // SOCKS5 UDP wrapper: RSV(2)=0 FRAG(1)=0 ATYP + addr + port + payload.
          if (msg.length < 10 || msg[0] !== 0 || msg[1] !== 0 || msg[2] !== 0) {
            sendNextStun(); return;
          }
          const atyp2 = msg[3];
          const offset = atyp2 === 0x01 ? 10 : atyp2 === 0x04 ? 22 : -1;
          if (offset < 0 || msg.length < offset + 20) { sendNextStun(); return; }
          const stunResp = msg.subarray(offset);
          // Binding Success Response (0x0101) + matching magic + txn.
          if (stunResp[0] === 0x01 && stunResp[1] === 0x01
              && stunResp.subarray(4, 8).equals(STUN_MAGIC)
              && stunResp.subarray(8, 20).equals(currentTxn)) {
            clearTimeout(udpTimer); clearTimeout(overall);
            try { udp.close(); } catch {}
            done(Date.now() - started);
          } else {
            sendNextStun();
          }
        });
        sendNextStun();
      }
    });

    tcp.on("close", () => done(null));
  }).finally(() => { /* TCP socket cleaned up in done() */ });
}
