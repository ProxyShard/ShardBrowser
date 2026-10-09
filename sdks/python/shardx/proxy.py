"""Proxy URL parsing + SOCKS5 UDP_ASSOCIATE probe.

The probe mirrors what the launcher does before deciding whether to
enable QUIC and which WebRTC policy to apply:

  • TCP control: greet (no-auth + user/pass), sub-negotiate auth,
    send CMD=0x03 (UDP_ASSOCIATE) with BND.ADDR=0.0.0.0:0.
  • If the server replies REP=0x00 (success), parse the relay
    endpoint and send a wrapped STUN Binding Request (RFC 5389)
    to a public STUN server through the relay — this is the same
    UDP path WebRTC actually uses.
  • If a STUN Binding Success Response with our txn-id comes back,
    return RTT in ms.

Anything short of a full round-trip → return None → caller falls
back to "no UDP available" (disable QUIC, force tcp_only WebRTC).
"""
from __future__ import annotations

import secrets
import socket
import struct
import time
from dataclasses import dataclass
from typing import Optional
from urllib.parse import unquote, urlparse

# Bytes the engine forwards literally from `--proxy-server` userinfo.
_LITERAL_USERINFO = set(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~!$&'()*+,;=")


def encode_proxy_userinfo(raw: str) -> str:
    """Percent-encode only octets that split the proxy argument. `;` and `,` stay raw."""
    out: list[str] = []
    for b in raw.encode("utf-8"):
        if b in _LITERAL_USERINFO:
            out.append(chr(b))
        else:
            out.append(f"%{b:02X}")
    return "".join(out)


# Cloudflare's anycast STUN (162.159.207.55:3478, stable IP literal so
# the relay doesn't need DNS).  Two fallbacks pinned to known IPs.
_STUN_TARGETS: tuple[tuple[str, int], ...] = (
    ("162.159.207.55", 3478),     # stun.cloudflare.com
    ("74.125.250.129", 19302),    # one of stun.l.google.com (Google)
    ("18.197.99.114", 3478),      # stun.nextcloud.com
)
_STUN_MAGIC = b"\x21\x12\xa4\x42"


@dataclass(frozen=True)
class ParsedProxy:
    scheme: str          # "socks5" | "http" | "https"
    host: str
    port: int
    username: Optional[str]
    password: Optional[str]

    @property
    def is_socks5(self) -> bool:
        return self.scheme == "socks5"

    def to_arg(self) -> str:
        """Format as ShardX engine's `--proxy-server` argument.

        The fork honours inline credentials (stock Chromium does not).
        Mirrors the launcher's `ProxyEntry::to_proxy_server_arg`: the
        engine does not percent-decode this userinfo (#75), so `;` and
        `,` stay literal.
        """
        host_port = f"{self.host}:{self.port}"
        if self.username or self.password:
            u = encode_proxy_userinfo(self.username or "")
            p = encode_proxy_userinfo(self.password or "")
            return f"{self.scheme}://{u}:{p}@{host_port}"
        return f"{self.scheme}://{host_port}"


def parse_proxy(url: str) -> ParsedProxy:
    """Accept `socks5://user:pass@host:port`, `http://host:port`, etc."""
    u = urlparse(url)
    scheme = (u.scheme or "socks5").lower()
    if scheme not in ("socks5", "http", "https"):
        raise ValueError(f"Unsupported proxy scheme: {scheme}")
    if not u.hostname or not u.port:
        raise ValueError(f"Proxy URL must include host and port: {url}")
    return ParsedProxy(
        scheme=scheme,
        host=u.hostname,
        port=u.port,
        username=unquote(u.username) if u.username else None,
        password=unquote(u.password) if u.password else None,
    )


def probe_udp(proxy: ParsedProxy, timeout: float = 6.0) -> Optional[float]:
    """Return UDP RTT (ms) through the SOCKS5 relay, or None if unavailable."""
    if not proxy.is_socks5:
        return None
    started = time.monotonic()
    try:
        tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        tcp.settimeout(timeout)
        tcp.connect((proxy.host, proxy.port))

        # Greet: ver=5, nmethods=2, methods=[0x00 no-auth, 0x02 user/pass]
        tcp.sendall(bytes([0x05, 0x02, 0x00, 0x02]))
        g = tcp.recv(2)
        if len(g) < 2 or g[0] != 0x05:
            return None
        if g[1] == 0x02:
            if not (proxy.username and proxy.password):
                return None
            u = proxy.username.encode()
            p = proxy.password.encode()
            tcp.sendall(bytes([0x01, len(u)]) + u + bytes([len(p)]) + p)
            a = tcp.recv(2)
            if len(a) < 2 or a[1] != 0x00:
                return None
        elif g[1] != 0x00:
            return None

        # UDP_ASSOCIATE: ver=5, cmd=3, rsv=0, atyp=1, BND=0.0.0.0:0
        tcp.sendall(bytes([0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0]))
        r = tcp.recv(64)
        if len(r) < 10 or r[1] != 0x00:
            return None

        atyp = r[3]
        if atyp == 0x01:
            relay_ip = ".".join(str(b) for b in r[4:8])
            relay_port = struct.unpack("!H", r[8:10])[0]
        elif atyp == 0x04:
            relay_ip = socket.inet_ntop(socket.AF_INET6, r[4:20])
            relay_port = struct.unpack("!H", r[20:22])[0]
        else:
            return None
        if relay_ip in ("0.0.0.0", "::"):
            relay_ip = proxy.host

        # Send a wrapped STUN Binding Request (RFC 5389) through the
        # relay to a public STUN server.  Same UDP shape WebRTC uses.
        udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        udp.settimeout(timeout / max(1, len(_STUN_TARGETS)))
        try:
            for stun_ip, stun_port in _STUN_TARGETS:
                txn = secrets.token_bytes(12)
                stun_req = b"\x00\x01\x00\x00" + _STUN_MAGIC + txn   # 20-byte header, no attrs
                wrap = (
                    b"\x00\x00\x00\x01"                              # RSV(2) FRAG(1) ATYP=IPv4
                    + socket.inet_aton(stun_ip)
                    + struct.pack("!H", stun_port)
                    + stun_req
                )
                try:
                    udp.sendto(wrap, (relay_ip, relay_port))
                    reply, _ = udp.recvfrom(2048)
                except socket.timeout:
                    continue
                # SOCKS5 UDP wrapper: RSV(2)=0 FRAG(1)=0 ATYP + addr + port.
                if len(reply) < 10 or reply[:3] != b"\x00\x00\x00":
                    continue
                atyp2 = reply[3]
                offset = 10 if atyp2 == 0x01 else 22 if atyp2 == 0x04 else 0
                if offset == 0 or len(reply) < offset + 20:
                    continue
                stun_resp = reply[offset:]
                # STUN Binding Success Response (0x0101) + matching magic + txn.
                if (stun_resp[0:2] == b"\x01\x01"
                        and stun_resp[4:8] == _STUN_MAGIC
                        and stun_resp[8:20] == txn):
                    return (time.monotonic() - started) * 1000.0
            return None
        finally:
            udp.close()
    except (OSError, socket.timeout):
        return None
    finally:
        try: tcp.close()
        except Exception: pass
