// The Widevine CDM, fetched the way Chrome's component updater fetches it:
// Google's update server names the current build, Google's download server
// hands out a CRX3 that must be signed for Widevine's id with the offered hash
// and size. Through the profile's proxy when it has one, straight out if not.
// The engine is built without BUNDLE_WIDEVINE_CDM, so each profile gets the
// cached CDM in `<udd>/WidevineCdm/<version>/` before it starts. Mirrors the
// launcher's widevine.rs, crx.rs and the widevine functions of launch.rs.
import { createHash, createPublicKey, createVerify, randomUUID } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, readlinkSync, renameSync, rmSync, statSync, writeFileSync, copyFileSync } from "node:fs";
import { arch as osArch, platform as osPlatform, release as osKernelRelease } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { request as httpRequest } from "node:http";
import { connect as netConnect, isIP, isIPv4, type Socket } from "node:net";
import { connect as tlsConnect } from "node:tls";
import AdmZip from "adm-zip";

import type { ParsedProxy } from "./proxy.js";

/** Widevine's component id: the SHA-256 of its signing key, a..p. */
export const WIDEVINE_ID = "oimompecagnajdejgnnjijobebaeigek";
export const UPDATE_URL = "https://update.googleapis.com/service/update2/json";
/** The package is about 20 MB; anything far larger is not it. */
const MAX_PACKAGE = 64 << 20;
const MAX_FILES = 64;
/** How often the cached CDM is checked against Google's, as Chrome checks its components. */
export const WIDEVINE_RECHECK_MS = 5 * 3600 * 1000;
/** How long a start waits for a first CDM; it starts without one after that. */
export const WIDEVINE_FIRST_WAIT_MS = 45_000;
const HINT_FILE = "latest-component-updated-widevine-cdm";
const VERSION_RE = /^\d+(\.\d+){0,3}$/;

// ---- the update check ----

/** This computer as Chrome's updater names it: @os, os.platform, arch, os.arch. */
export function updaterPlatform(plat: string = osPlatform(), cpu: string = osArch()): [string, string, string, string] {
  const arm = cpu === "arm64";
  if (plat === "darwin") return ["mac", "Mac OS X", arm ? "arm64" : "x64", arm ? "arm64" : "x86_64"];
  if (plat === "win32") return ["win", "Windows", arm ? "arm64" : "x64", arm ? "arm64" : "x86_64"];
  return ["linux", "Linux", arm ? "arm64" : "x64", arm ? "aarch64" : "x86_64"];
}

