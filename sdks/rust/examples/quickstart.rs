//! Minimal end-to-end: install the engine, create a persistent profile,
//! launch + drive it over CDP, then reopen it later — the same fingerprint and
//! browser state come back. Delete it when you're done.
//!
//! Run with:  cargo run --example quickstart

use shardx::{LaunchOptions, Profile, ShardX, ShardXOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sdk = ShardX::new(ShardXOptions::default())?;

    // 1. Create a persistent profile: takes a library template (or a random
    //    one when None), enriches it with randomized params, and freezes it
    //    under a fresh unique id. Do this ONCE per profile.
    let mut profile = sdk.create_profile(None).await?;
    println!("created profile {}", profile.id);

    // 1a. Anti-fingerprint noise — declarative: exactly these vectors on, the
    //     rest off (drop one on a later call and it turns off). Then persist.
    //     Vectors: canvas, webgl, audio, client_rects, sensors, fonts.
    profile.set_noise(&["canvas", "audio", "webgl"]);
    sdk.save_profile(&profile)?;

    // Saved profiles you can reopen later:
    println!("saved profiles: {:?}", sdk.list_saved_profiles()?);

    // 2. Launch it. `randomize: false` keeps the frozen fingerprint stable;
    //    state lives in <profiles_root>/<id>/ keyed by profile.id.
    let session = sdk
        .session(
            profile.clone(),
            LaunchOptions {
                randomize: false,
                // proxy: Some("socks5://user:pass@host:1080".into()),
                ..Default::default()
            },
        )
        .await?;

    let page = session.new_page("https://example.com").await?;
    println!("title: {:?}", page.get_title().await?);
    session.close().await?;

    // 3. Later (even another process): reopen the SAME profile by id — same
    //    fingerprint, same cookies/cache.
    let _reopened: Profile = sdk.open_profile(&profile.id)?;

    // 4. When you no longer need it, delete it (wipes config + state).
    // sdk.delete_profile(&profile.id)?;

    Ok(())
}
