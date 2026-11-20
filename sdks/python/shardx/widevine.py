"""The Widevine CDM, fetched the way Chrome's component updater fetches it:
Google's update server names the current build, Google's download server hands
out a CRX3 that must be signed for Widevine's id with the offered hash and
size. Through the profile's proxy when it has one, straight out if not. The
engine is built without BUNDLE_WIDEVINE_CDM, so each profile gets the cached
CDM in `<udd>/WidevineCdm/<version>/` before it starts. Mirrors the launcher's
widevine.rs, crx.rs and the widevine functions of launch.rs.

The signature check is plain Python (RSA PKCS#1 v1.5 and ECDSA P-256/P-384
over SHA-256): `cryptography` is not a dependency of this SDK."""
from __future__ import annotations

import hashlib
import hmac
import io
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import threading
import time
import uuid
import warnings
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Callable, Optional
from urllib.parse import quote

if TYPE_CHECKING:
    from .proxy import ParsedProxy

# Widevine's component id: the SHA-256 of its signing key, a..p.
WIDEVINE_ID = "oimompecagnajdejgnnjijobebaeigek"
UPDATE_URL = "https://update.googleapis.com/service/update2/json"
# The package is about 20 MB; anything far larger is not it.
_MAX_PACKAGE = 64 << 20
_MAX_FILES = 64
# How often the cached CDM is checked against Google's, as Chrome checks its components.
WIDEVINE_RECHECK = 5 * 3600.0
# How long a start waits for a first CDM; it starts without one after that.
WIDEVINE_FIRST_WAIT = 45.0
_HINT_FILE = "latest-component-updated-widevine-cdm"
_VERSION_RE = re.compile(r"\d+(\.\d+){0,3}")


class WidevineError(RuntimeError):
    pass


# ---- the update check ----

def updater_platform(sysname: Optional[str] = None, machine: Optional[str] = None) -> tuple[str, str, str, str]:
    """This computer as Chrome's updater names it: @os, os.platform, arch, os.arch."""
    sysname = sysname or sys.platform
    arm = (machine or platform.machine()).lower() in ("arm64", "aarch64")
    if sysname == "darwin":
        return ("mac", "Mac OS X", "arm64" if arm else "x64", "arm64" if arm else "x86_64")
    if sysname == "win32":
        return ("win", "Windows", "arm64" if arm else "x64", "arm64" if arm else "x86_64")
    return ("linux", "Linux", "arm64" if arm else "x64", "aarch64" if arm else "x86_64")


def os_version() -> str:
    """The OS release Chrome sends: the server answers "noupdate" without one."""
    if sys.platform == "darwin":
        try:
            v = subprocess.run(["/usr/sbin/sysctl", "-n", "kern.osproductversion"], capture_output=True,
                               text=True, timeout=5).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            v = ""
        if not re.fullmatch(r"\d+(\.\d+)*", v):
            return "15.0"
        return f"{v}.0" if v.count(".") == 1 else v
    if sys.platform == "win32":
        import winreg

        def value(name, default):
            try:
                with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                                    0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY) as k:
                    return winreg.QueryValueEx(k, name)[0]
            except OSError:
                return default

        return (f"{value('CurrentMajorVersionNumber', 10)}.{value('CurrentMinorVersionNumber', 0)}."
                f"{value('CurrentBuildNumber', '19045')}.{value('UBR', 0)}")
    m = re.match(r"[\d.]+", platform.release())
    return m.group(0) if m else "6.0.0"