/** The OS release Chrome sends: the server answers "noupdate" without one. */
export function osVersion(): string {
  const plat = osPlatform();
  if (plat === "darwin") {
    const r = spawnSync("/usr/sbin/sysctl", ["-n", "kern.osproductversion"], { encoding: "utf8" });
    const v = (r.stdout ?? "").trim();
    if (!/^\d+(\.\d+)*$/.test(v)) return "15.0";
    return v.split(".").length === 2 ? `${v}.0` : v;
  }
  if (plat === "win32") {
    // os.release() is major.minor.build; Chrome adds the update revision (UBR).
    const r = spawnSync("reg", ["query", "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "/v", "UBR"],
      { encoding: "utf8", windowsHide: true });
    const m = /UBR\s+REG_DWORD\s+0x([0-9a-f]+)/i.exec(r.stdout ?? "");
    const base = /^\d+\.\d+\.\d+/.exec(osKernelRelease())?.[0] ?? "10.0.19045";
    return `${base}.${m ? parseInt(m[1], 16) : 0}`;
  }
  return /^[\d.]+/.exec(osKernelRelease())?.[0] ?? "6.0.0";
}

/** The update check, as Chrome 158's component updater sends it (protocol 4.0). */
export function requestBody(chrome: string, have: string | undefined, background: boolean): Record<string, unknown> {
  const [os, platform, arch, osArchName] = updaterPlatform();
  const id = () => `{${randomUUID()}}`;
  return { request: {
    "@os": os,
    "@updater": "chrome",
    acceptformat: "crx3,download,puff,run,xz,zucc",
    apps: [{
      appid: WIDEVINE_ID,
      enabled: true,
      installsource: background ? "" : "ondemand",
      ping: { r: -2 },
      updatecheck: {},
      version: have ?? "0.0.0.0",
    }],
    arch,
    dedup: "cr",
    ismachine: false,
    os: { arch: osArchName, platform, version: osVersion() },
    prodchannel: "stable",
    prodversion: chrome,
    protocol: "4.0",
    requestid: id(),
    sessionid: id(),
    updaterchannel: "stable",
    updaterversion: chrome,
  } };
}

/** What the server offered: the version, its https download urls, hash and size. */
export interface Offer { version: string; urls: string[]; sha256: string; size: number }

/** null for "noupdate"; throws for anything else that is not an offer. */
export function parseOffer(text: string): Offer | null {
  let body: any;
  try { body = JSON.parse(text.replace(/^(\)\]\}')+/, "").trim()); } catch {
    throw new Error("the update server's answer is not JSON");
  }
  const app = body?.response?.apps?.[0];
  const status = app?.updatecheck?.status;
  if (status === "noupdate") return null;
  if (status !== "ok") {
    const why = typeof status === "string" ? status : typeof app?.status === "string" ? app.status : "nothing";
    throw new Error(`the update server answered ${why}`);
  }
  const check = app.updatecheck;
  if (typeof check.nextversion !== "string") throw new Error("no version in the offer");
  const ops = check.pipelines?.[0]?.operations;
  if (!Array.isArray(ops)) throw new Error("no pipeline in the offer");
  const dl = ops.find((o: any) => o?.type === "download");
  if (!dl) throw new Error("no download in the offer");
  const urls = (Array.isArray(dl.urls) ? dl.urls : [])
    .map((u: any) => u?.url)
    .filter((u: unknown): u is string => typeof u === "string" && u.startsWith("https://"));
  const sha256 = dl.out?.sha256;
  if (typeof sha256 !== "string") throw new Error("no hash in the offer");
  if (!Number.isSafeInteger(dl.size) || dl.size < 0) throw new Error("no size in the offer");
  if (urls.length === 0) throw new Error("no https download in the offer");
  return { version: check.nextversion, urls, sha256, size: dl.size };
}

// ---- CRX3, checked as Chrome's crx_verifier.cc checks it ----

const MAX_HEADER = 1 << 20;
const SIGNATURE_CONTEXT = Buffer.from("CRX3 SignedData\x00", "latin1");
/** Zip end-of-directory tokens: in a header they could make unzip see another archive. */
const ZIP_TOKENS = ["PK\x05\x06", "PK\x06\x07", "PK\x06\x06"].map((t) => Buffer.from(t, "latin1"));

/** Why a package was refused; `code` names the case as crx.rs does. */
export class CrxReject extends Error {
  constructor(readonly code: string, message: string) { super(message); }
}

export interface CrxExpect { id?: string; sha256?: string; size?: number }
export interface CrxVerified { crxId: string; developerKey: Buffer; archiveOffset: number; sha256: string }

/** The first 16 bytes of a hash, one letter a..p per nibble. */
export function idFromHash(hash: Uint8Array): string {
  let s = "";
  for (const b of hash.subarray(0, 16)) s += String.fromCharCode(97 + (b >> 4), 97 + (b & 15));
  return s;
}

/** The id a public key (SubjectPublicKeyInfo DER) gives. */
export function idFromKey(spki: Uint8Array): string {
  return idFromHash(createHash("sha256").update(spki).digest());
}

/** The length-delimited fields of a protobuf message, in order; null if malformed. */
function fields(b: Buffer): Array<[number, Buffer]> | null {
  let at = 0;
  const varint = (): number | null => {
    let v = 0;
    for (let shift = 0; shift < 64; shift += 7) {
      if (at >= b.length) return null;
      const byte = b[at++];
      v += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) return v;
    }
    return null;
  };
  const out: Array<[number, Buffer]> = [];
  while (at < b.length) {
    const tag = varint();
    if (tag === null || Math.floor(tag / 8) === 0) return null;
    let skip: number;
    switch (tag % 8) {
      case 0: if (varint() === null) return null; skip = 0; break;
      case 1: skip = 8; break;
      case 2: {
        const len = varint();
        if (len === null || at + len > b.length) return null;
        out.push([Math.floor(tag / 8), b.subarray(at, at + len)]);
        skip = len;
        break;
      }
      case 5: skip = 4; break;
      default: return null;
    }
    if (at + skip > b.length) return null;
    at += skip;
  }
  return out;
}

/** One DER element: tag, contents, what follows. Strict lengths, as BoringSSL reads them. */
function der(b: Buffer): [number, Buffer, Buffer] | null {
  if (b.length < 2) return null;
  const tag = b[0];
  const first = b[1];
  let at = 2;
  let len: number;
  if (first < 0x80) len = first;
  else {
    const n = first & 0x7f;
    if (n === 0 || n > 4 || at >= b.length || b[at] === 0 || at + n > b.length) return null;
    len = 0;
    for (let i = 0; i < n; i++) len = len * 256 + b[at + i];
    at += n;
    if (len < 0x80) return null;
  }
  if (at + len > b.length) return null;
  return [tag, b.subarray(at, at + len), b.subarray(at + len)];
}

