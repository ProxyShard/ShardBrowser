"""Screen strategies — three modes that match what the launcher does
in `clamp_screen_to_real_display` (`src-tauri/src/lib.rs`):

* `"profile"`   — keep whatever the fingerprint claims.
* `"cap_to_host"` — macOS default. Scale `screen.*` and `window.*` down
  if the host monitor is smaller than the FP claim; no-op otherwise.
* `"use_host"`  — Win/Linux default. Overwrite `screen.*` with the host
  display, subtract a small taskbar inset for avail_height, write
  matching window outer/inner sizes.

All modes leave DPR / color_depth / orientation / etc. untouched.
"""
from __future__ import annotations

from typing import Any, Optional, Tuple

from .host import host_screen_size


def apply_screen_strategy(config: dict, mode: str) -> None:
    """Mutate `config` in place per `mode`. Unknown modes are no-ops."""
    if mode == "profile":
        return
    host = host_screen_size()
    if host is None:
        # No host info — every mode degrades to no-op rather than risk a wrong write.
        return
    host_w, host_h = host

    if mode == "cap_to_host":
        _cap_to_host(config, host_w, host_h)
    elif mode == "use_host":
        _use_host(config, host_w, host_h)


def _scr(config: dict) -> dict:
    s = config.get("screen")
    if not isinstance(s, dict):
        s = {}
        config["screen"] = s
    return s


def _win(config: dict) -> dict:
    w = config.get("window")
    if not isinstance(w, dict):
        w = {}
        config["window"] = w
    return w


def _cap_to_host(config: dict, host_w: int, host_h: int) -> None:
    scr = config.get("screen")
    if not isinstance(scr, dict):
        return
    fp_w = int(scr.get("width") or 0)
    fp_h = int(scr.get("height") or 0)
    if fp_w <= 0 or fp_h <= 0:
        return
    if host_w >= fp_w and host_h >= fp_h:
        return  # host >= FP → keep curated FP screen (mac default)

    # Scale proportionally; keep aspect-ratio.
    ratio = min(host_w / fp_w, host_h / fp_h)
    new_w = max(1, int(round(fp_w * ratio)))
    new_h = max(1, int(round(fp_h * ratio)))

    fp_aw = int(scr.get("avail_width") or fp_w)
    fp_ah = int(scr.get("avail_height") or fp_h)
    new_aw = max(1, int(round(fp_aw * ratio)))
    new_ah = max(1, int(round(fp_ah * ratio)))

    scr["width"]  = new_w
    scr["height"] = new_h
    scr["avail_width"]  = new_aw
    scr["avail_height"] = new_ah

    win = config.get("window")
    if isinstance(win, dict):
        for k in ("outer_width", "inner_width"):
            v = win.get(k)
            if isinstance(v, (int, float)) and v > 0:
                win[k] = max(1, int(round(v * ratio)))
        for k in ("outer_height", "inner_height"):
            v = win.get(k)
            if isinstance(v, (int, float)) and v > 0:
                win[k] = max(1, int(round(v * ratio)))


def _use_host(config: dict, host_w: int, host_h: int) -> None:
    import sys
    taskbar_h = 40 if sys.platform == "win32" else 0
    avail_w = host_w
    avail_h = max(1, host_h - taskbar_h)

    scr = _scr(config)
    scr["width"]  = host_w
    scr["height"] = host_h
    scr["avail_width"]  = avail_w
    scr["avail_height"] = avail_h

    win = _win(config)
    win["outer_width"]  = avail_w
    win["outer_height"] = max(1, avail_h - 1)
    win["inner_width"]  = avail_w
    win["inner_height"] = max(1, avail_h - 88)


def default_mode_for(platform: str, mobile: bool = False) -> str:
    """Map `navigator.platform` to the launcher's default screen mode."""
    if platform == "macOS":
        return "cap_to_host"
    # A handset reports "Linux" too, and the host monitor is not a phone
    # screen: that pair is the one reading no real handset could give. Only a
    # desktop Linux takes the host.
    if mobile:
        return "profile"
    if platform in ("Windows", "Linux"):
        return "use_host"
    return "profile"