def request_body(chrome: str, have: Optional[str], background: bool) -> dict:
    """The update check, as Chrome 158's component updater sends it (protocol 4.0)."""
    os_name, plat, arch, os_arch = updater_platform()
    new_id = lambda: "{" + str(uuid.uuid4()) + "}"  # noqa: E731
    return {"request": {
        "@os": os_name,
        "@updater": "chrome",
        "acceptformat": "crx3,download,puff,run,xz,zucc",
        "apps": [{
            "appid": WIDEVINE_ID,
            "enabled": True,
            "installsource": "" if background else "ondemand",
            "ping": {"r": -2},
            "updatecheck": {},
            "version": have or "0.0.0.0",
        }],
        "arch": arch,
        "dedup": "cr",
        "ismachine": False,
        "os": {"arch": os_arch, "platform": plat, "version": os_version()},
        "prodchannel": "stable",
        "prodversion": chrome,
        "protocol": "4.0",
        "requestid": new_id(),
        "sessionid": new_id(),
        "updaterchannel": "stable",
        "updaterversion": chrome,
    }}


@dataclass(frozen=True)
class Offer:
    """What the server offered: the version, its https download urls, hash and size."""
    version: str
    urls: tuple[str, ...]
    sha256: str
    size: int


def parse_offer(text: str) -> Optional[Offer]:
    """None for "noupdate"; raises WidevineError for anything else that is not an offer."""
    body_text = text
    while body_text.startswith(")]}'"):
        body_text = body_text[4:]
    try:
        body = json.loads(body_text.strip())
    except ValueError:
        raise WidevineError("the update server's answer is not JSON") from None
    apps = ((body or {}).get("response") or {}).get("apps") if isinstance(body, dict) else None
    app = apps[0] if isinstance(apps, list) and apps and isinstance(apps[0], dict) else {}
    check = app.get("updatecheck") if isinstance(app.get("updatecheck"), dict) else {}
    status = check.get("status")
    if status == "noupdate":
        return None
    if status != "ok":
        why = status if isinstance(status, str) else app.get("status") if isinstance(app.get("status"), str) else "nothing"
        raise WidevineError(f"the update server answered {why}")
    version = check.get("nextversion")
    if not isinstance(version, str):
        raise WidevineError("no version in the offer")
    pipelines = check.get("pipelines")
    ops = pipelines[0].get("operations") if isinstance(pipelines, list) and pipelines and isinstance(pipelines[0], dict) else None
    if not isinstance(ops, list):
        raise WidevineError("no pipeline in the offer")
    dl = next((o for o in ops if isinstance(o, dict) and o.get("type") == "download"), None)
    if dl is None:
        raise WidevineError("no download in the offer")
    urls = tuple(u["url"] for u in (dl.get("urls") or []) if isinstance(u, dict)
                 and isinstance(u.get("url"), str) and u["url"].startswith("https://"))
    sha256 = (dl.get("out") or {}).get("sha256") if isinstance(dl.get("out"), dict) else None
    if not isinstance(sha256, str):
        raise WidevineError("no hash in the offer")
    size = dl.get("size")
    if not isinstance(size, int) or isinstance(size, bool) or size < 0:
        raise WidevineError("no size in the offer")
    if not urls:
        raise WidevineError("no https download in the offer")
    return Offer(version, urls, sha256, size)


# ---- CRX3, checked as Chrome's crx_verifier.cc checks it ----

_MAX_HEADER = 1 << 20
_SIGNATURE_CONTEXT = b"CRX3 SignedData\x00"
# Zip end-of-directory tokens: in a header they could make unzip see another archive.
_ZIP_TOKENS = (b"PK\x05\x06", b"PK\x06\x07", b"PK\x06\x06")