const OID_RSA = Buffer.from([0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]);
const OID_EC = Buffer.from([0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]);
const OID_P256 = Buffer.from([0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07]);
const OID_P384 = Buffer.from([0x2b, 0x81, 0x04, 0x00, 0x22]);

/** The kind of a SubjectPublicKeyInfo as a proof of this type needs it; null if unreadable or of the other kind. */
function spkiKind(key: Buffer, rsa: boolean): "rsa" | "p256" | "p384" | null {
  const top = der(key);
  if (!top || top[0] !== 0x30 || top[2].length) return null;
  const alg = der(top[1]);
  if (!alg || alg[0] !== 0x30) return null;
  const bits = der(alg[2]);
  if (!bits || bits[0] !== 0x03 || bits[2].length || bits[1].length < 1 || bits[1][0] !== 0) return null;
  const oid = der(alg[1]);
  if (!oid || oid[0] !== 0x06) return null;
  const params = oid[2];
  if (rsa) return oid[1].equals(OID_RSA) && params.equals(Buffer.from([0x05, 0x00])) ? "rsa" : null;
  if (!oid[1].equals(OID_EC)) return null;
  const curve = der(params);
  if (!curve || curve[0] !== 0x06 || curve[2].length) return null;
  return curve[1].equals(OID_P256) ? "p256" : curve[1].equals(OID_P384) ? "p384" : null;
}

function proofHolds(kind: "rsa" | "p256" | "p384", key: Buffer, sig: Buffer, parts: Buffer[]): boolean {
  try {
    const k = createPublicKey({ key, format: "der", type: "spki" });
    if (kind === "rsa") {
      const bits = k.asymmetricKeyDetails?.modulusLength ?? 0;
      if (k.asymmetricKeyType !== "rsa" || bits < 1024 || bits > 8192) return false;
    } else if (k.asymmetricKeyType !== "ec") return false;
    const v = createVerify("sha256");
    for (const p of parts) v.update(p);
    return v.verify(k, sig);
  } catch { return false; }
}

/** Every proof must verify and one must be the developer's key for the signed id. */
export function verifyCrx(bytes: Buffer, expect: CrxExpect = {}): CrxVerified {
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  if (expect.size !== undefined && expect.size !== bytes.length) {
    throw new CrxReject("WrongSize", `the download is ${bytes.length} bytes, the server said ${expect.size}`);
  }
  if (expect.sha256 !== undefined && expect.sha256.toLowerCase() !== sha256) {
    throw new CrxReject("WrongHash", "the download does not match the server's hash");
  }
  if (bytes.length < 12 || bytes.toString("latin1", 0, 4) !== "Cr24" || bytes.readUInt32LE(4) !== 3) {
    throw new CrxReject("NotCrx3", "not a CRX3 package");
  }
  const headerSize = bytes.readUInt32LE(8);
  if (headerSize > MAX_HEADER || bytes.length - 12 < headerSize) {
    throw new CrxReject("HeaderSize", "the CRX header's size is wrong");
  }
  const header = bytes.subarray(12, 12 + headerSize);
  const archiveOffset = 12 + headerSize;
  if (ZIP_TOKENS.some((t) => header.includes(t))) throw new CrxReject("ZipTokenInHeader", "the CRX header holds a zip marker");
  const bad = () => new CrxReject("BadHeader", "the CRX header cannot be read");

  // CrxFileHeader: proofs 2 (RSA) and 3 (ECDSA), signed_header_data 10000.
  const top = fields(header);
  if (!top) throw bad();
  const proofs: Array<{ rsa: boolean; key: Buffer; sig: Buffer }> = [];
  let signed: Buffer = Buffer.alloc(0);
  for (const [f, data] of top) {
    if (f === 2 || f === 3) {
      const sub = fields(data);
      if (!sub) throw bad();
      let key: Buffer = Buffer.alloc(0), sig: Buffer = Buffer.alloc(0);
      for (const [g, d] of sub) {
        if (g === 1) key = d;
        else if (g === 2) sig = d;
      }
      proofs.push({ rsa: f === 2, key, sig });
    } else if (f === 10000) signed = data;
  }
  const sd = fields(signed);
  if (!sd) throw bad();
  let crxIdBytes: Buffer | undefined;
  for (const [f, d] of sd) if (f === 1) crxIdBytes = d;
  // Chrome compares 32 letters; any other length matches no key.
  const declared = crxIdBytes?.length === 16 ? idFromHash(crxIdBytes) : undefined;

  let developerKey: Buffer | undefined;
  const checks: Array<{ kind: "rsa" | "p256" | "p384"; key: Buffer; sig: Buffer }> = [];
  for (const p of proofs) {
    if (declared !== undefined && declared === idFromKey(p.key)) developerKey = p.key;
    const kind = spkiKind(p.key, p.rsa);
    if (!kind) throw new CrxReject("BadKey", "a key in the CRX cannot be read");
    checks.push({ kind, key: p.key, sig: p.sig });
  }
  if (declared === undefined || !developerKey) {
    throw new CrxReject("NoDeveloperProof", "the CRX is not signed by its developer's key");
  }
  // Every proof covers context, le32(len), signed_header_data and the archive.
  const len = Buffer.alloc(4);
  len.writeUInt32LE(signed.length);
  const parts = [SIGNATURE_CONTEXT, len, signed, bytes.subarray(archiveOffset)];
  for (const c of checks) {
    if (!proofHolds(c.kind, c.key, c.sig, parts)) throw new CrxReject("BadSignature", "the CRX signature does not match its contents");
  }
  if (expect.id !== undefined && expect.id !== declared) {
    throw new CrxReject("WrongId", `the package is ${declared}, not ${expect.id}`);
  }
  return { crxId: declared, developerKey, archiveOffset, sha256 };
}

