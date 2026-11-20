//! The Web Store update check on a timer (design §3.2.4): 120 s after start,
//! then every 5 h ± 10 %, never sooner than that after the last one; updates
//! waiting for profiles to close are tried again every minute.

use crate::space;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FIRST: Duration = Duration::from_secs(120);
const EVERY: Duration = Duration::from_secs(5 * 60 * 60);
const TICK: Duration = Duration::from_secs(60);
/// A check nobody answered is tried again after this, doubling up to `EVERY`.
const RETRY: Duration = Duration::from_secs(30 * 60);

/// The installed engine's version; None for a browser of the person's own,
/// whose version the launcher does not know (the bundle's stands in then).
pub fn engine() -> Option<String> {
    let own = crate::settings::load().ok().and_then(|s| s.browser_path).is_some_and(|p| !p.trim().is_empty());
    if own {
        return None;
    }
    crate::runtime::engine_version()
}

/// On, and a proxy chosen for the Web Store: without one the timer stays quiet (#154).
fn enabled() -> bool {
    crate::settings::load().map(|s| s.ext_update_check).unwrap_or(true) && space::extensions::store_proxy_chosen()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// 5 h give or take a tenth, so launchers do not all ask at once.
fn interval() -> u64 {
    let every = EVERY.as_secs();
    let span = every / 5;
    every * 9 / 10 + (uuid::Uuid::new_v4().as_u128() % u128::from(span + 1)) as u64
}

/// When to try again after `failures` checks in a row got no answer.
fn backoff(failures: u32) -> u64 {
    RETRY.as_secs().saturating_mul(1 << failures.saturating_sub(1).min(4)).min(EVERY.as_secs())
}

/// The timer; wall-clock based, so a computer that slept checks when it wakes.
pub fn start() {
    tauri::async_runtime::spawn(async {
        // A last check "in the future" (the clock was set back since) counts as now.
        let last = space::extensions::updates_checked_at().min(now());
        let mut due = (last + interval()).max(now() + FIRST.as_secs());
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(TICK).await;
            let _ = tauri::async_runtime::spawn_blocking(space::extensions::retry_waiting).await;
            if now() < due {
                continue;
            }
            due = now() + interval();
            if !enabled() {
                continue;
            }
            // In the background only news is told; a failure stays in the log.
            match space::extensions::check_updates(engine()).await {
                Ok(news) => {
                    failures = 0;
                    if !news.is_empty() {
                        crate::notify_event("extension-updates:news", serde_json::to_value(&news).unwrap_or_default())
                    }
                }
                Err(e) => {
                    failures += 1;
                    due = now() + backoff(failures);
                    eprintln!("[extensions] update check: {e}; tried again in {} min", backoff(failures) / 60);
                }
            }
        }
    });
}

/// The Check button: asks now (or waits for a check under way) and returns
/// the updates. A team's extensions are not checked yet.
pub async fn check_now(t: &space::Ticket) -> Result<Vec<space::extensions::ExtensionUpdate>, String> {
    if t.team().is_some() {
        return Ok(Vec::new());
    }
    let engine = engine();
    space::extensions::check_updates(engine.clone()).await?;
    space::extensions::updates(engine).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unanswered_check_is_tried_again_after_a_growing_wait() {
        let mins: Vec<u64> = (1..=7).map(|n| backoff(n) / 60).collect();
        assert_eq!(mins, [30, 60, 120, 240, 300, 300, 300]);
    }
}