class CrxReject(Exception):
    """Why a package was refused; `code` names the case as crx.rs does."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class CrxVerified:
    crx_id: str
    developer_key: bytes
    archive_offset: int
    sha256: str


def id_from_hash(digest: bytes) -> str:
    """The first 16 bytes of a hash, one letter a..p per nibble."""
    return "".join(chr(97 + (b >> 4)) + chr(97 + (b & 15)) for b in digest[:16])


def id_from_key(spki: bytes) -> str:
    """The id a public key (SubjectPublicKeyInfo DER) gives."""
    return id_from_hash(hashlib.sha256(spki).digest())


def _fields(b: bytes) -> Optional[list[tuple[int, bytes]]]:
    """The length-delimited fields of a protobuf message, in order; None if malformed."""
    at = 0

    def varint():
        nonlocal at
        v = 0
        for shift in range(0, 64, 7):
            if at >= len(b):
                return None
            byte = b[at]
            at += 1
            v |= (byte & 0x7F) << shift
            if not byte & 0x80:
                return v
        return None

    out = []
    while at < len(b):
        tag = varint()
        if tag is None or tag >> 3 == 0:
            return None
        wire = tag & 7
        if wire == 0:
            if varint() is None:
                return None
            skip = 0
        elif wire == 1:
            skip = 8
        elif wire == 2:
            n = varint()
            if n is None or at + n > len(b):
                return None
            out.append((tag >> 3, b[at:at + n]))
            skip = n
        elif wire == 5:
            skip = 4
        else:
            return None
        if at + skip > len(b):
            return None
        at += skip
    return out


def _der(b: bytes) -> Optional[tuple[int, bytes, bytes]]:
    """One DER element: tag, contents, what follows. Strict lengths, as BoringSSL reads them."""
    if len(b) < 2:
        return None
    tag, first, at = b[0], b[1], 2
    if first < 0x80:
        n = first
    else:
        k = first & 0x7F
        if k == 0 or k > 4 or at >= len(b) or b[at] == 0 or at + k > len(b):
            return None
        n = int.from_bytes(b[at:at + k], "big")
        at += k
        if n < 0x80:
            return None
    if at + n > len(b):
        return None
    return tag, b[at:at + n], b[at + n:]


def _uint(content: bytes) -> Optional[int]:
    """A DER INTEGER that must be positive and minimally encoded."""
    if not content or content[0] & 0x80 or (len(content) > 1 and content[0] == 0 and not content[1] & 0x80):
        return None
    return int.from_bytes(content, "big")


_OID_RSA = bytes.fromhex("2a864886f70d010101")
_OID_EC = bytes.fromhex("2a8648ce3d0201")
_OID_P256 = bytes.fromhex("2a8648ce3d030107")
_OID_P384 = bytes.fromhex("2b81040022")


def _spki(key: bytes, rsa: bool) -> Optional[tuple[str, bytes]]:
    """A SubjectPublicKeyInfo's kind and raw key as a proof of this type needs it; None otherwise."""
    top = _der(key)
    if not top or top[0] != 0x30 or top[2]:
        return None
    alg = _der(top[1])
    if not alg or alg[0] != 0x30:
        return None
    bits = _der(alg[2])
    if not bits or bits[0] != 0x03 or bits[2] or not bits[1] or bits[1][0] != 0:
        return None
    oid = _der(alg[1])
    if not oid or oid[0] != 0x06:
        return None
    raw, params = bits[1][1:], oid[2]
    if rsa:
        return ("rsa", raw) if oid[1] == _OID_RSA and params == b"\x05\x00" else None
    if oid[1] != _OID_EC:
        return None
    curve = _der(params)
    if not curve or curve[0] != 0x06 or curve[2]:
        return None
    if curve[1] == _OID_P256:
        return "p256", raw
    if curve[1] == _OID_P384:
        return "p384", raw
    return None


# DigestInfo of SHA-256, as PKCS#1 v1.5 wraps the hash.
_SHA256_DIGEST_INFO = bytes.fromhex("3031300d060960864801650304020105000420")