// ---- download and install into the cache ----

interface HttpResult { status: number; body: Buffer }

const CONNECT_TIMEOUT_MS = 15_000;
const REQUEST_TIMEOUT_MS = 180_000;

/** Aborts when `parent` does or after `ms`; the timer holds the process only when `hold`. */
function deadline(ms: number, hold: boolean, parent?: AbortSignal): { signal: AbortSignal; clear: () => void } {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(new Error("timed out")), ms);
  if (!hold) timer.unref();
  const onAbort = () => ctl.abort(parent?.reason ?? new Error("cancelled"));
  if (parent?.aborted) onAbort();
  parent?.addEventListener("abort", onAbort, { once: true });
  return { signal: ctl.signal, clear: () => { clearTimeout(timer); parent?.removeEventListener("abort", onAbort); } };
}

const reason = (signal: AbortSignal) => (signal.reason instanceof Error ? signal.reason : new Error("cancelled"));

/** Waits for `event` on `s`; fails on an error, a close, or `signal`. */
function waitFor(s: Socket, event: string, signal: AbortSignal): Promise<void> {
  return new Promise((resolveIt, rejectIt) => {
    const done = (e?: Error) => {
      s.off(event, ok); s.off("error", done); s.off("close", closed);
      signal.removeEventListener("abort", aborted);
      if (e) rejectIt(e); else resolveIt();
    };
    const ok = () => done();
    const closed = () => done(new Error("the connection closed"));
    const aborted = () => done(reason(signal));
    if (signal.aborted) { aborted(); return; }
    s.once(event, ok); s.once("error", done); s.once("close", closed);
    signal.addEventListener("abort", aborted, { once: true });
  });
}

/** Exactly `n` bytes from `s`, pulled so that nothing past them is taken. */
function pull(s: Socket, n: number, signal: AbortSignal): Promise<Buffer> {
  return new Promise((resolveIt, rejectIt) => {
    const done = (b?: Buffer, e?: Error) => {
      s.off("readable", tryRead); s.off("end", closed); s.off("close", closed); s.off("error", failed);
      signal.removeEventListener("abort", aborted);
      if (b) resolveIt(b); else rejectIt(e);
    };
    const tryRead = () => { const b = s.read(n) as Buffer | null; if (b) done(b); };
    const closed = () => done(undefined, new Error("the proxy closed the connection"));
    const failed = (e: Error) => done(undefined, e);
    const aborted = () => done(undefined, reason(signal));
    if (signal.aborted) { aborted(); return; }
    s.on("readable", tryRead); s.once("end", closed); s.once("close", closed); s.once("error", failed);
    signal.addEventListener("abort", aborted, { once: true });
    tryRead();
  });
}

