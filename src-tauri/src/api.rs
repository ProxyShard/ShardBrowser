// Local automation HTTP API (axum) for ShardBrowser Launcher.
// 127.0.0.1:<api_port>; every endpoint except /health requires Bearer JWT (HS256).

use std::sync::{OnceLock, RwLock};

use axum::{
    extract::{Path, Query, Request},
    http::{header::AUTHORIZATION, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::space::{self, CmdError, Door, Ticket};

// ---- HS256 secret (process-global so live rotation invalidates old tokens) ----

fn secret_cell() -> &'static RwLock<String> {
    static SECRET: OnceLock<RwLock<String>> = OnceLock::new();
    SECRET.get_or_init(|| RwLock::new(String::new()))
}

/// Install/replace the signing secret.
pub fn set_secret(s: &str) {
    if let Ok(mut g) = secret_cell().write() {
        *g = s.to_string();
    }
}

fn read_secret() -> String {
    secret_cell().read().map(|g| g.clone()).unwrap_or_default()
}

// ---- JWT ----

#[derive(serde::Serialize, serde::Deserialize)]
struct Claims {
    sub: String,
    iat: u64,
    exp: u64,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mint(secret: &str, ttl_secs: u64) -> Result<String, String> {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let now = unix_now();
    let claims = Claims {
        sub: "shardx-api".into(),
        iat: now,
        exp: now.saturating_add(ttl_secs),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

fn verify(secret: &str, token: &str) -> bool {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .is_ok()
}

/// 10-year token shown in Settings UI.
pub fn long_lived_token(secret: &str) -> Result<String, String> {
    mint(secret, 60 * 60 * 24 * 365 * 10)
}

// ---- error type ----

struct ApiError(StatusCode, Value);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, json!({ "error": msg.into() }))
}

/// A door's refusal: invisible 404, refused 403 with what it needs, a
/// conflict 409, the team server unreachable 503.
fn refusal(c: &CmdError) -> ApiError {
    let mut body = json!({ "error": c.to_string(), "code": c.code() });
    match c {
        CmdError::Forbidden(n) => {
            body["need"] = json!(n.right);
            body["on"] = json!(n.on);
        }
        CmdError::Stale { now: Some(n), .. } => body["current_revision"] = json!(n),
        CmdError::Leased(device) => body["device_name"] = json!(device),
        _ => {}
    }
    ApiError(StatusCode::from_u16(c.status()).unwrap_or(StatusCode::BAD_GATEWAY), body)
}

/// The refusal inside an error, or `fallback` with its words.
fn fail<E: Into<anyhow::Error>>(fallback: StatusCode) -> impl Fn(E) -> ApiError {
    move |e| {
        let e = e.into();
        match space::error::find(&e) {
            Some(c) => refusal(c),
            None => err(fallback, e.to_string()),
        }
    }
}

/// The same, with every cause in the words.
fn fail_all<E: Into<anyhow::Error>>(fallback: StatusCode) -> impl Fn(E) -> ApiError {
    move |e| {
        let e = e.into();
        match space::error::find(&e) {
            Some(c) => refusal(c),
            None => err(fallback, format!("{e:#}")),
        }
    }
}

// ---- the space a request is about ----

/// Sent back on every answer: the space's edition the answer is about. A caller
/// that sends it on a write gets 409 space_changed once the launcher has moved on.
const EPOCH_HEADER: &str = "x-space-epoch";

/// Every request gets the ticket of the space in use as it came in; a write
/// goes to that space or nowhere.
async fn in_space(mut req: Request, next: Next) -> Response {
    let asked = match req.headers().get(EPOCH_HEADER) {
        None => None,
        Some(v) => match v.to_str().ok().and_then(|s| s.trim().parse::<u64>().ok()) {
            Some(n) => Some(n),
            None => return err(StatusCode::BAD_REQUEST, "X-Space-Epoch is a number").into_response(),
        },
    };
    let t = match space::ticket(asked) {
        Ok(t) => t,
        Err(e) => return refusal(&e).into_response(),
    };
    let epoch = t.epoch();
    req.extensions_mut().insert(t);
    let mut res = next.run(req).await;
    res.headers_mut().insert(EPOCH_HEADER, HeaderValue::from(epoch));
    res
}

type ApiResult = Result<Json<Value>, ApiError>;

// ---- auth middleware ----

async fn auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let secret = read_secret();
    let ok = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        })
        .map(|t| verify(&secret, t.trim()))
        .unwrap_or(false);
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