def _rsa_verify(raw: bytes, sig: bytes, digest: bytes) -> bool:
    """RSASSA-PKCS1-v1_5 with SHA-256: the whole expected block is rebuilt and compared."""
    seq = _der(raw)
    if not seq or seq[0] != 0x30 or seq[2]:
        return False
    n_el = _der(seq[1])
    e_el = _der(n_el[2]) if n_el else None
    if not n_el or not e_el or n_el[0] != 0x02 or e_el[0] != 0x02 or e_el[2]:
        return False
    n, e = _uint(n_el[1]), _uint(e_el[1])
    if n is None or e is None or not 1024 <= n.bit_length() <= 8192 or e < 3 or e % 2 == 0:
        return False
    k = (n.bit_length() + 7) // 8
    if len(sig) != k:
        return False
    s = int.from_bytes(sig, "big")
    if s >= n:
        return False
    t = _SHA256_DIGEST_INFO + digest
    expected = b"\x00\x01" + b"\xff" * (k - len(t) - 3) + b"\x00" + t
    return hmac.compare_digest(pow(s, e, n).to_bytes(k, "big"), expected)


@dataclass(frozen=True)
class _Curve:
    p: int
    b: int
    n: int
    gx: int
    gy: int
    size: int


_CURVES = {
    "p256": _Curve(
        p=0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF,
        b=0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B,
        n=0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551,
        gx=0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296,
        gy=0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5,
        size=32),
    "p384": _Curve(
        p=int("fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff", 16),
        b=int("b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef", 16),
        n=int("ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973", 16),
        gx=int("aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7", 16),
        gy=int("3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f", 16),
        size=48),
}
_INF = (0, 1, 0)


def _double(pt, p):
    """Jacobian doubling on a curve with a = -3."""
    x1, y1, z1 = pt
    if z1 == 0 or y1 == 0:
        return _INF
    delta = z1 * z1 % p
    gamma = y1 * y1 % p
    beta = x1 * gamma % p
    alpha = 3 * (x1 - delta) * (x1 + delta) % p
    x3 = (alpha * alpha - 8 * beta) % p
    z3 = ((y1 + z1) ** 2 - gamma - delta) % p
    y3 = (alpha * (4 * beta - x3) - 8 * gamma * gamma) % p
    return x3, y3, z3


def _add(a, b, p):
    """Jacobian addition."""
    if a[2] == 0:
        return b
    if b[2] == 0:
        return a
    x1, y1, z1 = a
    x2, y2, z2 = b
    z1z1, z2z2 = z1 * z1 % p, z2 * z2 % p
    u1, u2 = x1 * z2z2 % p, x2 * z1z1 % p
    s1, s2 = y1 * z2 * z2z2 % p, y2 * z1 * z1z1 % p
    if u1 == u2:
        return _double(a, p) if s1 == s2 else _INF
    h = (u2 - u1) % p
    i = 4 * h * h % p
    j = h * i % p
    r = 2 * (s2 - s1) % p
    v = u1 * i % p
    x3 = (r * r - j - 2 * v) % p
    y3 = (r * (v - x3) - 2 * s1 * j) % p
    z3 = ((z1 + z2) ** 2 - z1z1 - z2z2) * h % p
    return x3, y3, z3


def _ecdsa_verify(kind: str, raw: bytes, sig: bytes, digest: bytes) -> bool:
    c = _CURVES[kind]
    if len(raw) != 1 + 2 * c.size or raw[0] != 4:
        return False
    qx, qy = int.from_bytes(raw[1:1 + c.size], "big"), int.from_bytes(raw[1 + c.size:], "big")
    if qx >= c.p or qy >= c.p or (qy * qy - (qx ** 3 - 3 * qx + c.b)) % c.p:
        return False
    seq = _der(sig)
    if not seq or seq[0] != 0x30 or seq[2]:
        return False
    r_el = _der(seq[1])
    s_el = _der(r_el[2]) if r_el else None
    if not r_el or not s_el or r_el[0] != 0x02 or s_el[0] != 0x02 or s_el[2]:
        return False
    r, s = _uint(r_el[1]), _uint(s_el[1])
    if r is None or s is None or not (1 <= r < c.n and 1 <= s < c.n):
        return False
    e = int.from_bytes(digest, "big")
    if len(digest) * 8 > c.n.bit_length():
        e >>= len(digest) * 8 - c.n.bit_length()
    w = pow(s, -1, c.n)
    u1, u2 = e * w % c.n, r * w % c.n
    g, q = (c.gx, c.gy, 1), (qx, qy, 1)
    both = _add(g, q, c.p)
    acc = _INF
    for bit in range(max(u1.bit_length(), u2.bit_length()) - 1, -1, -1):
        acc = _double(acc, c.p)
        pick = ((u1 >> bit) & 1, (u2 >> bit) & 1)
        if pick == (1, 1):
            acc = _add(acc, both, c.p)
        elif pick == (1, 0):
            acc = _add(acc, g, c.p)
        elif pick == (0, 1):
            acc = _add(acc, q, c.p)
    if acc[2] == 0:
        return False
    zinv = pow(acc[2], -1, c.p)
    return acc[0] * zinv * zinv % c.p % c.n == r


