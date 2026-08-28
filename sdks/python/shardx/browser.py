"""Browser launch + lifecycle. Spawns the ShardX engine with the same
spoofing flags the desktop launcher uses, plus pre-launch:

  • resolve_auto_fields  — fill timezone/language/geolocation from a
    live geo lookup through the bound proxy.
  • apply_screen_strategy — cap to host monitor (macOS) or replace with
    the host monitor (Win/Linux), matching the launcher's
    `clamp_screen_to_real_display` / `--shardx-real-screen` switch.
  • probe_udp            — decide QUIC + WebRTC policy from a live SOCKS5
    UDP_ASSOCIATE probe.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from .auto_resolve import has_auto_fields, resolve_auto_fields
from .geo import GeoInfo, geo_check_via
from .profile import Profile, user_data_dir as _user_data_dir
from .proxy import ParsedProxy, parse_proxy, probe_udp
from .runtime import Runtime, apply_engine_version
from .screen import apply_screen_strategy, default_mode_for

_NOISE_DEFAULT = {
    "canvas":       {"enabled": False, "seed": 0},
    "webgl":        {"enabled": False, "seed": 0, "intensity": 0},
    "audio":        {"enabled": False, "seed": 0},
    "client_rects": {"enabled": False, "seed": 0, "max_offset": 0},
    "sensors":      {"enabled": False, "seed": 0},
    "fonts":        {"enabled": False, "seed": 0},
}


def _noise_seed(profile_id: str, slot: str) -> int:
    """Deterministic non-zero 32-bit FNV-1a of `<id>::<slot>`."""
    h = 2166136261
    for b in f"{profile_id}::{slot}".encode():
        h = ((h ^ b) * 16777619) & 0xFFFFFFFF
    return h or 1


def apply_noise_seeds(config: dict, profile_id: str) -> None:
    """Add the default noise block when absent, then fill any seed-0 vector
    with a stable per-profile value — without it every profile would share
    seed 0 and produce an identical canvas/audio/WebGL fingerprint."""
    noise = config.get("noise")
    if not isinstance(noise, dict):
        noise = {k: dict(v) for k, v in _NOISE_DEFAULT.items()}
        config["noise"] = noise
    for slot, block in noise.items():
        if isinstance(block, dict) and not block.get("seed"):
            block["seed"] = _noise_seed(profile_id, slot)


@dataclass
class BrowserSession:
    pid: int
    user_data_dir: Path
    cdp_url: Optional[str]
    process: subprocess.Popen = field(repr=False)
    proxy_udp_ms: Optional[float] = None
    quic_enabled: bool = False
    webrtc_mode: str = "auto"
    geo: Optional[GeoInfo] = None
    _stopped: bool = field(default=False, repr=False)

    def stop(self, timeout: float = 5.0) -> None:
        if self._stopped:
            return
        self._stopped = True
        try:
            if sys.platform == "win32":
                self.process.terminate()
            else:
                self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2.0)
        except ProcessLookupError:
            pass

    def __enter__(self) -> "BrowserSession":
        return self

    def __exit__(self, *exc) -> None:
        self.stop()


class Browser:
    """Resolves the engine binary + assembles the launch command line."""

    def __init__(self, runtime: Runtime):
        self.runtime = runtime

    def launch(
        self,
        profile: Profile,
        *,
        proxy: Optional[str] = None,
        cdp: bool = False,
        headless: bool = False,
        extra_args: Optional[list[str]] = None,
        env: Optional[dict[str, str]] = None,
        webrtc: str = "auto",                  # "auto" | "block" | "tcp_only"
        webrtc_public_ip: Optional[str] = None,
        quic: Optional[bool] = None,           # None = auto-decide from UDP probe
        screen_mode: Optional[str] = None,     # "profile" | "cap_to_host" | "use_host"
        probe_timeout: float = 6.0,
        user_data_dir: Optional[str | Path] = None,
    ) -> BrowserSession:
        # Auto-install on first use (high-level ShardX.launch already does
        # this; the call is here too so low-level Browser.launch users
        # don't have to remember).
        self.runtime.install()

        parsed: Optional[ParsedProxy] = parse_proxy(proxy) if proxy else None

        # ---- pre-launch: auto-resolve, screen strategy, UDP probe ------
        geo: Optional[GeoInfo] = None
        if has_auto_fields(profile.config):
            geo = resolve_auto_fields(profile.config, parsed)

        mode = screen_mode or default_mode_for(profile.platform)
        apply_screen_strategy(profile.config, mode)

        proxy_udp_ms: Optional[float] = None
        if parsed and parsed.is_socks5:
            proxy_udp_ms = probe_udp(parsed, timeout=probe_timeout)
        udp_ok = proxy_udp_ms is not None
        quic_enabled = quic if quic is not None else (parsed is not None and udp_ok)
        webrtc_mode = webrtc
        if webrtc_mode == "auto" and parsed is not None and not udp_ok:
            webrtc_mode = "tcp_only"

        # ---- profile + udd ---------------------------------------------
        udd_base = Path(user_data_dir).resolve() if user_data_dir else None
        udd = _user_data_dir(self.runtime, profile.id, base=udd_base)
        print(f"[shardx] profile '{profile.id}' → {udd}", flush=True)
        # Keep the spoofed Chrome version coherent with the installed engine,
        # regardless of where the profile config came from (library / file / dict).
        apply_engine_version(
            profile.config,
            self.runtime.chromium_version,
            self.runtime.grease_brand,
            self.runtime.grease_version,
        )
        apply_noise_seeds(profile.config, profile.id)
        fp_file = udd / "fingerprint.json"
        fp_file.write_text(json.dumps(profile.config))

        argv: list[str] = [
            str(self.runtime.binary_path),
            f"--fingerprint-profile={fp_file}",
            f"--user-data-dir={udd}",
            "--no-first-run",
        ]
        if not profile.has_webgpu:
            argv.append("--disable-features=WebGPU")
        if not headless and not cdp:
            argv += ["--restore-last-session", "--hide-crash-restore-bubble"]
        # Engine-side real-screen switch only fires on use_host (where the
        # SDK already rewrote screen.* — keep them in sync with the launcher).
        if mode == "use_host":
            argv.append("--shardx-real-screen")
        if parsed is not None:
            argv.append(f"--proxy-server={parsed.to_arg()}")
            argv.append("--enable-quic" if quic_enabled else "--disable-quic")
        if webrtc_mode == "block":
            argv += ["--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                     "--shardx-webrtc-policy=block"]
        elif webrtc_mode == "tcp_only":
            argv += ["--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                     "--shardx-webrtc-policy=tcp_only"]
            # Engine spoofs the public side of ICE candidates with this IP.
            # Match the launcher: ALWAYS resolve when proxy is bound — relying
            # on `geo` from auto-resolve only works when the profile has auto
            # sentinels, otherwise the engine falls back to the host IP.
            ip = webrtc_public_ip or (geo.ip if geo else None)
            if ip is None and parsed is not None:
                try:
                    ip = geo_check_via(parsed).ip or None
                except Exception:
                    ip = None
            if ip:
                argv.append(f"--shardx-webrtc-public-ip={ip}")
        if cdp:
            (udd / "DevToolsActivePort").unlink(missing_ok=True)
            argv += ["--remote-debugging-port=0", "--remote-allow-origins=*"]
        if headless:
            argv.append("--headless=new")
        if extra_args:
            argv += list(extra_args)

        proc_env = os.environ.copy()
        if env:
            proc_env.update(env)
        proc = subprocess.Popen(
            argv,
            env=proc_env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=(sys.platform != "win32"),
        )

        cdp_url = _read_cdp_endpoint(udd, timeout=15.0) if cdp else None

        return BrowserSession(
            pid=proc.pid,
            user_data_dir=udd,
            cdp_url=cdp_url,
            process=proc,
            proxy_udp_ms=proxy_udp_ms,
            quic_enabled=quic_enabled,
            webrtc_mode=webrtc_mode,
            geo=geo,
        )


def _read_cdp_endpoint(udd: Path, timeout: float) -> Optional[str]:
    deadline = time.monotonic() + timeout
    marker = udd / "DevToolsActivePort"
    while time.monotonic() < deadline:
        if marker.exists():
            try:
                port = int(marker.read_text().splitlines()[0].strip())
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=2.0) as r:
                    data = json.loads(r.read())
                    return data.get("webSocketDebuggerUrl")
            except Exception:
                pass
        time.sleep(0.1)
    return None