// ---- handlers ----

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "shardx-launcher",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn list_profiles() -> ApiResult {
    crate::space::ready().await;
    let metas = crate::space::profile::list_all().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let running = crate::process::Tracker::shared().running();
    let by_id: std::collections::HashMap<String, crate::process::RunningProfile> =
        running.into_iter().map(|r| (r.profile_id.clone(), r)).collect();
    let out: Vec<Value> = metas
        .into_iter()
        .map(|m| {
            let r = by_id.get(&m.id);
            json!({
                "id": m.id,
                "name": m.name,
                "notes": m.notes,
                "proxy_id": m.proxy_id,
                "last_launched_at": m.last_launched_at,
                "created_at": m.created_at,
                "pinned": m.pinned,
                "folder": m.folder,
                "color": m.color,
                "extensions": m.extensions,
                "running": r.is_some(),
                "pid": r.map(|x| x.pid),
                "cdp": r.and_then(|x| x.cdp.clone()),
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn get_profile(Path(id): Path<String>) -> ApiResult {
    crate::space::ready().await;
    let stored = crate::space::profile::fetch(&id).await.map_err(fail(StatusCode::NOT_FOUND))?;
    let mut val = serde_json::to_value(stored)
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    if let Some(cdp) = crate::process::Tracker::shared().cdp(&id) {
        if let Some(obj) = val.as_object_mut() {
            obj.insert("running".into(), json!(true));
            obj.insert("cdp".into(), serde_json::to_value(cdp).unwrap_or(Value::Null));
        }
    }
    Ok(Json(val))
}

// ---- get-new-fingerprint ----

/// Uniquified fingerprint without persisting; create-profile stores verbatim.
async fn new_fingerprint() -> ApiResult {
    new_fingerprint_impl(None).await
}

async fn new_fingerprint_for(Path(platform): Path<String>) -> ApiResult {
    new_fingerprint_impl(Some(platform)).await
}

async fn new_fingerprint_impl(platform: Option<String>) -> ApiResult {
    let fid = random_fingerprint_for(platform.as_deref())?;
    let mut cfg = crate::build_fingerprint_config(crate::main_window().as_ref(), &fid)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    cfg.remove("_meta");
    Ok(Json(json!({ "fingerprint": cfg })))
}

// ---- create-profile ----

#[derive(Deserialize)]
struct CreateReq {
    name: Option<String>,
    notes: Option<String>,
    proxy_id: Option<String>,
    /// Proxy string: added to store + full-tested, bound by id.
    proxy: Option<String>,
    folder: Option<String>,
    /// Icon and omnibox-pill accent, `#rrggbb`. Omitted = derived from the name.
    color: Option<String>,
    /// Extension-library ids to load at launch (`GET /extensions`).
    extensions: Option<Vec<String>>,
    /// Claimed display refresh rate in Hz (24-480). Absent = the engine's 60.
    refresh_rate: Option<i64>,
    fingerprint: Value,
}

/// Persist verbatim (enrich=false); proxy_id binds, proxy string upserts+tests.
async fn persist_created(door: Door, t: &Ticket, folder_override: Option<String>, body: CreateReq) -> ApiResult {
    let mut cfg = body
        .fingerprint
        .as_object()
        .cloned()
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`fingerprint` must be an object"))?;
    cfg.remove("_meta");
    if let Some(n) = body.name.as_ref() {
        cfg.insert("name".into(), json!(n));
    }
    if let Some(n) = body.notes.as_ref() {
        cfg.insert("notes".into(), json!(n));
    }

    let folder = folder_override.or(body.folder).unwrap_or_default();
    // The profile's own door first: a refused one stores no proxy either.
    let made = space::profile::Made {
        proxy_id: body.proxy_id.as_deref(),
        inline_proxy: body.proxy.is_some(),
        extensions: body.extensions.as_ref().is_some_and(|e| !e.is_empty()),
    };
    space::profile::check_create(door, t, &folder, &made).map_err(fail(StatusCode::FORBIDDEN))?;
    let mut meta = json!({ "id": "", "folder": folder });
    if let Some(c) = body.color.as_ref().filter(|c| !c.is_empty()) {
        meta["color"] = json!(c);
    }
    if let Some(ids) = body.extensions.as_ref() {
        validate_extension_ids(ids)?;
        meta["extensions"] = json!(ids);
    }
    let mut inline_proxy = false;
    if let Some(pid) = body.proxy_id.as_ref() {
        meta["proxy_id"] = json!(pid);
    } else if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::space::proxy::parse_single(pstr)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unparseable proxy: {pstr}")))?;
        let stored = crate::space::proxy::upsert_dedup(Door("proxy_save"), t, entry)
            .await
            .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
        // Best-effort full test (UDP + geo); launch re-probes UDP live anyway.
        let _ = crate::space::proxy::full_test(&stored).await;
        meta["proxy_id"] = json!(stored.id);
        inline_proxy = true;
        crate::notify_store_changed("proxies");
    }
    crate::ensure_default_noise(&mut cfg);
    if let Some(hz) = body.refresh_rate {
        apply_refresh_rate(&mut cfg, hz).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    cfg.insert("_meta".into(), meta);

    let stored: space::profile::StoredProfile =
        serde_json::from_value(Value::Object(cfg)).map_err(fail(StatusCode::BAD_REQUEST))?;
    let opts = space::profile::SaveOpts { inline_proxy, ..Default::default() };
    let pm = space::profile::save(door, t, stored, opts).await.map_err(fail(StatusCode::BAD_REQUEST))?;
    crate::notify_store_changed("profiles");
    Ok(Json(serde_json::to_value(pm).unwrap_or(Value::Null)))
}

async fn create_profile(Extension(t): Extension<Ticket>, Json(body): Json<CreateReq>) -> ApiResult {
    persist_created(Door("POST /profiles"), &t, None, body).await
}

async fn create_profile_in_folder(
    Extension(t): Extension<Ticket>,
    Path(folder): Path<String>,
    Json(body): Json<CreateReq>,
) -> ApiResult {
    persist_created(Door("POST /folders/:folder/profiles"), &t, Some(folder), body).await
}

// ---- temporary profiles ----

#[derive(Deserialize)]
struct TempReq {
    fingerprint_id: Option<String>,
    platform: Option<String>,
    /// Inline proxy (not stored).
    proxy: Option<String>,
    name: Option<String>,
    folder: Option<String>,
    /// Per vector: `{"canvas": true}` or a full block. Omitted vectors stay off.
    noise: Option<Value>,
    /// Claimed display refresh rate in Hz (24-480). Absent = the engine's 60.
    refresh_rate: Option<i64>,
}

/// Temporary profile (hidden, auto-deleted on close); pair with /start.
async fn create_temporary(Extension(t): Extension<Ticket>, Json(body): Json<TempReq>) -> ApiResult {
    // Kept on this machine and started at once: not a team's yet.
    if t.team().is_some() {
        return Err(refusal(&CmdError::NotOffered(space::TEAM_NOT_YET)));
    }
    let fid = match body.fingerprint_id {
        Some(f) => f,
        None => random_fingerprint_for(body.platform.as_deref())?,
    };
    let mut cfg = crate::build_fingerprint_config(crate::main_window().as_ref(), &fid)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    cfg.remove("_meta");
    if let Some(n) = body.name.as_ref() {
        cfg.insert("name".into(), json!(n));
    }
    crate::ensure_default_noise(&mut cfg);
    if let Some(n) = body.noise.as_ref() {
        apply_noise_overrides(&mut cfg, n)
            .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    if let Some(hz) = body.refresh_rate {
        apply_refresh_rate(&mut cfg, hz).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    let mut meta = json!({ "id": "", "folder": body.folder.unwrap_or_default(), "temporary": true });
    if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::space::proxy::parse_single(pstr)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unparseable proxy: {pstr}")))?;
        meta["inline_proxy"] = serde_json::to_value(entry).unwrap_or(Value::Null);
    }
    cfg.insert("_meta".into(), meta);

    t.still().map_err(|e| refusal(&e))?;
    let pm = crate::save_profile_here(crate::main_window().as_ref(), Value::Object(cfg), false)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({
        "id": pm.id,
        "name": pm.name,
        "fingerprint_id": fid,
        "temporary": true,
        "proxy_inline": body.proxy.is_some(),
    })))
}

/// Write `screen.refresh_rate` into a config. Its own field rather than part of
/// `fingerprint` because it is the one piece of the screen block an operator
/// sets on its own: a page reads it by timing frames, not by asking.
fn apply_refresh_rate(cfg: &mut serde_json::Map<String, Value>, hz: i64) -> Result<(), String> {
    if !(24..=480).contains(&hz) {
        return Err(format!("refresh_rate must be between 24 and 480, got {hz}"));
    }
    let screen = cfg
        .entry("screen")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    match screen.as_object_mut() {
        Some(o) => {
            o.insert("refresh_rate".into(), json!(hz));
            Ok(())
        }
        None => Err("`screen` is not an object".into()),
    }
}

/// Merge a caller's noise request into the config's block. Merged, not
/// replaced: `{"webgl": true}` must not zero its intensity.
fn apply_noise_overrides(
    cfg: &mut serde_json::Map<String, Value>,
    req: &Value,
) -> Result<(), String> {
    const VECTORS: &[&str] = &["canvas", "webgl", "audio", "client_rects", "sensors", "fonts"];
    let req = req.as_object().ok_or("`noise` must be an object")?;
    let noise = cfg
        .get_mut("noise")
        .and_then(|n| n.as_object_mut())
        .ok_or("profile has no noise block")?;

    for (key, val) in req {
        let key = key.as_str();
        if !VECTORS.contains(&key) {
            return Err(format!("unknown noise vector `{key}`"));
        }
        let slot = noise
            .entry(key.to_string())
            .or_insert_with(|| json!({ "enabled": false, "seed": 0 }));
        let Some(slot) = slot.as_object_mut() else { continue };
        match val {
            Value::Bool(on) => {
                slot.insert("enabled".into(), json!(on));
                // The defaults the UI writes for the two vectors that carry a
                // strength; turning one on with a zero strength is a no-op.
                if *on && key == "webgl" && slot.get("intensity").and_then(|v| v.as_f64()) == Some(0.0) {
                    slot.insert("intensity".into(), json!(0.0005));
                }
                if *on && key == "client_rects" && slot.get("max_offset").and_then(|v| v.as_f64()) == Some(0.0) {
                    slot.insert("max_offset".into(), json!(1));
                }
            }
            Value::Object(fields) => {
                for (k, v) in fields {
                    slot.insert(k.clone(), v.clone());
                }
            }
            _ => return Err(format!("`noise.{key}` must be a bool or an object")),
        }
    }
    Ok(())
}

/// Into the trash, like the UI's delete: this machine's, or the team's on its
/// server. Temporary profiles are torn down by the Tracker and never reach it.
async fn delete_profile(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    space::profile::remove(Door("DELETE /profiles/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("profiles");
    Ok(Json(json!({ "deleted": true, "id": id, "trashed": true })))
}

#[derive(Deserialize)]
struct EditReq {
    name: Option<String>,
    notes: Option<String>,
    /// "" unfiles.
    folder: Option<String>,
    /// "" unbinds.
    proxy_id: Option<String>,
    /// Proxy string: stored + tested, then bound.
    proxy: Option<String>,
    /// `#rrggbb`; "" goes back to the name-derived colour.
    color: Option<String>,
    /// Replaces the whole list; `[]` loads none.
    extensions: Option<Vec<String>>,
    /// Claimed display refresh rate in Hz (24-480); applied after `fingerprint`.
    refresh_rate: Option<i64>,
    /// Replace stored fingerprint verbatim.
    fingerprint: Option<Value>,
    /// The `_meta.rev` read: refused as stale (409) once the profile moved past it.
    rev: Option<u64>,
}

/// Reject unknown ids rather than writing a profile that launches without them.
fn validate_extension_ids(ids: &[String]) -> Result<(), ApiError> {
    let known: std::collections::HashSet<String> = crate::space::extensions::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?
        .into_iter()
        .map(|e| e.id)
        .collect();
    let missing: Vec<&str> = ids
        .iter()
        .filter(|id| !known.contains(*id))
        .map(|s| s.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_REQUEST,
            format!("no such extension: {}", missing.join(", ")),
        ))
    }
}

/// Edit profile; only provided fields change. Returns the updated profile.
async fn edit_profile(Extension(t): Extension<Ticket>, Path(id): Path<String>, Json(body): Json<EditReq>) -> ApiResult {
    let door = Door("PATCH /profiles/:id");
    let mut stored = space::profile::fetch_for_edit(&t, &id).await.map_err(fail(StatusCode::NOT_FOUND))?;
    let mut inline_proxy = false;

    if let Some(fp) = body.fingerprint {
        let mut cfg = fp
            .as_object()
            .cloned()
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`fingerprint` must be an object"))?;
        cfg.remove("_meta");
        stored.config = cfg;
    }
    if let Some(n) = body.name.as_ref() {
        stored.config.insert("name".into(), json!(n));
    }
    if let Some(n) = body.notes.as_ref() {
        stored.config.insert("notes".into(), json!(n));
    }
    if let Some(pid) = body.proxy_id.as_ref() {
        stored.meta.proxy_id = if pid.is_empty() { None } else { Some(pid.clone()) };
        stored.meta.inline_proxy = None;
    } else if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::space::proxy::parse_single(pstr)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unparseable proxy: {pstr}")))?;
        // The profile's own door first: a refused edit stores no proxy either.
        space::profile::check_edit(door, &t, &id, true).map_err(fail(StatusCode::FORBIDDEN))?;
        // The profile's current proxy is passed in, so a line that differs
        // from it only by its password updates that entry rather than
        // leaving a dead one behind on every session change.
        let s = crate::space::proxy::upsert_for_binding(Door("proxy_save"), &t, entry, stored.meta.proxy_id.as_deref())
            .await
            .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
        let _ = crate::space::proxy::full_test(&s).await;
        stored.meta.proxy_id = Some(s.id);
        stored.meta.inline_proxy = None;
        inline_proxy = true;
        crate::notify_store_changed("proxies");
    }
    if let Some(c) = body.color.as_ref() {
        // "" is how a caller asks for the derived colour back.
        stored.meta.color = (!c.is_empty()).then(|| c.clone());
    }
    if let Some(ids) = body.extensions.as_ref() {
        validate_extension_ids(ids)?;
        stored.meta.extensions = ids.clone();
    }
    if let Some(hz) = body.refresh_rate {
        apply_refresh_rate(&mut stored.config, hz)
            .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }

    // Without a rev, the one just read: a change made in between is not undone.
    let opts = space::profile::SaveOpts {
        sent_rev: Some(body.rev.unwrap_or(stored.meta.rev)),
        move_to: body.folder.clone(),
        inline_proxy,
    };
    space::profile::save(door, &t, stored, opts).await.map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;

    let updated = space::profile::load_raw(&id).map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("profiles");
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
struct RenameFolderReq {
    name: String,
}

async fn rename_folder_ep(
    Extension(t): Extension<Ticket>,
    Path(folder): Path<String>,
    Json(body): Json<RenameFolderReq>,
) -> ApiResult {
    let n = space::profile::rename_folder(Door("PATCH /folders/:folder"), &t, &folder, &body.name)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("profiles");
    Ok(Json(json!({ "renamed_to": body.name, "profiles": n })))
}

#[derive(Deserialize)]
struct DeleteFolderQuery {
    /// true → delete profiles; false (default) → unfile.
    #[serde(default)]
    delete_profiles: bool,
}

async fn delete_folder_ep(
    Extension(t): Extension<Ticket>,
    Path(folder): Path<String>,
    Query(q): Query<DeleteFolderQuery>,
) -> ApiResult {
    let n = space::profile::delete_folder(Door("DELETE /folders/:folder"), &t, &folder, q.delete_profiles)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("profiles");
    Ok(Json(json!({
        "deleted_folder": folder,
        "delete_profiles": q.delete_profiles,
        "profiles": n,
    })))
}

#[derive(Deserialize, Default)]
struct StartReq {
    #[serde(default)]
    headless: bool,
}

/// Launch with CDP; body `{ "headless": true }` opt-in.
async fn start_profile(Path(id): Path<String>, body: Option<Json<StartReq>>) -> ApiResult {
    let headless = body.map(|Json(b)| b.headless).unwrap_or(false);
    let outcome = crate::launch::launch_profile(&id, true, headless)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({
        "profile_id": id,
        "pid": outcome.pid,
        "headless": headless,
        "cdp": outcome.cdp,
        "cdp_error": outcome.cdp_error,
    })))
}

async fn stop_profile(Path(id): Path<String>) -> ApiResult {
    // Only the space in use: another space's profile is not there for the API.
    if !space::profile::in_active_space(&id) {
        return Err(refusal(&CmdError::Missing(format!("no profile {id} here"))));
    }
    let stopped = crate::process::Tracker::shared()
        .kill(&id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "profile_id": id, "stopped": stopped })))
}

