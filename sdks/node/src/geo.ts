// Live geo lookup for proxies — mirrors `geo_check_via` in
// `src-tauri/src/proxy.rs`. Supports the same three providers
// (ip-api.com / ipapi.co / ipwho.is). HTTP/HTTPS proxies use undici's
// ProxyAgent; SOCKS5 uses socks-proxy-agent (the only well-supported
// fetch-compatible SOCKS5 dispatcher in Node land).
import { ProxyAgent, fetch as undiciFetch } from "undici";
import { SocksProxyAgent } from "socks-proxy-agent";
import { request as httpRequest, type IncomingMessage } from "node:http";
import { request as httpsRequest } from "node:https";

import type { ParsedProxy } from "./proxy.js";

export type GeoProvider = "ip-api.com" | "ipapi.co" | "ipwho.is";

export interface GeoInfo {
  ip: string;
  country: string;
  /** ISO-3166 alpha-2. */
  countryCode: string;
  region: string;
  city: string;
  isp: string;
  /** IANA. */
  timezone: string;
  latitude: number;
  longitude: number;
  provider: string;
  /** Comma-separated ISO-639-1; only ipapi.co populates it. */
  languages?: string;
}

const URLS: Record<string, string> = {
  "ip-api.com": "http://ip-api.com/json/?fields=status,message,query,country,countryCode,regionName,city,isp,timezone,lat,lon",
  "ipapi.co":   "https://ipapi.co/json/",
  "ipwho.is":   "https://ipwho.is/",
};

function proxyUrl(p: ParsedProxy, scheme: string): string {
  if (p.username || p.password) {
    // undici/URL decodes userinfo. Not encodeProxyUserinfo — that form is for the engine (#75).
    const u = encodeURIComponent(p.username ?? "");
    const pw = encodeURIComponent(p.password ?? "");
    return `${scheme}://${u}:${pw}@${p.host}:${p.port}`;
  }
  return `${scheme}://${p.host}:${p.port}`;
}

async function fetchJson(url: string, proxy: ParsedProxy | null, timeoutMs: number): Promise<unknown> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    if (!proxy) {
      const r = await undiciFetch(url, { signal: ctl.signal });
      if (!r.ok) throw new Error(`geo HTTP ${r.status}`);
      return await r.json();
    }
    if (proxy.scheme === "socks5") {
      // socks-proxy-agent works with the global fetch on Node 18+
      // by piggy-backing onto http(s).request via dispatcher fallback.
      // We hand-roll a request through http/https so we keep dependency
      // surface small and DNS resolves through the proxy.
      const agent = new SocksProxyAgent(proxyUrl(proxy, "socks5h"));
      return await new Promise((resolve, reject) => {
        const u = new URL(url);
        const reqFn = u.protocol === "https:" ? httpsRequest : httpRequest;
        const req = reqFn({
          host: u.hostname,
          port: u.port || (u.protocol === "https:" ? 443 : 80),
          path: u.pathname + u.search,
          method: "GET",
          agent,
          signal: ctl.signal,
        }, (res: IncomingMessage) => {
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.push(c));
          res.on("end", () => {
            if (typeof res.statusCode === "number" && (res.statusCode < 200 || res.statusCode >= 300)) {
              reject(new Error(`geo HTTP ${res.statusCode}`));
              return;
            }
            try { resolve(JSON.parse(Buffer.concat(chunks).toString("utf8"))); }
            catch (e) { reject(e); }
          });
        });
        req.on("error", reject);
        req.end();
      });
    }
    // HTTP / HTTPS proxy via undici dispatcher.
    const dispatcher = new ProxyAgent(proxyUrl(proxy, proxy.scheme));
    const r = await undiciFetch(url, { signal: ctl.signal, dispatcher });
    if (!r.ok) throw new Error(`geo HTTP ${r.status}`);
    return await r.json();
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Probe the geo `proxy` exits at, or direct geo when `proxy` is null.
 * Throws on network error or provider-level fail (e.g. ip-api.com status=fail).
 */
export async function geoCheckVia(
  proxy: ParsedProxy | null,
  provider: GeoProvider | string = "ip-api.com",
): Promise<GeoInfo> {
  const url = URLS[provider] ?? URLS["ip-api.com"];
  const body = await fetchJson(url, proxy, 8000) as Record<string, any>;

  const s = (k: string): string => (typeof body[k] === "string" ? body[k] : "");
  const f = (k: string): number => {
    const v = body[k];
    const n = typeof v === "number" ? v : Number(v);
    return Number.isFinite(n) ? n : 0;
  };

  if (provider === "ip-api.com") {
    if (s("status") === "fail") throw new Error(`ip-api.com: ${s("message") || "unknown error"}`);
    return {
      ip: s("query"), country: s("country"), countryCode: s("countryCode"),
      region: s("regionName"), city: s("city"), isp: s("isp"),
      timezone: s("timezone"), latitude: f("lat"), longitude: f("lon"),
      provider,
    };
  }
  if (provider === "ipapi.co") {
    return {
      ip: s("ip"), country: s("country_name"), countryCode: s("country_code"),
      region: s("region"), city: s("city"), isp: s("org"),
      timezone: s("timezone"), latitude: f("latitude"), longitude: f("longitude"),
      provider, languages: s("languages"),
    };
  }
  if (provider === "ipwho.is") {
    const conn = (body["connection"] && typeof body["connection"] === "object") ? body["connection"] : {};
    const tz = (body["timezone"] && typeof body["timezone"] === "object") ? body["timezone"] : {};
    return {
      ip: s("ip"), country: s("country"), countryCode: s("country_code"),
      region: s("region"), city: s("city"),
      isp: (typeof conn.isp === "string" ? conn.isp : "") || "",
      timezone: (typeof tz.id === "string" ? tz.id : "") || "",
      latitude: f("latitude"), longitude: f("longitude"),
      provider,
    };
  }
  return {
    ip: s("query"), country: s("country"), countryCode: s("countryCode"),
    region: "", city: "", isp: "", timezone: "",
    latitude: 0, longitude: 0, provider,
  };
}
