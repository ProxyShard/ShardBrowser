# `shardx` — Rust SDK

Self-contained Rust SDK for the **ShardX anti-detect browser** by the
[ProxyShard](https://proxyshard.com?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher)
team. Same surface as the [Python](../python) and [Node](../node) SDKs: on
first use it downloads the engine, Widevine CDM, and the bundled fingerprint
library from the ProxyShard CDN into a per-user cache dir, then launches
isolated profiles with the exact spoofing flags the desktop launcher uses.

Supported hosts: **macOS arm64**, **Windows x64**, **Linux x64**.
On macOS/Linux the system `unzip` is used for extraction (preserves symlinks
and exec bits) — install it with `brew install unzip` / `apt install unzip`.

## Install

```toml
[dependencies]
shardx = "0.1"
tokio = { version = "1", features = ["full"] }
```

## Quickstart — launch **and drive** the browser

`session()` launches the engine and attaches a [chromiumoxide](https://docs.rs/chromiumoxide)
CDP browser in one call (the Rust equivalent of patchright in the Python/Node
SDKs). It's behind the default `control` feature.

```rust
use shardx::{ShardX, ShardXOptions, LaunchOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sdk = ShardX::new(ShardXOptions::default())?;

    // Create a persistent profile from a library template (or None for a
    // random one): enriches a COPY with randomized hw/platform_version and
    // freezes it under a unique id — the fingerprint library is read-only and
    // never launched directly. Do this once per profile.
    let profile = sdk.create_profile(Some("win-rtx4060")).await?;

    // Launch it through a proxy and get a driven browser. randomize: false
    // keeps the frozen fingerprint stable; cookies/cache persist across runs.
    let session = sdk
        .session(
            profile.clone(),
            LaunchOptions {
                proxy: Some("socks5://user:pass@host:1080".into()),
                randomize: false,
                ..Default::default()
            },
        )
        .await?;

    println!(
        "pid={}  quic={}  webrtc={:?}",
        session.engine.pid, session.engine.quic_enabled, session.engine.webrtc_mode,
    );

    // Drive it with chromiumoxide.
    let page = session.new_page("https://example.com").await?;
    println!("title: {:?}", page.get_title().await?);
    // `session.browser` is the full `chromiumoxide::Browser` for anything else.

    session.close().await?; // disconnect + stop the engine
    Ok(())
}
```

Pass `None` to `create_profile` for a **random** template. To launch your own
fingerprint, build a `Profile` (`Profile::from_file(path)` or
`Profile::new(serde_json::json!({ ... }), None)`) and hand it to `session` —
`launch`/`session` only take a `Profile`, never a raw library id.

### Without a driver (lighter build)

If you don't want the CDP client, disable the feature
(`shardx = { version = "0.1", default-features = false }`) and use
`launch` (no CDP) or `launch_cdp` (exposes `session.cdp_url` for your own
client):

```rust
let profile = sdk.create_profile(None).await?;
let mut engine = sdk.launch_cdp(profile, LaunchOptions::default()).await?;
println!("CDP: {:?}", engine.cdp_url);
engine.stop().await?;
```

## Persistent profiles

`list_profiles()` / random launches hand back **library templates** — launch
one and it re-reads the same template every time. For a profile you can
**return to** (same fingerprint *and* cookies/cache) or **delete**, create a
*saved profile*: it freezes a template (or a random one) with randomized
hardware/platform_version under a fresh unique id in its own folder
`<cache>/profiles/<id>/`, exactly like the desktop launcher's "create profile".

```rust
use shardx::{ShardX, ShardXOptions, LaunchOptions};

let sdk = ShardX::new(ShardXOptions::default())?;

// Create once (random template, or Some("win-rtx4060")).
let mut profile = sdk.create_profile(None).await?;
println!("{}", profile.id);
println!("{:?}", sdk.list_saved_profiles()?);          // ["<id>", ...]

// Launch it — randomize: false keeps the frozen fingerprint stable; cookies /
// cache persist in the profile's folder across runs.
let session = sdk.session(
    profile.clone(),
    LaunchOptions { randomize: false, ..Default::default() },
).await?;
// ... drive it, then session.close().await?; ...

// Later — even another process — reopen by id: same fingerprint + state.
let profile = sdk.open_profile(&profile.id)?;

// Remove the profile and all its state when you're done.
sdk.delete_profile(&profile.id)?;
```

Saved profiles never touch the bundled S3 library: templates stay read-only in
`<cache>/fingerprints/*.json`, saved profiles live in `<cache>/profiles/<id>/`
(holding `profile.json` + the browser's user-data-dir).

## Anti-fingerprint noise

Per-vector noise (canvas / WebGL / audio / DOMRect / sensors / fonts) is **off
by default**. `set_noise(...)` is **declarative** — exactly the vectors you list
are enabled (with soft defaults), every other one is turned off:

```rust
profile.set_noise(&["canvas", "audio", "webgl"]); // only these three on
profile.set_noise(&["canvas"]);                    // audio + webgl now off again
profile.set_noise(&[]);                            // all off
sdk.save_profile(&profile)?;                        // persist the choice
```

Seeds are derived **per-profile** at launch — stable across runs, unique per
profile — so two profiles with the same vectors enabled still produce different
canvas/audio/WebGL fingerprints. Soft defaults: WebGL `intensity 0.0005`,
DOMRect `max_offset 1`.

## Validate a proxy before binding it

```rust
let res = sdk.check_proxy("socks5://user:pass@host:1080").await?;
println!(
    "udp={:?}ms  quic={}  webrtc={:?}  exit={} ({})",
    res.udp_ms, res.would_enable_quic, res.would_set_webrtc,
    res.geo.ip, res.geo.country_code,
);
```

The same SOCKS5 `UDP_ASSOCIATE` probe the launcher runs decides whether QUIC
is enabled and whether WebRTC is forced to `tcp_only`.

## What `launch` does for you

Before spawning the engine the SDK reproduces the launcher's pre-flight:

* **auto-resolve** — fills `"auto"` timezone / language / geolocation from a
  live geo lookup *through the bound proxy* ([`resolve_auto_fields`]).
* **screen strategy** — `CapToHost` on macOS, `UseHost` on Win/Linux
  ([`apply_screen_strategy`]); override via `LaunchOptions::screen_mode`.
* **UDP probe** — decides QUIC + WebRTC policy from a live relay probe.

## Lower-level building blocks

Everything the façade uses is public and reusable:

```rust
use shardx::{Runtime, FingerprintLibrary, parse_proxy, probe_udp, geo_check_via,
             randomize_hardware, host_screen_size};
```

`Runtime` (download/cache/extract), `FingerprintLibrary` + `Profile`,
`parse_proxy` / `proxy_to_arg` / `probe_udp`, `geo_check_via`,
`randomize_hardware` / `randomize_platform_version`, `host_*` probes, and the
`screen` / `auto_resolve` helpers.

## Links

* **Site:**  [https://proxyshard.com](https://proxyshard.com?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher)
* **Docs:**  [https://docs.proxyshard.com](https://docs.proxyshard.com?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher)
* **Usage:** [https://docs.proxyshard.com/eng/usage-instructions/shardx-browser](https://docs.proxyshard.com/eng/usage-instructions/shardx-browser?utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher)

MIT licensed.