async fn export_cookies(Path(id): Path<String>) -> ApiResult {
    let pass = crate::space::perm::cookies(&id).map_err(fail(StatusCode::FORBIDDEN))?;
    let cookies = crate::space::cookies::export(&pass)
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "cookies": cookies })))
}

#[derive(Deserialize)]
struct ImportCookiesReq {
    cookies: Vec<crate::space::cookies::Cookie>,
}

async fn import_cookies(Path(id): Path<String>, Json(body): Json<ImportCookiesReq>) -> ApiResult {
    // Running browser would clobber imports on exit.
    if crate::is_profile_running(&id) {
        return Err(err(
            StatusCode::CONFLICT,
            "stop the profile before importing cookies",
        ));
    }
    let pass = crate::space::perm::cookies(&id).map_err(fail(StatusCode::FORBIDDEN))?;
    let n = crate::space::cookies::import(&pass, &body.cookies)
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "imported": n })))
}

/// The profiles of the space in use that are running.
async fn list_running() -> Json<Value> {
    Json(json!(space::running()))
}

async fn list_fingerprints() -> ApiResult {
    let all = crate::fingerprints::list_all()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let out: Vec<Value> = all
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id,
                "label": e.label,
                "platform": e.platform,
                "chrome": e.chrome,
                "gpu": e.gpu,
                "builtin": e.builtin,
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct AddProxyReq {
    /// "scheme://user:pass@host:port" or "host:port:user:pass"; wins over fields.
    proxy: Option<String>,
    /// socks5 | http | https (default socks5).
    kind: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    password: Option<String>,
    name: Option<String>,
    country: Option<String>,
    notes: Option<String>,
}

/// Add proxy (an identical one already stored is reused); returns summary.
async fn add_proxy(Extension(t): Extension<Ticket>, Json(body): Json<AddProxyReq>) -> ApiResult {
    let mut entry = if let Some(s) = body.proxy.as_ref() {
        crate::space::proxy::parse_single(s)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unparseable proxy: {s}")))?
    } else {
        let host = body
            .host
            .clone()
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`proxy` string or host+port required"))?;
        let port = body
            .port
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`port` required"))?;
        let kind = match body.kind.as_deref() {
            Some("http") => crate::space::proxy::ProxyKind::Http,
            Some("https") => crate::space::proxy::ProxyKind::Https,
            _ => crate::space::proxy::ProxyKind::Socks5,
        };
        crate::space::proxy::ProxyEntry {
            id: String::new(),
            name: String::new(),
            kind,
            host,
            port,
            username: body.username.clone().unwrap_or_default(),
            password: body.password.clone().unwrap_or_default(),
            country: String::new(),
            notes: String::new(),
        }
    };
    // metadata overrides (applied to parsed entries too).
    if let Some(n) = body.name.filter(|s| !s.is_empty()) {
        entry.name = n;
    }
    if let Some(c) = body.country {
        entry.country = c;
    }
    if let Some(nt) = body.notes {
        entry.notes = nt;
    }
    let stored = crate::space::proxy::upsert_dedup(Door("POST /proxies"), &t, entry)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("proxies");
    Ok(Json(json!({
        "id": stored.id,
        "name": stored.name,
        "kind": stored.kind,
        "host": stored.host,
        "port": stored.port,
        "country": stored.country,
    })))
}

async fn delete_proxy(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    space::proxy::delete(Door("DELETE /proxies/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("proxies");
    Ok(Json(json!({ "deleted": true, "id": id })))
}

async fn list_proxies() -> ApiResult {
    crate::space::ready().await;
    let list = crate::space::proxy::list().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    // Credentials never exposed over API.
    let out: Vec<Value> = list
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "kind": p.kind,
                "host": p.host,
                "port": p.port,
                "country": p.country,
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

// ---- extensions ----

fn extension_json(e: &crate::space::extensions::ExtensionEntry, with_icon: bool) -> Value {
    json!({
        "id": e.id,
        "name": e.name,
        "version": e.version,
        "description": e.description,
        "size_bytes": e.size_bytes,
        "added_at": e.added_at,
        // Data URLs run to hundreds of kilobytes each; a listing of thirty
        // extensions is not the place for them.
        "icon": with_icon.then(|| e.icon.clone()),
    })
}

async fn list_extensions() -> ApiResult {
    crate::space::ready().await;
    let list = crate::space::extensions::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let out: Vec<Value> = list.iter().map(|e| extension_json(e, false)).collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct AddExtensionReq {
    /// Web Store page, a bare extension id, or a direct .crx / .zip link.
    url: Option<String>,
    /// Local .crx / .zip file, or an unpacked folder, on this machine.
    path: Option<String>,
}

/// Add to the library. The response carries the icon, since the caller has
/// just asked for exactly this one.
async fn add_extension(Extension(t): Extension<Ticket>, Json(body): Json<AddExtensionReq>) -> ApiResult {
    let entry = match (body.url.as_deref(), body.path.as_deref()) {
        (Some(u), _) if !u.is_empty() => crate::space::extensions::import_url(Door("POST /extensions"), &t, u)
            .await
            .map_err(fail_all(StatusCode::BAD_REQUEST))?,
        (_, Some(p)) if !p.is_empty() => crate::space::extensions::import(Door("POST /extensions"), &t, std::path::Path::new(p))
            .await
            .map_err(fail_all(StatusCode::BAD_REQUEST))?,
        _ => return Err(err(StatusCode::BAD_REQUEST, "`url` or `path` required")),
    };
    crate::notify_store_changed("extensions");
    Ok(Json(extension_json(&entry, true)))
}

async fn delete_extension(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    crate::space::extensions::delete(Door("DELETE /extensions/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("extensions");
    Ok(Json(json!({ "deleted": true, "id": id })))
}

// ---- bookmarks ----

async fn list_bookmarks() -> ApiResult {
    crate::space::ready().await;
    let list = crate::space::bookmarks::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
struct BookmarkReq {
    /// Present = update that bookmark; absent = a new one.
    id: Option<String>,
    url: String,
    /// Blank uses the address itself.
    title: Option<String>,
    /// Launcher folder; "" (or absent) means every profile.
    folder: Option<String>,
}

/// Upsert. Profiles pick the change up on their next launch, not immediately —
/// the bookmarks file is written just before the browser reads it.
async fn save_bookmark(Extension(t): Extension<Ticket>, Json(body): Json<BookmarkReq>) -> ApiResult {
    let mark = crate::space::bookmarks::Bookmark {
        id: body.id.unwrap_or_default(),
        title: body.title.unwrap_or_default(),
        url: body.url,
        folder: body.folder.unwrap_or_default(),
    };
    let saved = crate::space::bookmarks::save(Door("POST /bookmarks"), &t, mark)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    crate::notify_store_changed("bookmarks");
    Ok(Json(serde_json::to_value(saved).unwrap_or(Value::Null)))
}

async fn delete_bookmark(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    crate::space::bookmarks::delete(Door("DELETE /bookmarks/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("bookmarks");
    Ok(Json(json!({ "deleted": true, "id": id })))
}

// ---- trash ----

async fn list_trash() -> ApiResult {
    let list = crate::space::trash::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

/// Puts the profile back under its own id, so anything that referenced it
/// still points at it.
async fn restore_trash(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    let meta = crate::space::trash::restore(&t, &id)
        .map_err(fail(StatusCode::NOT_FOUND))?;
    crate::notify_store_changed("profiles");
    Ok(Json(serde_json::to_value(meta).unwrap_or(Value::Null)))
}

/// Deletes the archive for good. There is nothing after this.
async fn purge_trash(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    crate::space::trash::purge(&t, &id)
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("trash");
    Ok(Json(json!({ "purged": true, "id": id })))
}

async fn list_folders() -> ApiResult {
    crate::space::ready().await;
    let metas = crate::space::profile::list_all().map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let mut set = std::collections::BTreeSet::new();
    for m in metas {
        if !m.folder.is_empty() {
            set.insert(m.folder);
        }
    }
    Ok(Json(json!(set.into_iter().collect::<Vec<_>>())))
}

/// Normalize platform string to library tag vocabulary.
fn normalize_platform(p: &str) -> String {
    match p.trim().to_lowercase().as_str() {
        "windows" | "win" => "Windows".into(),
        "linux" => "Linux".into(),
        "mac" | "macos" | "osx" | "darwin" => "macOS".into(),
        "android" | "phone" | "mobile" => "Android".into(),
        other => other.to_string(),
    }
}

/// Random fingerprint id for platform (host OS when None); falls back to all.
fn random_fingerprint_for(platform: Option<&str>) -> Result<String, ApiError> {
    let want = platform
        .map(normalize_platform)
        .unwrap_or_else(crate::host_platform);
    let all = crate::fingerprints::list_all()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    if all.is_empty() {
        return Err(err(StatusCode::NOT_FOUND, "fingerprint library is empty"));
    }
    // Library entries keep the template's navigator.platform, which is "Android"
    // for a phone; the OS name alone matched none of them and the caller got a
    // desktop profile instead. A "Linux arm…" platform is a phone too.
    let phone = want == "Android";
    let matches = |e: &crate::fingerprints::LibraryEntry| {
        if phone {
            e.platform.eq_ignore_ascii_case("android") || e.platform.to_ascii_lowercase().starts_with("linux arm")
        } else {
            e.platform.eq_ignore_ascii_case(&want)
        }
    };
    let matching: Vec<crate::fingerprints::LibraryEntry> = all.iter().filter(|e| matches(e)).cloned().collect();
    let pool = if matching.is_empty() { all } else { matching };
    let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % pool.len();
    Ok(pool[idx].id.clone())
}

// ---- automation ----
//
// Storage answers in every build; running only where the `automation` feature
// is compiled in.

async fn list_projects() -> ApiResult {
    crate::space::ready().await;
    let list = crate::space::automation::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
struct CreateProjectReq {
    #[serde(default)]
    name: String,
}

async fn create_project(Extension(t): Extension<Ticket>, body: Option<Json<CreateProjectReq>>) -> ApiResult {
    let name = body.map(|Json(b)| b.name).unwrap_or_default();
    let project = crate::space::automation::create(Door("POST /automation/projects"), &t, &name)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("automation");
    Ok(Json(serde_json::to_value(project).unwrap_or(Value::Null)))
}

fn find_project(id: &str) -> Result<crate::space::automation::Project, ApiError> {
    crate::space::automation::list()
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| refusal(&CmdError::Missing("no such project".into())))
}

async fn get_project(Path(id): Path<String>) -> ApiResult {
    crate::space::ready().await;
    Ok(Json(serde_json::to_value(find_project(&id)?).unwrap_or(Value::Null)))
}

/// Whole-project replace. The id in the path wins over the body's, and an
/// unknown id is a 404 rather than a silent create.
async fn save_project(Extension(t): Extension<Ticket>, Path(id): Path<String>, Json(mut body): Json<Value>) -> ApiResult {
    find_project(&id)?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("id".into(), Value::String(id.clone()));
    }
    let project: crate::space::automation::Project = serde_json::from_value(body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("that is not a project: {e}")))?;
    let saved = crate::space::automation::save(Door("PUT /automation/projects/:id"), &t, project)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("automation");
    Ok(Json(serde_json::to_value(saved).unwrap_or(Value::Null)))
}

async fn delete_project(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    find_project(&id)?;
    crate::space::automation::delete(Door("DELETE /automation/projects/:id"), &t, &id)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    crate::notify_store_changed("automation");
    Ok(Json(json!({ "deleted": true, "id": id })))
}

async fn duplicate_project(Extension(t): Extension<Ticket>, Path(id): Path<String>) -> ApiResult {
    find_project(&id)?;
    let copy = crate::space::automation::duplicate(Door("POST /automation/projects/:id/duplicate"), &t, &id)
        .await
        .map_err(fail(StatusCode::NOT_FOUND))?;
    crate::notify_store_changed("automation");
    Ok(Json(serde_json::to_value(copy).unwrap_or(Value::Null)))
}

/// Export strips every parameter marked secret, so a bundle carries no passwords.
async fn export_project(Path(id): Path<String>) -> ApiResult {
    let bundle = crate::space::automation::export(&id)
        .map_err(fail(StatusCode::NOT_FOUND))?;
    Ok(Json(serde_json::to_value(bundle).unwrap_or(Value::Null)))
}

async fn import_project(Extension(t): Extension<Ticket>, Json(body): Json<Value>) -> ApiResult {
    let bundle: crate::space::automation::Bundle = serde_json::from_value(body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("that is not a project bundle: {e}")))?;
    let project = crate::space::automation::import(Door("POST /automation/import"), &t, bundle)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    crate::notify_store_changed("automation");
    Ok(Json(serde_json::to_value(project).unwrap_or(Value::Null)))
}

/// Answers as soon as the run is under way; progress comes from `/status`.
async fn run_project(Path(id): Path<String>) -> ApiResult {
    #[cfg(feature = "automation")]
    {
        find_project(&id)?;
        crate::runner::start(&id)
            .await
            .map_err(fail(StatusCode::BAD_REQUEST))?;
        return Ok(Json(json!({ "project_id": id, "running": true })));
    }
    #[cfg(not(feature = "automation"))]
    {
        let _ = id;
        Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
    }
}

/// Asks the run to stop. The browsers it started close on their own once the
/// step they are in finishes, so a run does not vanish the instant this answers.
async fn stop_project(Path(id): Path<String>) -> ApiResult {
    #[cfg(feature = "automation")]
    {
        crate::runner::stop(&id);
        return Ok(Json(json!({ "project_id": id, "stopping": true })));
    }
    #[cfg(not(feature = "automation"))]
    {
        let _ = id;
        Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
    }
}

/// The run's state, or null when the project is not running and has not run
/// since the launcher started.
async fn project_status(Path(id): Path<String>) -> ApiResult {
    #[cfg(feature = "automation")]
    {
        return Ok(Json(
            serde_json::to_value(crate::runner::status(&id)).unwrap_or(Value::Null),
        ));
    }
    #[cfg(not(feature = "automation"))]
    {
        let _ = id;
        Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
    }
}

/// Every run going right now — what the fleet window shows.
async fn list_runs() -> ApiResult {
    #[cfg(feature = "automation")]
    {
        return Ok(Json(serde_json::to_value(crate::runner::all()).unwrap_or(Value::Null)));
    }
    #[cfg(not(feature = "automation"))]
    Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
}

async fn list_modules() -> ApiResult {
    #[cfg(feature = "automation")]
    {
        let list = crate::wasm::list()
            .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
        return Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)));
    }
    #[cfg(not(feature = "automation"))]
    Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
}

#[derive(Deserialize)]
struct InstallModuleReq {
    path: String,
}

/// `path` is a .wasm file on this machine; the API is local and a module is
/// native code the operator built themselves.
async fn install_module(Json(body): Json<InstallModuleReq>) -> ApiResult {
    #[cfg(feature = "automation")]
    {
        if body.path.trim().is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "`path` required"));
        }
        let info = crate::wasm::install(body.path.trim())
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
        return Ok(Json(serde_json::to_value(info).unwrap_or(Value::Null)));
    }
    #[cfg(not(feature = "automation"))]
    {
        let _ = body.path;
        Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
    }
}

async fn remove_module(Path(id): Path<String>) -> ApiResult {
    #[cfg(feature = "automation")]
    {
        crate::wasm::remove(&id)
            .map_err(fail(StatusCode::NOT_FOUND))?;
        return Ok(Json(json!({ "deleted": true, "id": id })));
    }
    #[cfg(not(feature = "automation"))]
    {
        let _ = id;
        Err(err(StatusCode::NOT_IMPLEMENTED, "automation is not compiled into this build"))
    }
}

// ---- server ----

pub async fn serve(secret: String, port: u16) {
    set_secret(&secret);
    let app = router();

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            eprintln!("[launcher] automation API listening on http://{addr}");
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("[launcher] API server error: {e}");
            }
        }
        Err(e) => eprintln!("[launcher] API bind {addr} failed: {e}"),
    }
}