/** SOCKS5 CONNECT to host:port, the name resolved at the proxy (socks5h). */
async function socksConnect(s: Socket, host: string, port: number, proxy: ParsedProxy, signal: AbortSignal): Promise<void> {
  const login = !!(proxy.username || proxy.password);
  s.write(Buffer.from(login ? [5, 2, 0, 2] : [5, 1, 0]));
  const [ver, method] = await pull(s, 2, signal);
  if (ver !== 5) throw new Error("the proxy does not speak SOCKS5");
  if (method === 2) {
    const u = Buffer.from(proxy.username ?? ""), pw = Buffer.from(proxy.password ?? "");
    if (u.length > 255 || pw.length > 255) throw new Error("the proxy login is too long for SOCKS5");
    s.write(Buffer.concat([Buffer.from([1, u.length]), u, Buffer.from([pw.length]), pw]));
    if ((await pull(s, 2, signal))[1] !== 0) throw new Error("the SOCKS5 proxy refused the login");
  } else if (method !== 0) {
    throw new Error("the SOCKS5 proxy wants a login method this client does not have");
  }
  const name = Buffer.from(host);
  const addr = isIPv4(host) ? Buffer.from([1, ...host.split(".").map(Number)]) : Buffer.concat([Buffer.from([3, name.length]), name]);
  const portBytes = Buffer.alloc(2);
  portBytes.writeUInt16BE(port);
  s.write(Buffer.concat([Buffer.from([5, 1, 0]), addr, portBytes]));
  const head = await pull(s, 4, signal);
  if (head[0] !== 5 || head[1] !== 0) throw new Error(`the SOCKS5 proxy refused the connection (code ${head[1]})`);
  const rest = head[3] === 1 ? 6 : head[3] === 4 ? 18 : head[3] === 3 ? (await pull(s, 1, signal))[0] + 2 : -1;
  if (rest < 0) throw new Error("the SOCKS5 proxy answered with an unknown address type");
  await pull(s, rest, signal);
}

/** HTTP CONNECT to host:port through an http(s) proxy. */
async function httpConnect(s: Socket, host: string, port: number, proxy: ParsedProxy, signal: AbortSignal): Promise<void> {
  const target = `${host.includes(":") ? `[${host}]` : host}:${port}`;
  const lines = [`CONNECT ${target} HTTP/1.1`, `Host: ${target}`];
  if (proxy.username || proxy.password) {
    lines.push(`Proxy-Authorization: Basic ${Buffer.from(`${proxy.username ?? ""}:${proxy.password ?? ""}`).toString("base64")}`);
  }
  s.write(`${lines.join("\r\n")}\r\n\r\n`);
  let head = "";
  while (!head.endsWith("\r\n\r\n")) {
    if (head.length > 8192) throw new Error("the proxy's answer to CONNECT is too long");
    head += (await pull(s, 1, signal)).toString("latin1");
  }
  const status = /^HTTP\/1\.[01] (\d{3})/.exec(head)?.[1];
  if (status !== "200") throw new Error(`the proxy answered CONNECT with ${status ?? "something that is not HTTP"}`);
}

/** A connection to host:port, through the proxy when there is one. With `hold`
 *  false nothing in it keeps the process alive: a script that is done exits. */
async function openSocket(
  host: string, port: number, secure: boolean, proxy: ParsedProxy | null, hold: boolean, parent: AbortSignal,
): Promise<Socket> {
  const ready = deadline(CONNECT_TIMEOUT_MS, hold, parent);
  const first = proxy ? { host: proxy.host, port: proxy.port } : { host, port };
  let s: Socket = netConnect(first);
  const loose = (x: Socket) => {
    if (hold) return;
    // Happy eyeballs opens a new handle per address: each is let go of too.
    x.unref();
    x.on("connectionAttempt", () => x.unref());
    x.once("connect", () => x.unref());
  };
  loose(s);
  const cut = () => s.destroy();
  ready.signal.addEventListener("abort", cut, { once: true });
  try {
    await waitFor(s, "connect", ready.signal);
    if (proxy?.scheme === "https") {
      s = tlsConnect({ socket: s, servername: isIP(proxy.host) ? undefined : proxy.host });
      loose(s);
      await waitFor(s, "secureConnect", ready.signal);
    }
    if (proxy?.scheme === "socks5") await socksConnect(s, host, port, proxy, ready.signal);
    else if (proxy) await httpConnect(s, host, port, proxy, ready.signal);
    if (secure) {
      s = tlsConnect({ socket: s, servername: isIP(host) ? undefined : host, ALPNProtocols: ["http/1.1"] });
      loose(s);
      await waitFor(s, "secureConnect", ready.signal);
    }
    return s;
  } catch (e) {
    s.destroy();
    throw e;
  } finally {
    ready.signal.removeEventListener("abort", cut);
    ready.clear();
  }
}

