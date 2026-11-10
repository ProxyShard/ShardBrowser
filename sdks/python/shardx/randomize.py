"""Per-launch randomisation of hardware_concurrency / device_memory /
platform_version. Mirrors `randomize_hardware` and
`randomize_platform_version` in `src-tauri/src/lib.rs`.

Mac profiles use the curated MAC_HW_CONFIGS table (matched to the
profile id). Windows / Linux profiles use the launcher's host-bracketed
logic: real x86 logical-core counts filtered to the [host_cores-4,
host_cores+2] window, with device_memory floored by core count and
ceilinged by `host_ram_bucket_gb`.
"""
from __future__ import annotations

import random
import re
from typing import Optional

from .host import host_logical_cores, host_ram_bucket_gb

# Chrome writes major.minor.patch, never two parts. Current releases as of
# 2026-11 (support.apple.com/100100); Sonoma ends at 14.8.9.
MACOS_PLATFORM_VERSIONS = [
    "14.8.8", "14.8.9",
    "15.7.9", "15.8.1", "15.8.2", "15.8.2",
    "26.6.1", "26.6.2", "26.7.1", "26.7.2", "26.7.2", "26.7.2",
    "27.0.1", "27.2.0", "27.2.0", "27.2.0",
]

# Chrome reports the UniversalApiContract: 10 on Win10 2004+, 15 on Win11
# 22H2/23H2, 19 on 24H2/25H2. 13 and 16-18 exist only on Insider builds.
WINDOWS_PLATFORM_VERSIONS = [
    "10.0.0", "10.0.0", "10.0.0", "10.0.0", "10.0.0",
    "15.0.0",
    "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0",
]

# Chrome on Linux reports an empty version, and the engine since build 4 says
# so itself. A value is still written so an older engine does not pass on the
# host's own.
LINUX_PLATFORM_VERSIONS = [
    "5.15.0", "6.1.0", "6.5.0",
    "6.6.0", "6.8.0", "6.10.0", "6.11.0", "6.12.0",
    "6.14.0", "6.15.0", "6.16.0",
]

# What a version that cannot stand becomes on a Mac, and on an M4 claiming 14.x.
_MACOS_SETTLED = "26.7.2"
_MACOS_SETTLED_M4 = "15.8.2"

# (cores, mem_GiB) combos per Apple model id — keep in sync with lib.rs.
MAC_HW_CONFIGS: dict[str, list[tuple[int, int]]] = {
    "mac-m1-air13":     [(8, 8), (8, 16)],
    "mac-m1-mbp13":     [(8, 8), (8, 16)],
    "mac-m1-imac24":    [(8, 8), (8, 16)],
    "mac-m1-pro-mbp14": [(8, 16), (10, 16), (10, 32)],
    "mac-m1-pro-mbp16": [(8, 16), (10, 16), (10, 32)],
    "mac-m1-max-mbp14": [(10, 32)],
    "mac-m1-max-mbp16": [(10, 32)],
    "mac-m2-air13":     [(8, 8), (8, 16)],
    "mac-m2-air15":     [(8, 8), (8, 16)],
    "mac-m2-mbp13":     [(8, 8), (8, 16)],
    "mac-m2-pro-mbp14": [(10, 16), (12, 16), (12, 32)],
    "mac-m2-pro-mbp16": [(10, 16), (12, 16), (12, 32)],
    "mac-m2-max-mbp14": [(12, 32)],
    "mac-m2-max-mbp16": [(12, 32)],
    "mac-m3-air13":     [(8, 8), (8, 16)],
    "mac-m3-air15":     [(8, 8), (8, 16)],
    "mac-m3-mbp14":     [(8, 8), (8, 16)],
    "mac-m3-imac24":    [(8, 8), (8, 16)],
    "mac-m3-pro-mbp14": [(11, 16), (12, 16), (12, 32)],
    "mac-m3-pro-mbp16": [(11, 16), (12, 16), (12, 32)],
    "mac-m3-max-mbp14": [(14, 32), (16, 32)],
    "mac-m3-max-mbp16": [(14, 32), (16, 32)],
    "mac-m4-air13":     [(10, 16), (10, 32)],
    "mac-m4-air15":     [(10, 16), (10, 32)],
    "mac-m4-mbp14":     [(10, 16), (10, 32)],
    "mac-m4-imac24":    [(10, 16), (10, 32)],
    "mac-m4-pro-mbp14": [(12, 16), (14, 16), (14, 32)],
    "mac-m4-pro-mbp16": [(12, 16), (14, 16), (14, 32)],
    "mac-m4-max-mbp14": [(14, 32), (16, 32)],
    "mac-m4-max-mbp16": [(14, 32), (16, 32)],
    "mac-m5-mbp14":     [(10, 16), (10, 32)],
}

# Real x86 logical-core counts (SMT + Intel hybrid) — same array as lib.rs.
X86_CORES: list[int] = [4, 6, 8, 12, 16, 20, 24, 28, 32]


