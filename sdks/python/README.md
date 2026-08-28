# shardx (Python)

Self-contained Python SDK for the **ShardX anti-detect browser** by the
[ProxyShard](https://proxyshard.com?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher) team.

This package does **not** depend on the desktop launcher. On first use
it downloads the patched Chromium 149 engine, Widevine CDM, and the
170-profile fingerprint library from our CDN into a local cache, then
launches isolated browser sessions on demand.

Driven by [patchright](https://github.com/Kaliiiiiiiiii-Vinyzu/patchright-python)
(stealth-patched Playwright) — `sdk.session()` returns a ready-to-use
`Browser` instance, no manual `connect_over_cdp` plumbing.

## Install

```bash
pip install shardx
```

Supported hosts: **macOS arm64**, **Windows x64**, **Linux x64**.

### Linux system dependencies

The bundled Chromium engine needs `unzip` + the standard set of shared
libraries any Chromium fork links against. On a fresh Debian / Ubuntu:

```bash
sudo apt install -y \
  unzip ca-certificates fonts-liberation \
  libnss3 libnspr4 libatk1.0-0 libatk-bridge2.0-0 libcups2 \
  libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 \
  libgbm1 libpango-1.0-0 libcairo2 libasound2 libxshmfence1
```

When launching as **root** or inside **Docker**, pass `--no-sandbox` and
`--disable-dev-shm-usage` via `extra_args=`:

```python
await sdk.session(..., extra_args=["--no-sandbox", "--disable-dev-shm-usage"])
```

## Quick start

```python
import asyncio
from shardx import ShardX

async def main():
    sdk = ShardX()
    # Engine + Widevine + fingerprint library auto-download from CDN on
    # the first `session`/`launch`/`list_profiles` call (~170 MB once,
    # etag-cached afterward).  No separate install step.

    # Create a persistent profile from a library template (or create_profile()
    # for a random one). Library templates aren't launched directly — this
    # freezes an enriched copy under a unique id you can return to. Do it once.
    profile = sdk.create_profile("win-rtx4060")

    # Launch + drive in one call. Yields a patchright `Browser`.
    async with sdk.session(profile, proxy="socks5://user:pass@host:port") as browser:
        ctx = browser.contexts[0]
        page = await ctx.new_page()
        await page.goto("https://browserleaks.com/quic")
        print(await page.title())

        # Inspect what the SDK resolved before launch:
        sess = browser._shardx
        print(sess.geo)                   # GeoInfo(...) from ip-api / ipapi.co
        print(sess.proxy_udp_ms,          # UDP RTT in ms or None
              sess.quic_enabled,          # bool
              sess.webrtc_mode)           # "auto" | "tcp_only" | "block"
    # browser + udd shut down cleanly on exit

asyncio.run(main())
```

### Random profile

```python
# create_profile() with no id freezes a random library template (filter the
# pool with platform="Windows"). It already randomises hardware_concurrency /
# device_memory / platform_version once, at creation.
profile = sdk.create_profile(platform="Windows")
async with sdk.session(profile) as browser:
    page = await browser.contexts[0].new_page()
    ...
```

### Discover bundled profiles

```python
print(sdk.list_profiles()[:5])
# ['linux-gt1030', 'linux-gtx1050', 'mac-m1-air13', 'mac-m1-imac24', 'mac-m1-max-mbp14']

print(sdk.list_profiles(platform="Windows")[:5])

profile = sdk.random_profile(platform="macOS")
print(profile.id, profile.config["webgl"]["renderer"])
```

### Validate a proxy before binding

```python
print(sdk.check_proxy("socks5://user:pass@host:port"))
# {
#   'udp_ms': 142.3,
#   'geo': GeoInfo(country_code='DE', timezone='Europe/Berlin', ...),
#   'would_enable_quic': True,
#   'would_set_webrtc': 'auto',
# }
```

## Persistent profiles

`list_profiles()` / `random_profile()` hand back **library templates** — launch
one and it re-reads the same template every time. For a profile you can
**return to** (same fingerprint *and* cookies/cache) or **delete**, create a
*saved profile*: it freezes a template (or a random one) with randomized
hardware/platform_version under a fresh unique id in its own folder
`<cache>/profiles/<id>/`, exactly like the desktop launcher's "create profile".

```python
sdk = ShardX()

# Create once (random template, or pass a library id like "win-rtx4060").
profile = sdk.create_profile()
print(profile.id)

print(sdk.list_saved_profiles())          # ['<id>', ...]

# Launch it. randomize stays False → the frozen fingerprint is stable;
# cookies/cache persist in the profile's folder across runs.
async with sdk.session(profile) as browser:
    ...

# Later — even a different process — reopen by id: same fingerprint + state.
profile = sdk.open_profile("<id>")

# Remove the profile and all its state when you're done.
sdk.delete_profile("<id>")
```

Saved profiles never touch the bundled S3 library: templates stay read-only in
`<cache>/fingerprints/*.json`, saved profiles live in `<cache>/profiles/<id>/`
(holding `profile.json` + the browser's user-data-dir).

## Anti-fingerprint noise

Per-vector noise (canvas / WebGL / audio / DOMRect / sensors / fonts) is **off
by default**. `set_noise(...)` is **declarative** — exactly the vectors you list
are enabled (with soft defaults), every other one is turned off:

```python
profile.set_noise("canvas", "audio", "webgl")   # only these three on
profile.set_noise("canvas")                       # audio + webgl now off again
profile.set_noise()                               # all off
sdk.save_profile(profile)                          # persist the choice
```

Seeds are derived **per-profile** at launch — stable across runs, unique per
profile — so two profiles with the same vectors enabled still produce different
canvas/audio/WebGL fingerprints. Soft defaults: WebGL `intensity 0.0005`,
DOMRect `max_offset 1`.

## Pre-launch checks

Every call to `sdk.session()` / `sdk.launch()` runs the same pre-spawn
pipeline the desktop launcher uses:

1. **`resolve_auto_fields`** — if the profile has `"auto"` sentinels for
   `timezone`, `navigator.language`, or `geolocation.mode`, the SDK
   makes a live geo lookup through the bound proxy (`ip-api.com` by
   default). It then writes concrete values: timezone (from the API,
   never a static table), `accept_language` chain, `languages`,
   `icu_locale` (always overwritten so `Intl.*` matches
   `navigator.language`), and lat/lng. Proxy-via failure → direct geo
   → host `$LANG` / `$TZ` as last-resort fallback. The chosen geo is
   surfaced on `session.geo`.
2. **`apply_screen_strategy`** — see below.
3. **`probe_udp`** — SOCKS5 UDP_ASSOCIATE round-trip. If it fails, QUIC
   is force-disabled and WebRTC switches to `tcp_only` automatically.

### Screen strategy

`screen_mode` kw to `session()` / `launch()`:

* **`"profile"`** — keep whatever the fingerprint claims.
* **`"cap_to_host"`** — *macOS default.* If the host monitor is smaller
  than the FP screen, scale `screen.*` + `window.*` down proportionally;
  otherwise no-op.
* **`"use_host"`** — *Windows/Linux default.* Overwrite `screen.*` with
  the real monitor (minus a 40 px Windows taskbar) and recompute
  `window.outer_*` / `window.inner_*` accordingly.

Default mode is picked from `navigator.platform`. Override per launch:

```python
async with sdk.session(profile, screen_mode="profile") as browser:
    ...
```

### Host-aware hardware randomisation

`randomize=True` re-picks `hardware_concurrency`, `device_memory`, and
`platform_version` before the launch — using the same logic as the
desktop launcher (`randomize_hardware` in `lib.rs`):

* **macOS** profiles use the curated `MAC_HW_CONFIGS` table by id.
* **Windows / Linux** profiles bracket the host's logical CPU count
  within `[host − 4, host + 2]` from the real x86 set
  `[4, 6, 8, 12, 16, 20, 24, 28, 32]`; `device_memory` is floored by
  core count (≥ 12 → 16, else 8) and capped by `host_ram_bucket_gb()`
  (8 / 16 / 32 GiB bucketed from `sysctl hw.memsize` / `/proc/meminfo`
  / `Get-CimInstance Win32_ComputerSystem`).

So a profile launched on an 8-core / 16 GB laptop will never claim
32 cores / 128 GB of RAM — keeps fingerprints internally consistent
with real-world hardware.

### Override fingerprint fields

```python
profile = sdk.library.load("win-rtx4060").with_override(
    name="my-account",
    timezone="Europe/Berlin",
    navigator={"language": "de-DE"},
)
async with sdk.session(profile, proxy="socks5://...") as browser:
    ...
```

### Use your own fingerprint JSON

```python
from shardx import Profile

profile = Profile.from_file("/path/to/my-custom.json")
async with sdk.session(profile) as browser:
    ...
```

### WebRTC policy

```python
async with sdk.session(
    profile,
    proxy="socks5://...",
    webrtc="tcp_only",                # or "block" | "auto" (default)
    webrtc_public_ip="203.0.113.42",  # advertised in ICE candidates
) as browser:
    ...
```

## Advanced: raw launch without patchright

If you'd rather drive the browser with a different CDP client (raw
`pychrome`, `pyppeteer`, your own WebSocket), skip `session()` and use
`launch()` directly:

```python
profile = sdk.create_profile("win-rtx4060")
sess = sdk.launch(profile, proxy="socks5://...", cdp=True)
print(sess.cdp_url)        # ws://127.0.0.1:54113/devtools/browser/c0a3…
# … drive it yourself …
sess.stop()
```

`launch()` does the same pre-launch pipeline (auto-resolve, screen
strategy, UDP probe, hw randomisation) and returns a `BrowserSession`
with `cdp_url`, `geo`, `proxy_udp_ms`, `quic_enabled`, `webrtc_mode`,
`user_data_dir`, and `stop()`.

## Cache layout

```
~/Library/Application Support/shardx-sdk/    (mac)
%LOCALAPPDATA%\shardx-sdk\                   (win)
~/.cache/shardx-sdk/                         (linux)
├── manifest.json             ← etag cache for browser/widevine/fingerprints
├── ShardX-Mac-arm64/         ← extracted engine
│   └── ShardX.app/…
├── fingerprints/             ← 170 bundled .json profiles
│   ├── win-rtx4060.json
│   └── …
└── profiles/                 ← saved profiles (create_profile) + their state
    └── <profile-id>/
        ├── profile.json      ← the frozen fingerprint config
        └── …                 ← user-data-dir: cookies, IndexedDB, cache
```

Override the cache root:

```python
sdk = ShardX(cache_dir="/data/shardx")
```

## Update the runtime

The SDK auto-checks remote etags on the first `session`/`launch`/`list_profiles`
call of each process and re-downloads anything that changed.  To force a
re-download mid-process (e.g. CI scenarios):

```python
sdk.runtime.install(force=True)
```

## License

MIT (this SDK). The Chromium-fork engine binary downloaded at runtime
is a closed-source product — see the
[main repo](https://github.com/ProxyShard/ShardBrowser) for engine
licensing.