/** One HTTP/1.1 exchange over `sock`, which it closes; the body capped at `maxBytes`. */
function exchange(
  sock: Socket, method: string, u: URL, headers: Record<string, string>, body: string | undefined,
  maxBytes: number, signal: AbortSignal,
): Promise<HttpResult & { location?: string }> {
  return new Promise<HttpResult & { location?: string }>((resolveIt, rejectIt) => {
    let settled = false;
    const done = (v?: HttpResult & { location?: string }, e?: Error) => {
      if (settled) return;
      settled = true;
      if (v) resolveIt(v); else rejectIt(e);
    };
    const all: Record<string, string> = { ...headers, Host: u.host, Connection: "close" };
    if (body !== undefined) all["Content-Length"] = String(Buffer.byteLength(body));
    const req = httpRequest({ method, path: `${u.pathname}${u.search}`, headers: all, createConnection: () => sock, signal }, (r) => {
      const status = r.statusCode ?? 0;
      if (status >= 300 && status < 400 && r.headers.location) {
        r.destroy();
        done({ status, location: r.headers.location, body: Buffer.alloc(0) });
        return;
      }
      const chunks: Buffer[] = [];
      let got = 0;
      r.on("data", (c: Buffer) => {
        got += c.length;
        if (got > maxBytes) { r.destroy(); done(undefined, new Error(`the answer from ${u.host} is larger than expected`)); return; }
        chunks.push(c);
      });
      r.on("end", () => done({ status, body: Buffer.concat(chunks) }));
      r.on("error", (e) => done(undefined, e));
      r.on("close", () => done(undefined, new Error(`the connection to ${u.host} closed before the answer was complete`)));
    });
    req.on("error", (e) => done(undefined, e));
    req.end(body);
  }).finally(() => sock.destroy());
}

/** One request, straight out or through the profile's proxy, redirects followed.
 *  `hold` false: its sockets and timers do not keep the process alive. */
async function httpCall(
  method: "GET" | "POST", url: string, headers: Record<string, string>, body: string | undefined,
  proxy: ParsedProxy | null, maxBytes: number, hold: boolean, signal?: AbortSignal,
): Promise<HttpResult> {
  const whole = deadline(REQUEST_TIMEOUT_MS, hold, signal);
  try {
    for (let hop = 0; hop < 10; hop++) {
      const u = new URL(url);
      const secure = u.protocol === "https:";
      if (!secure && u.protocol !== "http:") throw new Error(`cannot fetch ${u.protocol} addresses`);
      const host = u.hostname.replace(/^\[|\]$/g, "");
      const sock = await openSocket(host, Number(u.port) || (secure ? 443 : 80), secure, proxy, hold, whole.signal);
      const res = await exchange(sock, method, u, headers, body, maxBytes, whole.signal);
      if (!res.location || method !== "GET") return { status: res.status, body: res.body };
      url = new URL(res.location, u).toString();
    }
    throw new Error("too many redirects");
  } finally {
    whole.clear();
  }
}

/** Fetches the current CDM into `cache` when the server has a newer one than
 *  `have`. Returns the version put there, or null when `have` is current. A
 *  background check never keeps the process alive; a first one does. */
export async function fetchWidevine(
  proxy: ParsedProxy | null, chrome: string, have: string | undefined, cache: string,
  background: boolean, signal?: AbortSignal, updateUrl: string = UPDATE_URL,
): Promise<string | null> {
  const major = chrome.split(".")[0] || "158";
  const ua = `Mozilla/5.0 Chrome/${major}.0.0.0`;
  const hold = !background;
  let res: HttpResult;
  try {
    res = await httpCall("POST", updateUrl, {
      "User-Agent": ua,
      "Content-Type": "application/json",
      "X-Goog-Update-AppId": WIDEVINE_ID,
      "X-Goog-Update-Interactivity": background ? "bg" : "fg",
      "X-Goog-Update-Updater": `chrome-${chrome}`,
    }, JSON.stringify(requestBody(chrome, have, background)), proxy, 1 << 20, hold, signal);
  } catch (e) {
    throw new Error(`the update server could not be reached: ${(e as Error).message}`);
  }
  if (res.status < 200 || res.status >= 300) throw new Error(`the update server answered HTTP ${res.status}`);
  const offer = parseOffer(res.body.toString("utf8"));
  if (!offer) return null;
  if (offer.size > MAX_PACKAGE) throw new Error(`the offered package is ${offer.size >> 20} MB, more than a CDM`);
  let last: Error | undefined;
  for (const url of offer.urls) {
    let bytes: Buffer, verified: CrxVerified;
    try {
      const r = await httpCall("GET", url, { "User-Agent": ua }, undefined, proxy, offer.size + 1, hold, signal);
      if (r.status < 200 || r.status >= 300) throw new Error(`HTTP ${r.status} from ${url}`);
      bytes = r.body;
      try { verified = verifyCrx(bytes, { id: WIDEVINE_ID, sha256: offer.sha256, size: offer.size }); } catch (e) {
        throw new Error(`the package failed its check: ${(e as Error).message}`);
      }
    } catch (e) {
      last = e as Error;
      if (signal?.aborted) break;
      continue;
    }
    installPackage(bytes, verified.archiveOffset, offer.version, cache);
    return offer.version;
  }
  throw last ?? new Error("no download worked");
}