def verify_crx(data: bytes, *, id: Optional[str] = None, sha256: Optional[str] = None,
               size: Optional[int] = None) -> CrxVerified:
    """Every proof must verify and one must be the developer's key for the signed id."""
    digest_hex = hashlib.sha256(data).hexdigest()
    if size is not None and size != len(data):
        raise CrxReject("WrongSize", f"the download is {len(data)} bytes, the server said {size}")
    if sha256 is not None and sha256.lower() != digest_hex:
        raise CrxReject("WrongHash", "the download does not match the server's hash")
    if len(data) < 12 or data[:4] != b"Cr24" or int.from_bytes(data[4:8], "little") != 3:
        raise CrxReject("NotCrx3", "not a CRX3 package")
    header_size = int.from_bytes(data[8:12], "little")
    if header_size > _MAX_HEADER or len(data) - 12 < header_size:
        raise CrxReject("HeaderSize", "the CRX header's size is wrong")
    header = data[12:12 + header_size]
    archive_offset = 12 + header_size
    if any(t in header for t in _ZIP_TOKENS):
        raise CrxReject("ZipTokenInHeader", "the CRX header holds a zip marker")
    bad = CrxReject("BadHeader", "the CRX header cannot be read")

    # CrxFileHeader: proofs 2 (RSA) and 3 (ECDSA), signed_header_data 10000.
    top = _fields(header)
    if top is None:
        raise bad
    proofs, signed = [], b""
    for f, d in top:
        if f in (2, 3):
            sub = _fields(d)
            if sub is None:
                raise bad
            key = sig = b""
            for g, v in sub:
                if g == 1:
                    key = v
                elif g == 2:
                    sig = v
            proofs.append((f == 2, key, sig))
        elif f == 10000:
            signed = d
    sd = _fields(signed)
    if sd is None:
        raise bad
    crx_id = None
    for f, d in sd:
        if f == 1:
            crx_id = d
    # Chrome compares 32 letters; any other length matches no key.
    declared = id_from_hash(crx_id) if crx_id is not None and len(crx_id) == 16 else None

    developer_key, checks = None, []
    for rsa, key, sig in proofs:
        if declared is not None and declared == id_from_key(key):
            developer_key = key
        kind = _spki(key, rsa)
        if kind is None:
            raise CrxReject("BadKey", "a key in the CRX cannot be read")
        checks.append((kind, sig))
    if declared is None or developer_key is None:
        raise CrxReject("NoDeveloperProof", "the CRX is not signed by its developer's key")
    # Every proof covers context, le32(len), signed_header_data and the archive.
    h = hashlib.sha256(_SIGNATURE_CONTEXT)
    h.update(len(signed).to_bytes(4, "little"))
    h.update(signed)
    h.update(memoryview(data)[archive_offset:])
    digest = h.digest()
    for (kind, raw), sig in checks:
        ok = _rsa_verify(raw, sig, digest) if kind == "rsa" else _ecdsa_verify(kind, raw, sig, digest)
        if not ok:
            raise CrxReject("BadSignature", "the CRX signature does not match its contents")
    if id is not None and id != declared:
        raise CrxReject("WrongId", f"the package is {declared}, not {id}")
    return CrxVerified(declared, developer_key, archive_offset, digest_hex)