def _mac_first_major(config: dict) -> int:
    """The first macOS major the claimed Apple chip shipped with: M4 came with
    15, M5 with 26; older chips run everything in the pool."""
    webgl = config.get("webgl")
    renderer = webgl.get("renderer") if isinstance(webgl, dict) else None
    if not isinstance(renderer, str) or "Apple M" not in renderer:
        return 0
    digits = re.match(r"\d+", renderer[renderer.index("Apple M") + 7:], re.ASCII)
    gen = int(digits.group()) if digits else 0
    return 26 if gen >= 5 else 15 if gen == 4 else 0


def _major(version: str) -> Optional[int]:
    head = version.split(".")[0]
    return int(head) if re.fullmatch(r"\d+", head, re.ASCII) else None


def randomize_platform_version(config: dict) -> None:
    """In-place: pick a fresh `navigator.platform_version` (+ mirror to client_hints).
    A Mac gets only versions its claimed chip can run."""
    platform = ((config.get("navigator") or {}).get("platform") or "")
    pool = {
        "macOS":   MACOS_PLATFORM_VERSIONS,
        "Windows": WINDOWS_PLATFORM_VERSIONS,
        "Linux":   LINUX_PLATFORM_VERSIONS,
    }.get(platform)
    if platform == "macOS":
        first = _mac_first_major(config)
        pool = [v for v in MACOS_PLATFORM_VERSIONS if (_major(v) or 0) >= first]
    if not pool:
        return
    v = random.choice(pool)
    nav = config.setdefault("navigator", {})
    nav["platform_version"] = v
    ch = config.get("client_hints")
    if isinstance(ch, dict):
        ch["platform_version"] = v


def _settled_windows(version: str) -> str:
    if version in ("10.0.0", "14.0.0", "15.0.0", "19.0.0"):
        return version
    # The engine counts an unreadable version as Windows 11, and picks its fonts by that.
    major = _major(version)
    return "10.0.0" if major is not None and major < 13 else "19.0.0"


def _settled_macos(version: str, first_major: int) -> str:
    parts = version.split(".")
    if not all(re.fullmatch(r"\d+", p, re.ASCII) for p in parts):
        return _MACOS_SETTLED
    nums = [int(p) for p in parts]
    if not 1 <= len(nums) <= 3 or nums[0] < 12:
        return _MACOS_SETTLED
    if nums[0] < first_major:
        return _MACOS_SETTLED if first_major >= 26 else _MACOS_SETTLED_M4
    nums += [0] * (3 - len(nums))
    return ".".join(str(n) for n in nums)


def settle_platform_version(config: dict) -> bool:
    """In-place: a Windows or macOS `platform_version` Chrome could not report
    (Windows 13/16/17, a two-part macOS version, an M4 on 14.x) becomes the
    nearest one it can, in navigator and client_hints alike. Deterministic, so
    launch applies it every time. Linux is left to the engine. Returns whether
    anything changed. SDK equivalent of the launcher's function of the same name."""
    nav = config.get("navigator") if isinstance(config.get("navigator"), dict) else {}
    ch = config.get("client_hints") if isinstance(config.get("client_hints"), dict) else {}
    # The engine reads client_hints over navigator.
    current = next((v for v in (ch.get("platform_version"), nav.get("platform_version"))
                    if isinstance(v, str) and v), "")
    platform = nav.get("platform")
    if platform == "Windows":
        want = _settled_windows(current)
    elif platform == "macOS":
        want = _settled_macos(current, _mac_first_major(config))
    else:
        return False
    changed = False
    for key in ("navigator", "client_hints"):
        obj = config.get(key)
        if isinstance(obj, dict) and obj.get("platform_version") != want:
            obj["platform_version"] = want
            changed = True
    return changed


def randomize_hardware(config: dict, profile_id: Optional[str] = None) -> None:
    """In-place: pick a fresh (hardware_concurrency, device_memory) pair.

    macOS: curated MAC_HW_CONFIGS table by profile id.
    Windows / Linux: bracket the host CPU count within [C-4, C+2] from the
    X86_CORES set; floor device_memory by core count (>=12 → 16, else 8) and
    cap by `host_ram_bucket_gb()`.
    """
    platform = ((config.get("navigator") or {}).get("platform") or "")

    if platform == "macOS" and profile_id and profile_id in MAC_HW_CONFIGS:
        cores, mem = random.choice(MAC_HW_CONFIGS[profile_id])
    elif platform in ("Windows", "Linux"):
        c = host_logical_cores()
        lo = max(0, c - 4)
        hi = c + 2
        candidates = [n for n in X86_CORES if lo <= n <= hi]
        if candidates:
            cores = random.choice(candidates)
        else:
            cores = min(X86_CORES, key=lambda n: abs(n - c))
        real = host_ram_bucket_gb()
        floor = 16 if cores >= 12 else 8
        mem_cand = [m for m in (8, 16, 32) if floor <= m <= real]
        mem = random.choice(mem_cand) if mem_cand else real
    else:
        return

    nav = config.setdefault("navigator", {})
    nav["hardware_concurrency"] = cores
    nav["device_memory"] = mem