/** Unpacks a verified package beside `cache`, checks its manifest names the
 *  offered version, then puts it in the cache's place. */
function installPackage(bytes: Buffer, archiveOffset: number, version: string, cache: string): void {
  const staging = `${cache}.incoming-${process.pid}`;
  rmQuiet(staging);
  try {
    unzipWithin(bytes.subarray(archiveOffset), staging, MAX_PACKAGE * 2, MAX_FILES);
    if (cdmVersionOf(staging) !== version) {
      throw new Error(existsSync(join(staging, "manifest.json"))
        ? "the package's manifest names another version than the offer"
        : "no manifest.json in the package");
    }
    rmSync(cache, { recursive: true, force: true });
    renameSync(staging, cache);
  } finally {
    rmQuiet(staging);
  }
}

/** A relative path inside the archive, or null for one that would leave it. */
function enclosedName(name: string): string | null {
  if (name.includes("\0")) return null;
  const s = name.replace(/\\/g, "/");
  if (s.startsWith("/") || /^[A-Za-z]:/.test(s)) return null;
  const parts = s.split("/").filter((p) => p !== "" && p !== ".");
  if (parts.length === 0 || parts.includes("..")) return null;
  return parts.join(sep);
}

function unzipWithin(archive: Buffer, dst: string, max: number, most: number): void {
  let zip: AdmZip;
  try { zip = new AdmZip(archive); } catch { throw new Error("not a zip/crx archive"); }
  const entries = zip.getEntries();
  if (entries.length > most) throw new Error(`the archive holds more than ${most} files`);
  const tooBig = () => new Error(`the package unpacks to more than ${max >> 20} MB`);
  if (entries.reduce((n, e) => n + (e.header.size || 0), 0) > max) throw tooBig();
  mkdirSync(dst, { recursive: true });
  let total = 0;
  for (const e of entries) {
    const rel = enclosedName(e.entryName);
    if (!rel) continue;
    const out = join(dst, rel);
    if (e.isDirectory) { mkdirSync(out, { recursive: true }); continue; }
    mkdirSync(dirname(out), { recursive: true });
    const data = e.getData();
    total += data.length;
    if (total > max) throw tooBig();
    writeFileSync(out, data);
  }
}

// ---- the cache and each profile ----

/** Version string as numbers, so "4.10.2891.0" sorts above "4.9.9999.0". */
export function compareCdmVersions(a: string, b: string): number {
  const pa = a.split(".").map((p) => (/^\d+$/.test(p) ? Number(p) : 0));
  const pb = b.split(".").map((p) => (/^\d+$/.test(p) ? Number(p) : 0));
  for (let i = 0; i < Math.min(pa.length, pb.length); i++) if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1;
  return pa.length - pb.length;
}

export function cdmVersionOf(dir: string): string | undefined {
  try {
    const v = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"))?.version;
    return typeof v === "string" ? v : undefined;
  } catch { return undefined; }
}

function checkedLately(stamp: string): boolean {
  try {
    // Date.now() is whole milliseconds and some disks round times up: a little ahead is now.
    const age = Date.now() - statSync(stamp).mtimeMs;
    return age > -2000 && age < WIDEVINE_RECHECK_MS;
  } catch { return false; }
}

export type WidevineFetcher = typeof fetchWidevine;

let oneAtATime: Promise<unknown> = Promise.resolve();

/** A first CDM is waited for (up to 45 s); a cached one is checked for a newer
 *  build in the background, every 5 hours. `cache` is `<cacheDir>/widevine-cdm`. */
export async function ensureWidevine(
  cache: string, chrome: string, proxy: ParsedProxy | null,
  opts: { fetcher?: WidevineFetcher; firstWaitMs?: number; updateUrl?: string } = {},
): Promise<void> {
  const fetcher: WidevineFetcher = opts.fetcher
    ?? ((p, c, h, d, bg, sig) => fetchWidevine(p, c, h, d, bg, sig, opts.updateUrl));
  const stamp = `${cache}.checked`;
  if (cdmVersionOf(cache) !== undefined && checkedLately(stamp)) return;
  const first = cdmVersionOf(cache) === undefined;
  const ctl = new AbortController();
  const check = async () => {
    const have = cdmVersionOf(cache);
    if ((have !== undefined && checkedLately(stamp)) || ctl.signal.aborted) return;
    try {
      const got = await fetcher(proxy, chrome, have, cache, have !== undefined, ctl.signal);
      if (got) console.log(`[shardx] Widevine CDM ${got} from Google`);
      writeFileSync(stamp, "");
    } catch (e) {
      if (!ctl.signal.aborted) console.warn(`[shardx] Widevine check failed: ${(e as Error).message}`);
    }
  };
  // One check at a time, however many profiles start together.
  const run = oneAtATime.then(check);
  oneAtATime = run.catch(() => {});
  if (!first) return;
  let timer: NodeJS.Timeout | undefined;
  const late = await Promise.race([
    run.then(() => false),
    new Promise<boolean>((r) => { timer = setTimeout(() => r(true), opts.firstWaitMs ?? WIDEVINE_FIRST_WAIT_MS); }),
  ]);
  clearTimeout(timer);
  if (late) {
    ctl.abort();
    console.warn("[shardx] no Widevine CDM yet: Google did not answer in time; this profile starts without one");
  }
}