# ---- download and install into the cache ----

def _proxy_url(p: "ParsedProxy") -> str:
    """httpx's spelling of the proxy; `socks5h` sends DNS through it."""
    scheme = "socks5h" if p.scheme == "socks5" else p.scheme
    if p.username or p.password:
        return f"{scheme}://{quote(p.username or '', safe='')}:{quote(p.password or '', safe='')}@{p.host}:{p.port}"
    return f"{scheme}://{p.host}:{p.port}"


def _client(proxy: Optional["ParsedProxy"], chrome: str):
    import httpx

    major = chrome.split(".")[0] or "158"
    kw: dict = {"timeout": httpx.Timeout(180.0, connect=15.0), "follow_redirects": True,
                "headers": {"User-Agent": f"Mozilla/5.0 Chrome/{major}.0.0.0"}}
    if proxy is not None and proxy.host:
        kw["proxy"] = _proxy_url(proxy)
    else:
        kw["trust_env"] = False
    return httpx.Client(**kw)


def fetch_widevine(proxy: Optional["ParsedProxy"], chrome: str, have: Optional[str], cache: Path,
                   background: bool, cancel: Optional[threading.Event] = None) -> Optional[str]:
    """Fetches the current CDM into `cache` when the server has a newer one than
    `have`. Returns the version put there, or None when `have` is current."""
    cache = Path(cache)
    with _client(proxy, chrome) as c:
        try:
            r = c.post(UPDATE_URL, content=json.dumps(request_body(chrome, have, background), separators=(",", ":")),
                       headers={"Content-Type": "application/json", "X-Goog-Update-AppId": WIDEVINE_ID,
                                "X-Goog-Update-Interactivity": "bg" if background else "fg",
                                "X-Goog-Update-Updater": f"chrome-{chrome}"})
        except Exception as e:
            raise WidevineError(f"the update server could not be reached: {e}") from e
        if not 200 <= r.status_code < 300:
            raise WidevineError(f"the update server answered HTTP {r.status_code}")
        offer = parse_offer(r.text)
        if offer is None:
            return None
        if offer.size > _MAX_PACKAGE:
            raise WidevineError(f"the offered package is {offer.size >> 20} MB, more than a CDM")
        last: Optional[Exception] = None
        for url in offer.urls:
            try:
                data = _download(c, url, offer.size, cancel)
                try:
                    v = verify_crx(data, id=WIDEVINE_ID, sha256=offer.sha256, size=offer.size)
                except CrxReject as e:
                    raise WidevineError(f"the package failed its check: {e}") from e
            except Exception as e:
                last = e
                if cancel is not None and cancel.is_set():
                    break
                continue
            install_package(data, v.archive_offset, offer.version, cache)
            return offer.version
    raise last or WidevineError("no download worked")


def _download(c, url: str, size: int, cancel: Optional[threading.Event]) -> bytes:
    buf = bytearray()
    with c.stream("GET", url) as r:
        if not 200 <= r.status_code < 300:
            raise WidevineError(f"HTTP {r.status_code} from {url}")
        for chunk in r.iter_bytes():
            if cancel is not None and cancel.is_set():
                raise WidevineError("cancelled")
            buf += chunk
            if len(buf) > size:
                raise WidevineError(f"the download from {url} is larger than the offer said")
    return bytes(buf)


