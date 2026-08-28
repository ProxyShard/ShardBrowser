//! Screen strategies — three modes matching the launcher's
//! `clamp_screen_to_real_display`:
//!
//! * `Profile` — keep whatever the fingerprint claims.
//! * `CapToHost` — macOS default. Scale screen/window down if the host is
//!   smaller than the FP claim; no-op otherwise.
//! * `UseHost` — Win/Linux default. Overwrite screen/window with the host
//!   display, subtract a taskbar inset for avail_height.

use serde_json::{json, Value};

use crate::host::host_screen_size;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenStrategy {
    Profile,
    CapToHost,
    UseHost,
}

/// Default screen mode for a `navigator.platform`, matching the launcher.
pub fn default_screen_mode_for(platform: &str) -> ScreenStrategy {
    match platform {
        "macOS" => ScreenStrategy::CapToHost,
        "Windows" | "Linux" => ScreenStrategy::UseHost,
        _ => ScreenStrategy::Profile,
    }
}

fn as_int(v: Option<&Value>) -> i64 {
    v.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

/// Apply `mode` to `cfg` in place. No-op for `Profile` or when the host size
/// can't be probed.
pub fn apply_screen_strategy(cfg: &mut Value, mode: ScreenStrategy) {
    if mode == ScreenStrategy::Profile {
        return;
    }
    let Some((hw, hh)) = host_screen_size() else {
        return;
    };
    match mode {
        ScreenStrategy::CapToHost => cap_to_host(cfg, hw as i64, hh as i64),
        ScreenStrategy::UseHost => use_host(cfg, hw as i64, hh as i64),
        ScreenStrategy::Profile => {}
    }
}

fn cap_to_host(cfg: &mut Value, hw: i64, hh: i64) {
    let Some(scr) = cfg.get("screen").and_then(|s| s.as_object()).cloned() else {
        return;
    };
    let fp_w = as_int(scr.get("width"));
    let fp_h = as_int(scr.get("height"));
    if fp_w <= 0 || fp_h <= 0 {
        return;
    }
    if hw >= fp_w && hh >= fp_h {
        return;
    }
    let ratio = (hw as f64 / fp_w as f64).min(hh as f64 / fp_h as f64);
    let scale = |v: i64| ((v as f64 * ratio).round() as i64).max(1);

    let fp_aw = {
        let v = as_int(scr.get("avail_width"));
        if v > 0 { v } else { fp_w }
    };
    let fp_ah = {
        let v = as_int(scr.get("avail_height"));
        if v > 0 { v } else { fp_h }
    };

    let scr_mut = cfg["screen"].as_object_mut().unwrap();
    scr_mut.insert("width".into(), json!(scale(fp_w)));
    scr_mut.insert("height".into(), json!(scale(fp_h)));
    scr_mut.insert("avail_width".into(), json!(scale(fp_aw)));
    scr_mut.insert("avail_height".into(), json!(scale(fp_ah)));

    if let Some(win) = cfg.get_mut("window").and_then(|w| w.as_object_mut()) {
        for k in ["outer_width", "inner_width", "outer_height", "inner_height"] {
            let v = as_int(win.get(k));
            if v > 0 {
                win.insert(k.into(), json!(scale(v)));
            }
        }
    }
}

fn use_host(cfg: &mut Value, hw: i64, hh: i64) {
    let taskbar = if cfg!(target_os = "windows") { 40 } else { 0 };
    let avail_w = hw;
    let avail_h = (hh - taskbar).max(1);

    if !cfg.get("screen").map(|s| s.is_object()).unwrap_or(false) {
        cfg["screen"] = json!({});
    }
    let scr = cfg["screen"].as_object_mut().unwrap();
    scr.insert("width".into(), json!(hw));
    scr.insert("height".into(), json!(hh));
    scr.insert("avail_width".into(), json!(avail_w));
    scr.insert("avail_height".into(), json!(avail_h));

    if !cfg.get("window").map(|w| w.is_object()).unwrap_or(false) {
        cfg["window"] = json!({});
    }
    let win = cfg["window"].as_object_mut().unwrap();
    win.insert("outer_width".into(), json!(avail_w));
    win.insert("outer_height".into(), json!((avail_h - 1).max(1)));
    win.insert("inner_width".into(), json!(avail_w));
    win.insert("inner_height".into(), json!((avail_h - 88).max(1)));
}