/** Take a CDM the engine fetched for itself into the cache when it is newer,
 *  so the next profile starts with it. Returns the version taken, if any. */
export function harvestWidevine(cache: string, udd: string): string | undefined {
  const root = join(udd, "WidevineCdm");
  if (!existsSync(root)) return undefined;
  let newest: { v: string; p: string } | undefined;
  for (const name of readdirSync(root)) {
    const p = join(root, name);
    if (!isDir(p)) continue;
    // The engine names each folder by its version; anything else is not one of its.
    const v = cdmVersionOf(p);
    if (v === undefined || v !== name || !VERSION_RE.test(v)) continue;
    if (!newest || compareCdmVersions(v, newest.v) > 0) newest = { v, p };
  }
  if (!newest) return undefined;
  const have = cdmVersionOf(cache);
  if (have !== undefined && compareCdmVersions(have, newest.v) >= 0) return undefined;
  // Into a sibling first: a half-copied cache is worse than none.
  const staging = `${cache}.incoming-${process.pid}`;
  rmQuiet(staging);
  try {
    copyDir(newest.p, staging);
    rmSync(cache, { recursive: true, force: true });
    renameSync(staging, cache);
  } finally {
    rmQuiet(staging);
  }
  console.log(`[shardx] Widevine CDM ${newest.v} cached from a profile`);
  return newest.v;
}

/** Copies the cached CDM into `<udd>/WidevineCdm/<version>/`, the layout
 *  Chromium's component installer reads, and writes the Linux hint. Returns
 *  the version folder, or undefined while nothing is cached. */
export function installWidevine(cache: string, udd: string): string | undefined {
  if (!existsSync(cache)) return undefined;
  const version = cdmVersionOf(cache);
  if (version === undefined || !VERSION_RE.test(version)) {
    throw new Error(`the cached CDM at ${cache} names no usable version`);
  }
  const root = join(udd, "WidevineCdm");
  const versioned = join(root, version);
  if (!existsSync(versioned)) {
    // A flat layout an older SDK or launcher left goes.
    if (existsSync(join(root, "manifest.json"))) {
      for (const stray of ["manifest.json", "LICENSE", "_platform_specific"]) rmQuiet(join(root, stray));
    }
    const staging = join(root, `.${version}.incoming`);
    rmQuiet(staging);
    try {
      copyDir(cache, staging);
      renameSync(staging, versioned);
    } finally {
      rmQuiet(staging);
    }
  }
  writeWidevineHint(root, versioned);
  return versioned;
}

/** The hint Chrome on Linux loads its component CDM from at start, before the
 *  sandbox closes; harmless on other platforms. */
export function writeWidevineHint(root: string, versioned: string): void {
  const hint = join(root, HINT_FILE);
  const body = JSON.stringify({ Path: resolve(versioned) });
  let now: string | undefined;
  try { now = readFileSync(hint, "utf8"); } catch { /* none yet */ }
  if (now !== body) writeFileSync(hint, body);
}

function isDir(p: string): boolean {
  try { return statSync(p).isDirectory(); } catch { return false; }
}

/** A plain copy; links are resolved so the copy stands on its own. */
function copyDir(src: string, dst: string): void {
  mkdirSync(dst, { recursive: true });
  for (const name of readdirSync(src)) {
    const from = join(src, name), to = join(dst, name);
    const st = lstatSync(from);
    if (st.isSymbolicLink()) {
      const target = resolve(dirname(from), readlinkSync(from));
      if (isDir(target)) copyDir(target, to); else copyFileSync(target, to);
    } else if (st.isDirectory()) copyDir(from, to);
    else copyFileSync(from, to);
  }
}

function rmQuiet(p: string): void {
  try { rmSync(p, { recursive: true, force: true }); } catch { /* clutter */ }
}