def install_package(data: bytes, archive_offset: int, version: str, cache: Path) -> None:
    """Unpacks a verified package beside `cache`, checks its manifest names the
    offered version, then puts it in the cache's place."""
    cache = Path(cache)
    staging = cache.with_name(f"{cache.name}.incoming-{os.getpid()}")
    shutil.rmtree(staging, ignore_errors=True)
    try:
        _unzip_within(data[archive_offset:], staging, _MAX_PACKAGE * 2, _MAX_FILES)
        if cdm_version_of(staging) != version:
            raise WidevineError("the package's manifest names another version than the offer"
                                if (staging / "manifest.json").exists() else "no manifest.json in the package")
        shutil.rmtree(cache, ignore_errors=True)
        os.replace(staging, cache)
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def _enclosed(name: str) -> Optional[str]:
    """A relative path inside the archive, or None for one that would leave it."""
    if "\0" in name:
        return None
    s = name.replace("\\", "/")
    if s.startswith("/") or re.match(r"[A-Za-z]:", s):
        return None
    parts = [p for p in s.split("/") if p not in ("", ".")]
    if not parts or ".." in parts:
        return None
    return os.path.join(*parts)


def _unzip_within(archive: bytes, dst: Path, max_bytes: int, most: int) -> None:
    try:
        zf = zipfile.ZipFile(io.BytesIO(archive))
    except zipfile.BadZipFile:
        raise WidevineError("not a zip/crx archive") from None
    with zf:
        infos = zf.infolist()
        if len(infos) > most:
            raise WidevineError(f"the archive holds more than {most} files")
        too_big = WidevineError(f"the package unpacks to more than {max_bytes >> 20} MB")
        if sum(i.file_size for i in infos) > max_bytes:
            raise too_big
        dst.mkdir(parents=True, exist_ok=True)
        total = 0
        for info in infos:
            rel = _enclosed(info.filename)
            if rel is None:
                continue
            out = dst / rel
            if info.is_dir():
                out.mkdir(parents=True, exist_ok=True)
                continue
            out.parent.mkdir(parents=True, exist_ok=True)
            with zf.open(info) as src, open(out, "wb") as f:
                while True:
                    chunk = src.read(1 << 20)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > max_bytes:
                        raise too_big
                    f.write(chunk)


# ---- the cache and each profile ----

def compare_cdm_versions(a: str, b: str) -> int:
    """Version strings as numbers, so "4.10.2891.0" sorts above "4.9.9999.0"."""
    pa = [int(p) if p.isdigit() else 0 for p in a.split(".")]
    pb = [int(p) if p.isdigit() else 0 for p in b.split(".")]
    return (pa > pb) - (pa < pb)


def cdm_version_of(d: Path) -> Optional[str]:
    try:
        v = json.loads((Path(d) / "manifest.json").read_text(encoding="utf-8")).get("version")
    except (OSError, ValueError, AttributeError):
        return None
    return v if isinstance(v, str) else None


def _checked_lately(stamp: Path) -> bool:
    try:
        age = time.time() - stamp.stat().st_mtime
    except OSError:
        return False
    # Some disks round times up: a little ahead is now.
    return -2.0 < age < WIDEVINE_RECHECK


Fetcher = Callable[..., Optional[str]]
_ONE_AT_A_TIME = threading.Lock()


def ensure_widevine(cache: Path, chrome: str, proxy: Optional["ParsedProxy"], *,
                    fetcher: Optional[Fetcher] = None, first_wait: float = WIDEVINE_FIRST_WAIT) -> None:
    """A first CDM is waited for (up to 45 s); a cached one is checked for a newer
    build in the background, every 5 hours. `cache` is `<cache_dir>/widevine-cdm`."""
    cache = Path(cache)
    fetch = fetcher or fetch_widevine
    stamp = cache.with_name(f"{cache.name}.checked")
    if cdm_version_of(cache) is not None and _checked_lately(stamp):
        return
    first = cdm_version_of(cache) is None
    cancel = threading.Event()

    def check() -> None:
        # One check at a time, however many profiles start together.
        with _ONE_AT_A_TIME:
            have = cdm_version_of(cache)
            if (have is not None and _checked_lately(stamp)) or cancel.is_set():
                return
            try:
                got = fetch(proxy, chrome, have, cache, have is not None, cancel)
                if got:
                    print(f"[shardx] Widevine CDM {got} from Google", flush=True)
                stamp.write_bytes(b"")
            except Exception as e:
                if not cancel.is_set():
                    warnings.warn(f"[shardx] Widevine check failed: {e}", RuntimeWarning, stacklevel=2)

    t = threading.Thread(target=check, name="shardx-widevine", daemon=True)
    t.start()
    if not first:
        return
    t.join(first_wait)
    if t.is_alive():
        cancel.set()
        warnings.warn("[shardx] no Widevine CDM yet: Google did not answer in time; this profile starts without one",
                      RuntimeWarning, stacklevel=2)


