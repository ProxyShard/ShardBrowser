# @proxyshard/shardx (Node)

Self-contained Node/TypeScript SDK for the **ShardX anti-detect
browser** by the [ProxyShard](https://proxyshard.com?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher) team.

Does **not** depend on the desktop launcher. On first use it downloads
the patched Chromium 149 engine, Widevine CDM, and the 170-profile
fingerprint library from our CDN into a local cache, then launches
isolated browser sessions on demand.

Driven by [patchright](https://github.com/Kaliiiiiiiiii-Vinyzu/patchright)
(stealth-patched Playwright) — `sdk.session()` returns a ready-to-use
`Browser` instance, no manual `connectOverCDP` plumbing.

## Install

```bash
npm install @proxyshard/shardx
```

Supported hosts: **macOS arm64**, **Windows x64**, **Linux x64**. Node ≥ 18.

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
`--disable-dev-shm-usage` via `extraArgs`:

```ts
await sdk.session(profile, { extraArgs: ["--no-sandbox", "--disable-dev-shm-usage"] });
```

## Quick start

```ts
import { ShardX } from "@proxyshard/shardx";

const sdk = new ShardX();
// Engine + Widevine + fingerprint library auto-download from CDN on
// the first session/launch/listProfiles call (~170 MB once, etag-cached
// afterward).  No separate install step.

// Create a persistent profile from a library template (or createProfile() for
// a random one). Library templates aren't launched directly — this freezes an
// enriched copy under a unique id you can return to. Do it once.
const profile = await sdk.createProfile("win-rtx4060");

// Launch + drive in one call. Returns the patchright Browser.
const { browser, session, close } = await sdk.session(profile, {
  proxy: "socks5://user:pass@host:port",
});
try {
  const ctx = browser.contexts()[0];
  const page = await ctx.newPage();
  await page.goto("https://browserleaks.com/quic");
  console.log(await page.title());

  // Inspect what the SDK resolved before launch:
  console.log(session.geo);             // { countryCode: 'DE', timezone: 'Europe/Berlin', ... }
  console.log(session.proxyUdpMs,       // UDP RTT in ms or null
              session.quicEnabled,      // boolean
              session.webrtcMode);      // "auto" | "tcp_only" | "block"
} finally {
  await close();                        // tears down patchright + the engine
}
```

### Random profile

```ts
// createProfile() with no id freezes a random library template (filter the
// pool with { platform: "Windows" }). It already randomises hw_concurrency /
// RAM / platform_version once, at creation.
const profile = await sdk.createProfile(undefined, { platform: "Windows" });
const { browser, close } = await sdk.session(profile);
try {
  const page = await browser.contexts()[0].newPage();
  // ...
} finally {
  await close();
}
```

### Discover bundled profiles

```ts
console.log(sdk.listProfiles().slice(0, 5));
// [ 'linux-gt1030', 'linux-gtx1050', 'mac-m1-air13', 'mac-m1-imac24', 'mac-m1-max-mbp14' ]

console.log(sdk.listProfiles({ platform: "Windows" }).slice(0, 5));

const profile = sdk.randomProfile({ platform: "macOS" });
console.log(profile.id, profile.config.webgl.renderer);
```

### Validate a proxy before binding

```ts
console.log(await sdk.checkProxy("socks5://user:pass@host:port"));
// {
//   udpMs: 142,
//   geo: { countryCode: 'DE', timezone: 'Europe/Berlin', ... },
//   wouldEnableQuic: true,
//   wouldSetWebrtc: 'auto',
// }
```

## Persistent profiles

`listProfiles()` / `randomProfile()` hand back **library templates** — launch
one and it re-reads the same template every time. For a profile you can
**return to** (same fingerprint *and* cookies/cache) or **delete**, create a
*saved profile*: it freezes a template (or a random one) with randomized
hardware/platform_version under a fresh unique id in its own folder
`<cache>/profiles/<id>/`, exactly like the desktop launcher's "create profile".

```ts
const sdk = new ShardX();

// Create once (random template, or pass a library id like "win-rtx4060").
const profile = await sdk.createProfile();
console.log(profile.id);

console.log(sdk.listSavedProfiles());     // ['<id>', ...]

// Launch it. randomize stays false → the frozen fingerprint is stable;
// cookies/cache persist in the profile's folder across runs.
const { browser, close } = await sdk.session(profile);
// ...
await close();

// Later — even a different process — reopen by id: same fingerprint + state.
const reopened = sdk.openProfile("<id>");

// Remove the profile and all its state when you're done.
sdk.deleteProfile("<id>");
```

Saved profiles never touch the bundled S3 library: templates stay read-only in
`<cache>/fingerprints/*.json`, saved profiles live in `<cache>/profiles/<id>/`
(holding `profile.json` + the browser's user-data-dir).

## Anti-fingerprint noise

Per-vector noise (canvas / WebGL / audio / DOMRect / sensors / fonts) is **off
by default**. `setNoise(...)` is **declarative** — exactly the vectors you list
are enabled (with soft defaults), every other one is turned off:

```ts
profile.setNoise("canvas", "audio", "webgl");   // only these three on
profile.setNoise("canvas");                       // audio + webgl now off again
profile.setNoise();                               // all off
sdk.saveProfile(profile);                          // persist the choice
```

Seeds are derived **per-profile** at launch — stable across runs, unique per
profile — so two profiles with the same vectors enabled still produce different
canvas/audio/WebGL fingerprints. Soft defaults: WebGL `intensity 0.0005`,
DOMRect `maxOffset 1`.

## Pre-launch checks

Every call to `sdk.session()` / `sdk.launch()` runs the same pre-spawn
pipeline the desktop launcher uses:

1. **`resolveAutoFields`** — if the profile has `"auto"` sentinels for
   `timezone`, `navigator.language`, or `geolocation.mode`, the SDK
   makes a live geo lookup through the bound proxy (`ip-api.com` by
   default). Concrete values get written back: timezone (from the API,
   never a static table), `accept_language` chain, `languages`,
   `icu_locale` (always overwritten so `Intl.*` matches
   `navigator.language`), and lat/lng. Proxy-via failure → direct geo
   → host `Intl.DateTimeFormat().resolvedOptions().timeZone` as
   last-resort fallback. The resolved geo is surfaced on
   `session.geo`.
2. **`applyScreenStrategy`** — see below.
3. **`probeUdp`** — SOCKS5 UDP_ASSOCIATE round-trip. If it fails, QUIC
   is force-disabled and WebRTC switches to `tcp_only` automatically.

### Screen strategy

`screenMode` option to `session()` / `launch()`:

* **`"profile"`** — keep whatever the fingerprint claims.
* **`"cap_to_host"`** — *macOS default.* If the host monitor is smaller
  than the FP screen, scale `screen.*` + `window.*` down proportionally;
  otherwise no-op.
* **`"use_host"`** — *Windows / Linux default.* Overwrite `screen.*`
  with the real monitor (minus a 40 px Windows taskbar) and recompute
  `window.outer*` / `window.inner*`.

Default mode is picked from `navigator.platform`. Override per launch:

```ts
await sdk.session(profile, { screenMode: "profile" });
```

### Host-aware hardware randomisation

`randomize: true` re-picks `hardware_concurrency`, `device_memory`, and
`platform_version` before the launch — using the same logic as the
desktop launcher:

* **macOS** profiles use the curated `MAC_HW_CONFIGS` table by id.
* **Windows / Linux** profiles bracket the host's logical CPU count
  within `[host − 4, host + 2]` from the real x86 set
  `[4, 6, 8, 12, 16, 20, 24, 28, 32]`; `device_memory` is floored by
  core count (≥ 12 → 16, else 8) and capped by `hostRamBucketGb()`
  (8 / 16 / 32 GiB bucketed from `sysctl hw.memsize` / `/proc/meminfo`
  / `Get-CimInstance Win32_ComputerSystem`).

So a profile launched on an 8-core / 16 GB laptop will never claim
32 cores / 128 GB of RAM.

### Override fingerprint fields

```ts
const profile = sdk.library
  .load("win-rtx4060")
  .withOverride({
    name: "my-account",
    timezone: "Europe/Berlin",
    navigator: { language: "de-DE" },
  });

const { browser, close } = await sdk.session(profile, { proxy: "socks5://..." });
```

### Use your own fingerprint JSON

```ts
import { Profile } from "@proxyshard/shardx";

const profile = Profile.fromFile("/path/to/my-custom.json");
const { browser, close } = await sdk.session(profile);
```

### WebRTC policy

```ts
await sdk.session(profile, {
  proxy: "socks5://...",
  webrtc: "tcp_only",                // "auto" (default) | "block" | "tcp_only"
  webrtcPublicIp: "203.0.113.42",    // advertised in ICE candidates
});
```

### Progress callback during the first-run download

The first `session`/`launch`/`listProfiles` triggers the download.  Hook
it with a progress callback on the constructor:

```ts
const sdk = new ShardX({
  progress: (label, received, total) => {
    const pct = total ? Math.floor((received / total) * 100) : 0;
    console.log(`${label}: ${pct}%`);
  },
});
const profile = await sdk.createProfile("win-rtx4060");
const { browser, close } = await sdk.session(profile);
```

## Advanced: raw launch without patchright

If you'd rather drive the browser with a different CDP client (raw
`chrome-remote-interface`, puppeteer-core's `connect`, your own
WebSocket), skip `session()` and use `launch()` directly:

```ts
const profile = await sdk.createProfile("win-rtx4060");
const sess = await sdk.launch(profile, { proxy: "socks5://...", cdp: true });
console.log(sess.cdpUrl);              // ws://127.0.0.1:54113/devtools/browser/...
// ... drive it yourself ...
await sess.stop();
```

`launch()` runs the same pre-launch pipeline (auto-resolve, screen
strategy, UDP probe, hw randomisation) and returns a `BrowserSession`
with `cdpUrl`, `geo`, `proxyUdpMs`, `quicEnabled`, `webrtcMode`,
`userDataDir`, and `stop()`.

## Cache layout

```
~/Library/Application Support/shardx-sdk/    (mac)
%LOCALAPPDATA%\shardx-sdk\                   (win)
~/.cache/shardx-sdk/                         (linux)
├── manifest.json             ← etag cache
├── ShardX-Mac-arm64/         ← extracted engine
├── fingerprints/             ← 170 bundled .json templates (read-only)
└── profiles/<id>/            ← saved profile (createProfile) + its state
    ├── profile.json          ← the frozen fingerprint config
    └── …                     ← user-data-dir: cookies, IndexedDB, cache
```

Override:

```ts
const sdk = new ShardX({ cacheDir: "/data/shardx" });
```

## Update the runtime

The SDK auto-checks remote etags on the first `session`/`launch`/`listProfiles`
call of each process and re-downloads anything that changed.  To force a
re-download mid-process:

```ts
await sdk.runtime.install({ force: true });
```

## License

MIT (this SDK). The Chromium-fork engine binary it downloads at
runtime is a closed-source product — see the
[main repo](https://github.com/ProxyShard/ShardBrowser) for engine
licensing.