/// Every route, assembled. Split out so a test can build it: axum only catches
/// two routes on one path at construction, and that is a panic at startup.
fn router() -> Router {
    let protected = Router::new()
        .route("/profiles", get(list_profiles).post(create_profile))
        .route("/profiles/temporary", post(create_temporary))
        .route("/profiles/:id", get(get_profile).patch(edit_profile).delete(delete_profile))
        .route("/profiles/:id/start", post(start_profile))
        .route("/profiles/:id/stop", post(stop_profile))
        .route("/profiles/:id/cookies", get(export_cookies).post(import_cookies))
        .route("/folders", get(list_folders))
        .route("/folders/:folder", patch(rename_folder_ep).delete(delete_folder_ep))
        .route("/folders/:folder/profiles", post(create_profile_in_folder))
        .route("/fingerprint/new", get(new_fingerprint))
        .route("/fingerprint/new/:platform", get(new_fingerprint_for))
        .route("/fingerprints", get(list_fingerprints))
        .route("/running", get(list_running))
        .route("/proxies", get(list_proxies).post(add_proxy))
        .route("/proxies/:id", delete(delete_proxy))
        .route("/extensions", get(list_extensions).post(add_extension))
        .route("/extensions/:id", delete(delete_extension))
        .route("/bookmarks", get(list_bookmarks).post(save_bookmark))
        .route("/bookmarks/:id", delete(delete_bookmark))
        .route("/trash", get(list_trash))
        .route("/trash/:id", delete(purge_trash))
        .route("/trash/:id/restore", post(restore_trash))
        .route("/automation/projects", get(list_projects).post(create_project))
        .route(
            "/automation/projects/:id",
            get(get_project).put(save_project).delete(delete_project),
        )
        .route("/automation/projects/:id/duplicate", post(duplicate_project))
        .route("/automation/projects/:id/export", get(export_project))
        .route("/automation/projects/:id/run", post(run_project))
        .route("/automation/projects/:id/stop", post(stop_project))
        .route("/automation/projects/:id/status", get(project_status))
        .route("/automation/import", post(import_project))
        .route("/automation/runs", get(list_runs))
        .route("/automation/modules", get(list_modules).post(install_module))
        .route("/automation/modules/:id", delete(remove_module))
        .route_layer(middleware::from_fn(in_space))
        .route_layer(middleware::from_fn(auth));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(middleware::from_fn(unless_locked))
}

