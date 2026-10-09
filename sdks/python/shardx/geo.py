"""Live geo lookup for proxies — mirrors `geo_check_via` in
`src-tauri/src/proxy.rs`. Supports the same three providers
(ip-api.com / ipapi.co / ipwho.is) and uses `socks5h://` for SOCKS5 so
DNS is resolved through the proxy."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional
from urllib.parse import quote

import httpx

from .proxy import ParsedProxy


@dataclass
class GeoInfo:
    ip: str
    country: str
    country_code: str   # ISO-3166 alpha-2
    region: str
    city: str
    isp: str
    timezone: str       # IANA, "" if provider didn't return one
    latitude: float
    longitude: float
    provider: str
    languages: str = "" # comma-separated ISO-639-1, only ipapi.co populates it


_URLS = {
    "ip-api.com": "http://ip-api.com/json/?fields=status,message,query,country,countryCode,regionName,city,isp,timezone,lat,lon",
    "ipapi.co":   "https://ipapi.co/json/",
    "ipwho.is":   "https://ipwho.is/",
}


def _proxy_url(p: ParsedProxy) -> str:
    """Build a proxy URL httpx accepts. `socks5h://` lets DNS go through the proxy."""
    scheme = "socks5h" if p.scheme == "socks5" else p.scheme
    if p.username or p.password:
        # httpx decodes userinfo. Not encode_proxy_userinfo — that form is for the engine (#75).
        u = quote(p.username or "", safe="")
        pw = quote(p.password or "", safe="")
        return f"{scheme}://{u}:{pw}@{p.host}:{p.port}"
    return f"{scheme}://{p.host}:{p.port}"


def geo_check_via(proxy: Optional[ParsedProxy], provider: str = "ip-api.com") -> GeoInfo:
    """Probe the geo `proxy` exits at, or direct geo when `proxy` is None.

    Raises `httpx.HTTPError` on network failure and `RuntimeError` for
    provider-level errors (e.g. ip-api.com `status=fail`).
    """
    url = _URLS.get(provider, _URLS["ip-api.com"])
    # httpx 0.28 dropped the `proxies={...}` mapping in favour of the
    # singular `proxy=...`. Both .Client and the new APIs accept it.
    kwargs: dict = {"timeout": 8.0}
    if proxy is not None:
        kwargs["proxy"] = _proxy_url(proxy)
    else:
        kwargs["trust_env"] = False   # bypass system proxy on direct check

    with httpx.Client(**kwargs) as c:
        r = c.get(url)
        r.raise_for_status()
        body = r.json()

    def s(key: str) -> str:
        v = body.get(key)
        return v if isinstance(v, str) else ""

    def f(key: str) -> float:
        v = body.get(key)
        try:
            return float(v) if v is not None else 0.0
        except (TypeError, ValueError):
            return 0.0

    if provider == "ip-api.com":
        if s("status") == "fail":
            raise RuntimeError(f"ip-api.com: {s('message') or 'unknown error'}")
        return GeoInfo(
            ip=s("query"),
            country=s("country"),
            country_code=s("countryCode"),
            region=s("regionName"),
            city=s("city"),
            isp=s("isp"),
            timezone=s("timezone"),
            latitude=f("lat"),
            longitude=f("lon"),
            provider=provider,
        )
    if provider == "ipapi.co":
        return GeoInfo(
            ip=s("ip"),
            country=s("country_name"),
            country_code=s("country_code"),
            region=s("region"),
            city=s("city"),
            isp=s("org"),
            timezone=s("timezone"),
            latitude=f("latitude"),
            longitude=f("longitude"),
            provider=provider,
            languages=s("languages"),
        )
    if provider == "ipwho.is":
        conn = body.get("connection") or {}
        tz = body.get("timezone") or {}
        return GeoInfo(
            ip=s("ip"),
            country=s("country"),
            country_code=s("country_code"),
            region=s("region"),
            city=s("city"),
            isp=(conn.get("isp") if isinstance(conn, dict) else "") or "",
            timezone=(tz.get("id") if isinstance(tz, dict) else "") or "",
            latitude=f("latitude"),
            longitude=f("longitude"),
            provider=provider,
        )
    # Unknown provider — best-effort decode.
    return GeoInfo(
        ip=s("query"), country=s("country"), country_code=s("countryCode"),
        region="", city="", isp="", timezone="",
        latitude=0.0, longitude=0.0, provider=provider,
    )