def harvest_widevine(cache: Path, udd: Path) -> Optional[str]:
    """Take a CDM the engine fetched for itself into the cache when it is newer,
    so the next profile starts with it. Returns the version taken, if any."""
    cache, root = Path(cache), Path(udd) / "WidevineCdm"
    if not root.is_dir():
        return None
    newest = None
    for p in root.iterdir():
        if not p.is_dir():
            continue
        # The engine names each folder by its version; anything else is not one of its.
        v = cdm_version_of(p)
        if v is None or v != p.name or not _VERSION_RE.fullmatch(v):
            continue
        if newest is None or compare_cdm_versions(v, newest[0]) > 0:
            newest = (v, p)
    if newest is None:
        return None
    have = cdm_version_of(cache)
    if have is not None and compare_cdm_versions(have, newest[0]) >= 0:
        return None
    # Into a sibling first: a half-copied cache is worse than none.
    staging = cache.with_name(f"{cache.name}.incoming-{os.getpid()}")
    shutil.rmtree(staging, ignore_errors=True)
    try:
        shutil.copytree(newest[1], staging, symlinks=False)
        shutil.rmtree(cache, ignore_errors=True)
        os.replace(staging, cache)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    print(f"[shardx] Widevine CDM {newest[0]} cached from a profile", flush=True)
    return newest[0]


def install_widevine(cache: Path, udd: Path) -> Optional[Path]:
    """Copies the cached CDM into `<udd>/WidevineCdm/<version>/`, the layout
    Chromium's component installer reads, and writes the Linux hint. Returns the
    version folder, or None while nothing is cached."""
    cache = Path(cache)
    if not cache.exists():
        return None
    version = cdm_version_of(cache)
    if version is None or not _VERSION_RE.fullmatch(version):
        raise WidevineError(f"the cached CDM at {cache} names no usable version")
    root = Path(udd) / "WidevineCdm"
    versioned = root / version
    if not versioned.exists():
        # A flat layout an older SDK or launcher left goes.
        if (root / "manifest.json").exists():
            for stray in ("manifest.json", "LICENSE", "_platform_specific"):
                p = root / stray
                if p.is_dir():
                    shutil.rmtree(p, ignore_errors=True)
                elif p.exists():
                    p.unlink()
        staging = root / f".{version}.incoming"
        shutil.rmtree(staging, ignore_errors=True)
        try:
            shutil.copytree(cache, staging, symlinks=False)
            os.replace(staging, versioned)
        finally:
            shutil.rmtree(staging, ignore_errors=True)
    write_widevine_hint(root, versioned)
    return versioned


def write_widevine_hint(root: Path, versioned: Path) -> None:
    """The hint Chrome on Linux loads its component CDM from at start, before the
    sandbox closes; harmless on other platforms."""
    hint = Path(root) / _HINT_FILE
    body = json.dumps({"Path": os.path.abspath(versioned)}, separators=(",", ":"), ensure_ascii=False)
    try:
        if hint.read_text(encoding="utf-8") == body:
            return
    except OSError:
        pass
    hint.write_text(body, encoding="utf-8")