/// While the data is being moved or reset (the lock in migrate.rs), every
/// route answers 503: nothing starts, and nothing is written into folders on
/// their way out.
async fn unless_locked(req: Request, next: Next) -> Response {
    if crate::migrate::in_progress() {
        return err(StatusCode::SERVICE_UNAVAILABLE, crate::launch::LOCKED).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Two routes on one path panic when the router is built, so building it
    /// is the whole test.
    #[test]
    fn router_builds() {
        let _ = router();
    }

    #[tokio::test]
    async fn automation_needs_a_token() {
        let _serial = crate::migrate::serial();
        for (method, path) in [
            ("GET", "/automation/projects"),
            ("GET", "/automation/runs"),
            ("POST", "/automation/projects/x/run"),
            ("GET", "/automation/modules"),
        ] {
            let req = Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap();
            let res = router().oneshot(req).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} answered without a token"
            );
        }
    }

    /// `/health` is the one route that stays open, for probing before a token.
    #[tokio::test]
    async fn health_stays_open() {
        let _serial = crate::migrate::serial();
        let req = Request::builder().uri("/health").body(Body::empty()).unwrap();
        let res = router().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// I4: every route, open or not, answers 503 while the lock is held.
    #[tokio::test]
    async fn every_route_waits_while_the_data_lock_is_held() {
        let _serial = crate::migrate::serial();
        assert!(crate::migrate::hold());
        let mut answers = Vec::new();
        for (method, path) in [("GET", "/health"), ("GET", "/profiles"), ("POST", "/profiles/x/start")] {
            let req = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
            answers.push(router().oneshot(req).await.unwrap().status());
        }
        crate::migrate::release();
        assert!(answers.iter().all(|s| *s == StatusCode::SERVICE_UNAVAILABLE), "{answers:?}");
    }

    /// One request with the API's token; the answer and the edition it is about.
    async fn authed(method: &str, path: &str, body: Option<Value>) -> (StatusCode, Option<String>, Value) {
        set_secret("api-personal-test");
        let token = mint("api-personal-test", 600).unwrap();
        let req = Request::builder().method(method).uri(path).header(AUTHORIZATION, format!("Bearer {token}"));
        let req = match body {
            Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = router().oneshot(req).await.unwrap();
        let status = res.status();
        let epoch = res.headers().get(EPOCH_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, epoch, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn the_personal_space_writes_as_before_and_says_its_edition() {
        let _serial = crate::migrate::serial();
        let _root = crate::store::test_root("api-personal").with_config();
        let (status, epoch, made) = authed("POST", "/profiles", Some(json!({ "name": "A", "fingerprint": {} }))).await;
        assert_eq!(status, StatusCode::OK, "{made}");
        assert_eq!(epoch, Some(crate::team::space_epoch().to_string()));
        let id = made["id"].as_str().unwrap().to_string();
        let path = format!("/profiles/{id}");
        let (_, _, got) = authed("GET", &path, None).await;
        let rev = got["_meta"]["rev"].as_u64().unwrap();

        let (status, _, got) = authed("PATCH", &path, Some(json!({ "name": "B", "rev": rev, "folder": "F" }))).await;
        assert_eq!((status, got["name"].as_str(), got["_meta"]["folder"].as_str()), (StatusCode::OK, Some("B"), Some("F")));
        let (status, _, got) = authed("PATCH", &path, Some(json!({ "name": "C", "rev": rev }))).await;
        assert_eq!((status, got["code"].as_str()), (StatusCode::CONFLICT, Some("stale")), "{got}");

        let (status, _, got) = authed("POST", "/proxies", Some(json!({ "proxy": "203.0.113.8:1080" }))).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        let proxy = format!("/proxies/{}", got["id"].as_str().unwrap());
        assert_eq!(authed("DELETE", &proxy, None).await.0, StatusCode::OK);
        assert_eq!(authed("DELETE", &proxy, None).await.0, StatusCode::NOT_FOUND);

        let (status, _, got) = authed("DELETE", &path, None).await;
        assert_eq!((status, got["trashed"].as_bool()), (StatusCode::OK, Some(true)), "{got}");
        let (status, _, got) = authed("GET", &path, None).await;
        assert_eq!((status, got["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
        let (_, _, trash) = authed("GET", "/trash", None).await;
        assert_eq!(trash.as_array().map(Vec::len), Some(1), "{trash}");
        // A write sent under another edition is refused before it reads anything.
        let req = Request::builder()
            .method("POST")
            .uri("/profiles")
            .header(AUTHORIZATION, format!("Bearer {}", mint("api-personal-test", 600).unwrap()))
            .header(EPOCH_HEADER, (crate::team::space_epoch() + 1).to_string())
            .header("content-type", "application/json")
            .body(Body::from(json!({ "name": "D", "fingerprint": {} }).to_string()))
            .unwrap();
        assert_eq!(router().oneshot(req).await.unwrap().status(), StatusCode::CONFLICT);
        let (_, _, all) = authed("GET", "/profiles", None).await;
        assert_eq!(all, json!([]));
    }

    /// The local API in a team's space, against a real team server in-process.
    mod in_a_team {
        use super::*;
        use crate::space::client::{Method, TeamClient};
        use crate::space::{Door, Ticket};
        use crate::team::{self, Space};
        use base64::Engine as _;
        use shardbrowser_team_server::{Action, Grant, Scope, TestServer};

        fn allow(scope: Scope, action: Action) -> Grant {
            Grant { scope, action, allow: true }
        }

        fn folder(name: &str) -> Scope {
            Scope::Folder { name: name.into() }
        }

        fn b64(v: Value) -> String {
            base64::engine::general_purpose::STANDARD.encode(v.to_string())
        }

        /// A team server, its admin, and a member joined from here and in its space.
        struct Team {
            // Stopped before its data goes with the root.
            srv: TestServer,
            _root: crate::store::TestRoot,
            team_id: String,
            member_id: String,
            token: String,
            // Switching waits on launches and data moves, which other tests make.
            _serial: std::sync::MutexGuard<'static, ()>,
        }

        impl Team {
            async fn new(grants: &[Grant]) -> Team {
                let serial = crate::migrate::serial();
                let root = crate::store::test_root("api-team").with_config();
                let srv = TestServer::start(root.path.join("server")).await.unwrap();
                let member = srv.add_member("Vera", grants).unwrap();
                set_secret("api-team-test");
                let token = mint("api-team-test", 600).unwrap();
                Team {
                    team_id: srv.server.team_id().to_string(),
                    member_id: member.id.clone(),
                    token,
                    srv,
                    _root: root,
                    _serial: serial,
                }
                .joined(&member.token)
                .await
            }

            /// The same, joined with the admin's token: every right, and the folders.
            async fn as_admin() -> Team {
                let serial = crate::migrate::serial();
                let root = crate::store::test_root("api-team-admin").with_config();
                let srv = TestServer::start(root.path.join("server")).await.unwrap();
                set_secret("api-team-test");
                let token = mint("api-team-test", 600).unwrap();
                let admin = srv.admin_token.clone();
                Team { team_id: srv.server.team_id().to_string(), member_id: String::new(), token, srv, _root: root, _serial: serial }
                    .joined(&admin)
                    .await
            }

            async fn joined(self, member_token: &str) -> Team {
                team::join(&self.srv.url, member_token).await.unwrap();
                self
            }

            /// Into the team, its lists read afresh.
            async fn enter(&self) {
                crate::space::cache::keep_only(None);
                crate::space::switch(Space::Team { team_id: self.team_id.clone() }).await.unwrap();
            }

            fn admin(&self) -> TeamClient {
                TeamClient::new(&self.srv.url, &self.srv.admin_token, "admin-device").unwrap()
            }

            async fn admin_call(&self, method: Method, path: &str, body: Option<Value>) -> Value {
                let (_, bytes) = self.admin().call(method, path, body.as_ref()).await.unwrap();
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            }

            async fn folder(&self, name: &str) {
                self.admin_call(Method::POST, "/admin/folders", Some(json!({ "name": name }))).await;
            }

            async fn profile(&self, id: &str, folder: &str, name: &str, settings: Value) {
                let body = json!({
                    "name": name, "folder": folder, "data": b64(settings),
                    "card": { "notes": "notes-secret-9", "tags": ["tag-secret-2"] },
                });
                self.admin_call(Method::PUT, &format!("/profiles/{id}"), Some(body)).await;
            }

            /// The profile as the admin reads it.
            async fn on_server(&self, id: &str) -> Value {
                self.admin_call(Method::GET, &format!("/profiles/{id}"), None).await
            }

            async fn set_grants(&self, grants: &[Grant]) {
                let path = format!("/admin/members/{}/grants", self.member_id);
                self.admin_call(Method::PUT, &path, Some(json!({ "grants": grants }))).await;
                // As the live channel would: the new permission, taken now.
                let issued = TeamClient::for_team(&self.team_id).unwrap().permission().await.unwrap();
                crate::space::perm::accept(&self.team_id, &issued.permission).unwrap();
            }

            /// Everything the team holds, as its admin sees it.
            async fn everything(&self) -> Value {
                let mut all = json!({
                    "profiles": self.admin_call(Method::GET, "/profiles", None).await,
                    "proxies": self.admin_call(Method::GET, "/proxies", None).await,
                    "folders": self.admin_call(Method::GET, "/folders", None).await,
                    "trash": self.admin_call(Method::GET, "/trash", None).await,
                });
                for kind in ["bookmark", "extension", "automation", "tag"] {
                    all[kind] = self.admin_call(Method::GET, &format!("/items/{kind}"), None).await;
                }
                all
            }

            async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
                let (status, _, body) = self.call_with(method, path, body, None).await;
                (status, body)
            }

            /// One request through the whole router; `epoch` is sent as X-Space-Epoch.
            async fn call_with(
                &self,
                method: &str,
                path: &str,
                body: Option<Value>,
                epoch: Option<u64>,
            ) -> (StatusCode, Option<u64>, Value) {
                let mut req = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token));
                if let Some(e) = epoch {
                    req = req.header(EPOCH_HEADER, e.to_string());
                }
                let req = match body {
                    Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
                    None => req.body(Body::empty()),
                }
                .unwrap();
                let res = router().oneshot(req).await.unwrap();
                let status = res.status();
                let epoch = res.headers().get(EPOCH_HEADER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
                let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
                (status, epoch, body)
            }
        }

        /// A profile of this machine's own, from the personal space.
        fn mine(id: &str) -> std::path::PathBuf {
            let path = crate::store::profiles_dir().unwrap().join(format!("{id}.json"));
            std::fs::write(&path, json!({ "_meta": { "id": id }, "name": "Mine" }).to_string()).unwrap();
            path
        }

        /// An id for each kind of thing a route names, and a body a caller would send.
        fn request_for(door: &str) -> (String, String, Option<Value>) {
            let (method, path) = door.split_once(' ').unwrap();
            let id = if path.starts_with("/proxies") {
                "team-x-1"
            } else if path.starts_with("/extensions") {
                "team-e-1"
            } else if path.starts_with("/bookmarks") {
                "team-b-1"
            } else if path.starts_with("/automation/projects") {
                "team-a-1"
            } else if path.starts_with("/automation/modules") {
                "m-1"
            } else {
                "team-p-1"
            };
            let uri = path.replace(":id", id).replace(":folder", "Folder-secret-31").replace(":platform", "windows");
            let body = match door {
                "POST /profiles" | "POST /folders/:folder/profiles" => Some(json!({ "name": "x", "fingerprint": {} })),
                "POST /profiles/temporary" | "POST /profiles/:id/start" | "POST /automation/import" => Some(json!({})),
                "PATCH /profiles/:id" => Some(json!({ "name": "y" })),
                "POST /profiles/:id/cookies" => Some(json!({ "cookies": [] })),
                "PATCH /folders/:folder" => Some(json!({ "name": "Renamed" })),
                "POST /proxies" => Some(json!({ "proxy": "203.0.113.9:1080" })),
                "POST /extensions" => Some(json!({ "path": "/nonexistent/ext.zip" })),
                "POST /bookmarks" => Some(json!({ "url": "https://x.example" })),
                "POST /automation/projects" | "PUT /automation/projects/:id" => Some(json!({ "name": "p" })),
                "POST /automation/modules" => Some(json!({ "path": "/nonexistent/m.wasm" })),
                _ => None,
            };
            (method.to_string(), uri, body)
        }

        #[tokio::test]
        async fn a_member_with_no_grants_gets_nothing() {
            let t = Team::new(&[]).await;
            t.folder("Folder-secret-31").await;
            let settings = json!({ "navigator": { "user_agent": "UA-secret-55" } });
            t.profile("team-p-1", "Folder-secret-31", "name-secret-7", settings).await;
            let proxy = json!({ "name": "proxy-secret-4", "kind": "socks5", "data": b64(json!({ "host": "203.0.113.77", "port": 1080 })) });
            t.admin_call(Method::PUT, "/proxies/team-x-1", Some(proxy)).await;
            for (kind, id, name, data) in [
                ("bookmark", "team-b-1", "mark", json!({ "url": "https://mark-secret-6.example" })),
                ("extension", "team-e-1", "ext-secret-8", json!({ "version": "1" })),
                ("automation", "team-a-1", "project-secret-3", json!({})),
                ("tag", "team-t-1", "tag-name-secret-1", json!({ "color": "red" })),
            ] {
                let body = json!({ "name": name, "data": b64(data) });
                t.admin_call(Method::PUT, &format!("/items/{kind}/{id}"), Some(body)).await;
            }
            let own = mine("mine-1");
            t.enter().await;
            let before = t.everything().await;
            let secrets = [
                "Folder-secret-31", "name-secret-7", "UA-secret-55", "notes-secret-9", "tag-secret-2",
                "proxy-secret-4", "203.0.113.77", "mark-secret-6", "ext-secret-8", "project-secret-3",
                "tag-name-secret-1", "Mine",
            ];

            let doors = crate::space::doors::routes(include_str!("api.rs"));
            assert!(doors.len() > 40, "{doors:?}");
            for door in &doors {
                let (method, uri, body) = request_for(door);
                let (status, got) = t.call(&method, &uri, body).await;
                let text = got.to_string();
                eprintln!("{door}: {status} {text}");
                assert_ne!(status, StatusCode::UNAUTHORIZED, "{door}");
                for s in secrets {
                    assert!(!text.contains(s), "{door} told {s}: {status} {text}");
                }
                if crate::space::doors::is_open(door) {
                    continue;
                }
                let empty = got == json!([]) || got.is_null();
                assert!(!status.is_success() || empty, "{door} answered {status}: {text}");
                assert!(!status.is_server_error() || status == StatusCode::NOT_IMPLEMENTED, "{door}: {status} {text}");
            }

            // What is there is invisible; what would be made is refused, with what it needs.
            let (status, got) = t.call("GET", "/profiles/team-p-1", None).await;
            assert_eq!((status, got["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
            let (status, got) = t.call("DELETE", "/proxies/team-x-1", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{got}");
            let (status, got) = t.call("POST", "/profiles", Some(json!({ "name": "x", "fingerprint": {} }))).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!((got["code"].as_str(), got["need"].as_str(), got["on"].as_str()), (Some("forbidden"), Some("create"), Some("folder")));
            let (status, got) = t.call("POST", "/proxies", Some(json!({ "proxy": "203.0.113.9:1080" }))).await;
            assert_eq!((status, got["need"].as_str(), got["on"].as_str()), (StatusCode::FORBIDDEN, Some("create"), Some("team")));
            let (status, got) = t.call("PATCH", "/folders/Folder-secret-31", Some(json!({ "name": "R" }))).await;
            assert_eq!((status, got["need"].as_str()), (StatusCode::FORBIDDEN, Some("admin")));
            // The personal space's own profile is another space's: not there either.
            let (status, _) = t.call("GET", "/profiles/mine-1", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let (status, _) = t.call("DELETE", "/profiles/mine-1", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(own.is_file());

            assert_eq!(t.everything().await, before, "a refused write reached the team");
            drop(t.srv);
        }

        #[tokio::test]
        async fn a_write_from_an_old_revision_is_stale_and_one_under_a_lease_is_leased() {
            let t = Team::new(&[allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Edit)]).await;
            t.folder("Ads").await;
            t.profile("p-1", "Ads", "First", json!({ "navigator": { "user_agent": "UA" } })).await;
            t.enter().await;
            let (status, got) = t.call("GET", "/profiles/p-1", None).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            let read_at = got["_meta"]["rev"].as_u64().unwrap();

            // Saved by someone else meanwhile.
            t.profile("p-1", "Ads", "Theirs", json!({ "navigator": { "user_agent": "UA" } })).await;
            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "name": "Mine", "rev": read_at }))).await;
            assert_eq!(status, StatusCode::CONFLICT, "{got}");
            assert_eq!(got["code"], "stale");
            assert_eq!(got["current_revision"].as_u64(), Some(read_at + 1));
            assert_eq!(t.on_server("p-1").await["name"], "Theirs");

            // The editor's save reads as its conflict.
            let tk = Ticket::now();
            let mut old = crate::space::profile::fetch_for_edit(&tk, "p-1").await.unwrap();
            old.config.insert("name".into(), json!("From the editor"));
            let opts = crate::space::profile::SaveOpts { sent_rev: Some(read_at), ..Default::default() };
            let e = crate::space::profile::save(Door("profile_save"), &tk, old, opts).await.unwrap_err();
            assert!(e.to_string().contains("changed after you opened it"), "{e}");

            // From what is there now, it goes through.
            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "name": "Mine" }))).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            assert_eq!(t.on_server("p-1").await["name"], "Mine");
            let listed = crate::space::profile::list_all().unwrap();
            assert_eq!(listed[0].name, "Mine", "held here at once");

            // Open on another device: refused, with where.
            t.admin().call(Method::POST, "/profiles/p-1/lease", None).await.unwrap();
            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "name": "Again" }))).await;
            assert_eq!((status, got["code"].as_str()), (StatusCode::CONFLICT, Some("leased")), "{got}");
            assert_eq!(got["device_name"].as_str(), Some(team::device_name().as_str()));
            assert_eq!(t.on_server("p-1").await["name"], "Mine");
            drop(t.srv);
        }

        #[tokio::test]
        async fn moving_a_profile_needs_create_where_it_lands() {
            let ads = [allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Edit)];
            let t = Team::new(&[ads[0].clone(), ads[1].clone(), allow(folder("Shops"), Action::See)]).await;
            for f in ["Ads", "Shops"] {
                t.folder(f).await;
            }
            t.profile("p-1", "Ads", "First", json!({})).await;
            t.enter().await;

            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "folder": "Shops" }))).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{got}");
            assert_eq!((got["code"].as_str(), got["need"].as_str(), got["on"].as_str()), (Some("forbidden"), Some("create"), Some("folder")));
            let moved = crate::space::profile::set_folder(Door("profile_set_folder"), &Ticket::now(), "p-1", "Shops").await;
            let refused = crate::space::error::find(moved.as_ref().unwrap_err()).cloned();
            assert!(matches!(refused, Some(CmdError::Forbidden(n)) if n.right == "create"), "{moved:?}");
            let there = t.on_server("p-1").await;
            assert_eq!((there["folder"].as_str(), there["revision"].as_u64()), (Some("Ads"), Some(1)));

            // Create there, but more rights than here: the server refuses the move.
            let mut wider = ads.to_vec();
            wider.extend([Action::See, Action::Create, Action::Delete].map(|a| allow(folder("Shops"), a)));
            t.set_grants(&wider).await;
            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "folder": "Shops" }))).await;
            assert_eq!((status, got["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")), "{got}");
            assert_eq!(t.on_server("p-1").await["folder"], "Ads");

            // Create there and the same rights: it moves.
            let mut same = ads.to_vec();
            same.extend([Action::See, Action::Edit, Action::Create].map(|a| allow(folder("Shops"), a)));
            t.set_grants(&same).await;
            let (status, got) = t.call("PATCH", "/profiles/p-1", Some(json!({ "folder": "Shops" }))).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            assert_eq!(t.on_server("p-1").await["folder"], "Shops");
            drop(t.srv);
        }

        #[tokio::test]
        async fn a_member_with_the_rights_writes_each_kind_of_change_to_the_team() {
            let all = [Action::See, Action::Start, Action::Edit, Action::Create, Action::Delete];
            let t = Team::new(&all.map(|a| allow(Scope::Team, a))).await;
            t.folder("Ads").await;
            t.enter().await;
            let tk = Ticket::now();

            // A proxy, then a profile bound to it.
            let entry = crate::space::proxy::parse_single("203.0.113.5:1080:user:pass").unwrap();
            let x = crate::space::proxy::upsert(Door("proxy_save"), &tk, entry).await.unwrap();
            let theirs = t.admin_call(Method::GET, &format!("/proxies/{}", x.id), None).await;
            assert_eq!(theirs["name"], "203.0.113.5:1080");
            assert_eq!(crate::space::proxy::get(&x.id).unwrap().unwrap().password, "pass");
            let body = json!({ "name": "Shop", "folder": "Ads", "proxy_id": x.id, "notes": "n1", "fingerprint": { "noise": { "canvas": { "seed": 0 } } } });
            let (status, made) = t.call("POST", "/profiles", Some(body)).await;
            assert_eq!(status, StatusCode::OK, "{made}");
            let id = made["id"].as_str().unwrap().to_string();
            let there = t.on_server(&id).await;
            assert_eq!((there["folder"].as_str(), there["card"]["proxy_id"].as_str()), (Some("Ads"), Some(x.id.as_str())));
            assert_eq!(there["card"]["notes"], "n1");
            let settings: Value = serde_json::from_slice(
                &base64::engine::general_purpose::STANDARD.decode(there["data"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
            assert_ne!(settings["noise"]["canvas"]["seed"], 0, "seeded by its id");
            assert!(settings["_meta"].get("pinned").is_none_or(|p| p == false));

            // Unbound, tagged, moved out of its folder: each from the revision there.
            crate::space::profile::bind_proxy(Door("profile_bind_proxy"), &tk, &id, None).await.unwrap();
            let tags = [(id.clone(), vec!["vip".to_string()])].into_iter().collect();
            crate::space::tags::assign(Door("tag_assign"), &tk, tags).await.unwrap();
            crate::space::profile::set_folder(Door("profile_set_folder"), &tk, &id, "").await.unwrap();
            let there = t.on_server(&id).await;
            assert_eq!(there["card"]["proxy_id"], Value::Null);
            assert_eq!(there["card"]["tags"], json!(["vip"]));
            assert_eq!((there["folder"].as_str(), there["revision"].as_u64()), (Some(""), Some(4)));

            // A copy beside it, then the original to the team's trash.
            let copy = crate::space::profile::clone_profile(Door("profile_clone"), &tk, &id).await.unwrap();
            assert_eq!(t.on_server(&copy.id).await["name"], "Shop (copy)");
            let (status, got) = t.call("DELETE", &format!("/profiles/{id}"), None).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            crate::space::proxy::delete(Door("proxy_delete"), &tk, &x.id).await.unwrap();
            let trash = t.admin_call(Method::GET, "/trash", None).await;
            let trashed: Vec<&str> = trash.as_array().unwrap().iter().filter_map(|r| r["id"].as_str()).collect();
            assert!(trashed.contains(&id.as_str()) && trashed.contains(&x.id.as_str()), "{trash}");
            let listed: Vec<String> = crate::space::profile::list_all().unwrap().into_iter().map(|m| m.id).collect();
            assert_eq!(listed, std::slice::from_ref(&copy.id));

            // Not an admin: the folders are not this member's to change.
            let renamed = crate::space::profile::rename_folder(Door("folder_rename"), &tk, "Ads", "Shops").await;
            assert!(crate::space::error::find(&renamed.unwrap_err()).is_some_and(|c| c.code() == "forbidden"));
            drop(t.srv);
        }

        #[tokio::test]
        async fn the_admin_renames_and_deletes_a_team_folder_its_profiles_along() {
            let t = Team::as_admin().await;
            t.folder("Ads").await;
            for id in ["p-1", "p-2"] {
                t.profile(id, "Ads", id, json!({})).await;
            }
            t.enter().await;
            let (status, got) = t.call("PATCH", "/folders/Ads", Some(json!({ "name": "Shops" }))).await;
            assert_eq!((status, got["profiles"].as_u64()), (StatusCode::OK, Some(2)), "{got}");
            assert_eq!(t.on_server("p-1").await["folder"], "Shops");

            let (status, got) = t.call("DELETE", "/folders/Shops?delete_profiles=true", None).await;
            assert_eq!((status, got["profiles"].as_u64()), (StatusCode::OK, Some(2)), "{got}");
            let trash = t.admin_call(Method::GET, "/trash", None).await;
            assert_eq!(trash.as_array().unwrap().len(), 2, "{trash}");
            assert!(crate::space::profile::list_all().unwrap().is_empty());

            // An empty folder goes at once; the admin keeps and colours one.
            let tk = Ticket::now();
            crate::space::folders::add(Door("folder_add"), &tk, "Empty").await.unwrap();
            let look = crate::space::folders::FolderMeta { color: Some("red".into()), icon: "cart".into() };
            crate::space::folders::set_meta(Door("folder_meta_set"), &tk, "Empty", look).await.unwrap();
            let folders = t.admin_call(Method::GET, "/folders", None).await;
            assert!(folders.as_array().unwrap().iter().any(|f| f["name"] == "Empty" && f["color"] == "red"), "{folders}");
            let (status, got) = t.call("DELETE", "/folders/Empty", None).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            let folders = t.admin_call(Method::GET, "/folders", None).await;
            assert!(!folders.as_array().unwrap().iter().any(|f| f["name"] == "Empty"), "{folders}");
            drop(t.srv);
        }

        #[tokio::test]
        async fn a_save_racing_a_space_switch_is_refused_as_space_changed() {
            let t = Team::new(&[allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Edit)]).await;
            t.folder("Ads").await;
            t.profile("p-1", "Ads", "First", json!({})).await;
            t.enter().await;

            // The save reaches its last look; the launcher moves to the personal space then.
            let tk = Ticket::now();
            let mut stored = crate::space::profile::fetch_for_edit(&tk, "p-1").await.unwrap();
            stored.config.insert("name".into(), json!("Late"));
            crate::space::hold::arm();
            let save = tokio::spawn(async move {
                crate::space::profile::save(Door("profile_save"), &tk, stored, Default::default()).await
            });
            crate::space::hold::wait_reached().await;
            crate::space::switch(Space::Personal).await.unwrap();
            crate::space::hold::release();
            let e = save.await.unwrap().unwrap_err();
            assert_eq!(crate::space::error::find(&e), Some(&CmdError::SpaceChanged), "{e}");
            assert_eq!(t.on_server("p-1").await["revision"], 1);
            assert!(crate::space::profile::list_all().unwrap().is_empty(), "nothing landed here either");
            assert!(crate::space::window_ticket(None).is_ok(), "personal takes the window's writes as they come");

            // A write the interface asked before a switch is refused at its door.
            let asked_at = crate::team::space_epoch();
            t.enter().await;
            assert_eq!(crate::space::ticket(Some(asked_at)), Err(CmdError::SpaceChanged));
            // In a team the window has to name its edition, or nothing is written.
            assert_eq!(crate::space::window_ticket(None).unwrap_err().code(), "space_unknown");
            assert!(crate::space::window_ticket(Some(crate::team::space_epoch())).is_ok());

            // The API: a write sent with the edition it read under.
            let (status, read_at, _) = t.call_with("GET", "/profiles", None, None).await;
            assert_eq!(status, StatusCode::OK);
            crate::space::switch(Space::Personal).await.unwrap();
            t.enter().await;
            let late = Some(json!({ "name": "Late" }));
            let (status, _, got) = t.call_with("PATCH", "/profiles/p-1", late.clone(), read_at).await;
            assert_eq!((status, got["code"].as_str()), (StatusCode::CONFLICT, Some("space_changed")), "{got}");
            assert_eq!(t.on_server("p-1").await["revision"], 1);
            let now = Some(crate::team::space_epoch());
            let (status, _, got) = t.call_with("PATCH", "/profiles/p-1", late, now).await;
            assert_eq!(status, StatusCode::OK, "{got}");
            assert_eq!(t.on_server("p-1").await["name"], "Late");
            drop(t.srv);
        }

        /// The window's profile delete names its space like every other write.
        #[tokio::test]
        async fn a_profile_delete_from_the_window_without_its_epoch_writes_nothing() {
            let t = Team::new(&[allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Delete)]).await;
            t.folder("Ads").await;
            t.profile("p-1", "Ads", "First", json!({})).await;
            t.enter().await;
            let listed = |all: Value| all.as_array().unwrap().iter().any(|p| p["id"] == "p-1");

            assert_eq!(crate::profile_delete("p-1".into(), None).await, Err(crate::space::NO_EPOCH.to_string()));
            let stale = Some(crate::team::space_epoch() - 1);
            let refused = crate::profile_delete("p-1".into(), stale).await;
            assert_eq!(refused, Err(crate::space::error::SPACE_CHANGED.to_string()));
            assert!(listed(t.admin_call(Method::GET, "/profiles", None).await), "nothing was deleted");

            crate::profile_delete("p-1".into(), Some(crate::team::space_epoch())).await.unwrap();
            assert!(!listed(t.admin_call(Method::GET, "/profiles", None).await));
            drop(t.srv);
        }
    }
}
