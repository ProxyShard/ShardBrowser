// Local automation HTTP API (axum) for ShardX Launcher.
// 127.0.0.1:<api_port>; every endpoint except /health requires Bearer JWT (HS256).

use std::sync::{OnceLock, RwLock};

use tokio::sync::watch;

use axum::{
    extract::{Path, Query, Request},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

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

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
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
    let metas = crate::profile::list_all()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let running = crate::process::Tracker::shared().running();
    let by_id: std::collections::HashMap<String, crate::process::RunningProfile> = running
        .into_iter()
        .map(|r| (r.profile_id.clone(), r))
        .collect();
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
                "running": r.is_some(),
                "pid": r.map(|x| x.pid),
                "cdp": r.and_then(|x| x.cdp.clone()),
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn get_profile(Path(id): Path<String>) -> ApiResult {
    let stored =
        crate::profile::load_raw(&id).map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;
    let mut val = serde_json::to_value(stored)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(cdp) = crate::process::Tracker::shared().cdp(&id) {
        if let Some(obj) = val.as_object_mut() {
            obj.insert("running".into(), json!(true));
            obj.insert(
                "cdp".into(),
                serde_json::to_value(cdp).unwrap_or(Value::Null),
            );
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
    fingerprint: Value,
}

/// Persist verbatim (enrich=false); proxy_id binds, proxy string upserts+tests.
async fn persist_created(folder_override: Option<String>, body: CreateReq) -> ApiResult {
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
    let mut meta = json!({ "id": "", "folder": folder });
    if let Some(pid) = body.proxy_id.as_ref() {
        meta["proxy_id"] = json!(pid);
    } else if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::proxy::parse_single(pstr).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unparseable proxy: {pstr}"),
            )
        })?;
        let stored = crate::proxy::upsert_dedup(entry)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        // Best-effort full test (UDP + geo); launch re-probes UDP live anyway.
        let _ = crate::proxy::full_test(&stored).await;
        meta["proxy_id"] = json!(stored.id);
    }
    cfg.insert("_meta".into(), meta);

    let pm = crate::save_profile_core(crate::main_window().as_ref(), Value::Object(cfg), false)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(serde_json::to_value(pm).unwrap_or(Value::Null)))
}

async fn create_profile(Json(body): Json<CreateReq>) -> ApiResult {
    persist_created(None, body).await
}

async fn create_profile_in_folder(
    Path(folder): Path<String>,
    Json(body): Json<CreateReq>,
) -> ApiResult {
    persist_created(Some(folder), body).await
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
}

/// Temporary profile (hidden, auto-deleted on close); pair with /start.
async fn create_temporary(Json(body): Json<TempReq>) -> ApiResult {
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
    let mut meta =
        json!({ "id": "", "folder": body.folder.unwrap_or_default(), "temporary": true });
    if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::proxy::parse_single(pstr).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unparseable proxy: {pstr}"),
            )
        })?;
        meta["inline_proxy"] = serde_json::to_value(entry).unwrap_or(Value::Null);
    }
    cfg.insert("_meta".into(), meta);

    let pm = crate::save_profile_core(crate::main_window().as_ref(), Value::Object(cfg), false)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({
        "id": pm.id,
        "name": pm.name,
        "fingerprint_id": fid,
        "temporary": true,
        "proxy_inline": body.proxy.is_some(),
    })))
}

async fn delete_profile(Path(id): Path<String>) -> ApiResult {
    crate::profile::delete(&id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "deleted": true, "id": id })))
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
    /// Replace stored fingerprint verbatim.
    fingerprint: Option<Value>,
}

/// Edit profile; only provided fields change. Returns the updated profile.
async fn edit_profile(Path(id): Path<String>, Json(body): Json<EditReq>) -> ApiResult {
    let mut stored =
        crate::profile::load_raw(&id).map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

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
        stored.meta.proxy_id = if pid.is_empty() {
            None
        } else {
            Some(pid.clone())
        };
        stored.meta.inline_proxy = None;
    } else if let Some(pstr) = body.proxy.as_ref() {
        let entry = crate::proxy::parse_single(pstr).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("unparseable proxy: {pstr}"),
            )
        })?;
        let s = crate::proxy::upsert_dedup(entry)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let _ = crate::proxy::full_test(&s).await;
        stored.meta.proxy_id = Some(s.id);
        stored.meta.inline_proxy = None;
    }

    crate::profile::save_raw(&mut stored)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // set_folder handles unfile; save_raw keeps the existing folder when empty.
    if let Some(f) = body.folder.as_ref() {
        crate::profile::set_folder(&id, f)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let updated = crate::profile::load_raw(&id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
struct RenameFolderReq {
    name: String,
}

async fn rename_folder_ep(
    Path(folder): Path<String>,
    Json(body): Json<RenameFolderReq>,
) -> ApiResult {
    let n = crate::profile::rename_folder(&folder, &body.name)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "renamed_to": body.name, "profiles": n })))
}

#[derive(Deserialize)]
struct DeleteFolderQuery {
    /// true → delete profiles; false (default) → unfile.
    #[serde(default)]
    delete_profiles: bool,
}

async fn delete_folder_ep(
    Path(folder): Path<String>,
    Query(q): Query<DeleteFolderQuery>,
) -> ApiResult {
    let n = crate::profile::delete_folder(&folder, q.delete_profiles)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "profile_id": id,
        "pid": outcome.pid,
        "headless": headless,
        "cdp": outcome.cdp,
    })))
}

async fn stop_profile(Path(id): Path<String>) -> ApiResult {
    let stopped = crate::process::Tracker::shared()
        .kill(&id)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "profile_id": id, "stopped": stopped })))
}

async fn export_cookies(Path(id): Path<String>) -> ApiResult {
    let cookies = crate::cookies::export(&id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "cookies": cookies })))
}

#[derive(Deserialize)]
struct ImportCookiesReq {
    cookies: Vec<crate::cookies::Cookie>,
}

async fn import_cookies(Path(id): Path<String>, Json(body): Json<ImportCookiesReq>) -> ApiResult {
    // Running browser would clobber imports on exit.
    if crate::is_profile_running(&id) {
        return Err(err(
            StatusCode::CONFLICT,
            "stop the profile before importing cookies",
        ));
    }
    let n = crate::cookies::import(&id, &body.cookies)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "imported": n })))
}

async fn list_running() -> Json<Value> {
    Json(json!(crate::process::Tracker::shared().running()))
}

async fn list_fingerprints() -> ApiResult {
    let all = crate::fingerprints::list_all()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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

/// Add proxy (deduped by endpoint); returns summary.
async fn add_proxy(Json(body): Json<AddProxyReq>) -> ApiResult {
    let mut entry = if let Some(s) = body.proxy.as_ref() {
        crate::proxy::parse_single(s)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unparseable proxy: {s}")))?
    } else {
        let host = body.host.clone().ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "`proxy` string or host+port required",
            )
        })?;
        let port = body
            .port
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "`port` required"))?;
        let kind = match body.kind.as_deref() {
            Some("http") => crate::proxy::ProxyKind::Http,
            Some("https") => crate::proxy::ProxyKind::Https,
            _ => crate::proxy::ProxyKind::Socks5,
        };
        crate::proxy::ProxyEntry {
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
    let stored = crate::proxy::upsert_dedup(entry)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "id": stored.id,
        "name": stored.name,
        "kind": stored.kind,
        "host": stored.host,
        "port": stored.port,
        "country": stored.country,
    })))
}

async fn delete_proxy(Path(id): Path<String>) -> ApiResult {
    crate::proxy::delete(&id).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "deleted": true, "id": id })))
}

async fn list_proxies() -> ApiResult {
    let list =
        crate::proxy::list().map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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

async fn list_folders() -> ApiResult {
    let metas = crate::profile::list_all()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        other => other.to_string(),
    }
}

/// Random fingerprint id for platform (host OS when None); falls back to all.
fn random_fingerprint_for(platform: Option<&str>) -> Result<String, ApiError> {
    let want = platform
        .map(normalize_platform)
        .unwrap_or_else(crate::host_platform);
    let all = crate::fingerprints::list_all()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if all.is_empty() {
        return Err(err(StatusCode::NOT_FOUND, "fingerprint library is empty"));
    }
    let matching: Vec<crate::fingerprints::LibraryEntry> = all
        .iter()
        .filter(|e| e.platform.eq_ignore_ascii_case(&want))
        .cloned()
        .collect();
    let pool = if matching.is_empty() { all } else { matching };
    let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % pool.len();
    Ok(pool[idx].id.clone())
}

// ---- managed server lifecycle ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub enabled: bool,
    pub port: u16,
    pub secret: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiState {
    pub running: bool,
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn state_cell() -> &'static RwLock<ApiState> {
    static STATE: OnceLock<RwLock<ApiState>> = OnceLock::new();
    STATE.get_or_init(|| {
        RwLock::new(ApiState {
            running: false,
            port: None,
            error: None,
        })
    })
}

fn config_sender() -> &'static OnceLock<watch::Sender<ApiConfig>> {
    static CONFIG: OnceLock<watch::Sender<ApiConfig>> = OnceLock::new();
    &CONFIG
}

fn set_state(state: ApiState) {
    if let Ok(mut current) = state_cell().write() {
        *current = state;
    }
}

pub fn state() -> ApiState {
    state_cell()
        .read()
        .map(|state| state.clone())
        .unwrap_or(ApiState {
            running: false,
            port: None,
            error: Some("API state lock poisoned".into()),
        })
}

pub fn validate_config(config: &ApiConfig) -> Result<(), String> {
    if config.enabled && config.port == 0 {
        return Err("automation API port must be between 1 and 65535".into());
    }
    if config.secret.is_empty() {
        return Err("automation API secret must not be empty".into());
    }
    Ok(())
}

/// Apply settings to the running supervisor. Secret rotation takes effect immediately.
pub fn configure(config: ApiConfig) -> Result<(), String> {
    validate_config(&config)?;
    set_secret(&config.secret);
    let sender = config_sender()
        .get()
        .ok_or_else(|| "automation API supervisor is not started".to_string())?;
    sender.send_replace(config);
    Ok(())
}

/// Start the single process-wide API supervisor.
pub fn start_supervisor(config: ApiConfig) -> Result<(), String> {
    validate_config(&config)?;
    set_secret(&config.secret);
    let (sender, receiver) = watch::channel(config);
    config_sender()
        .set(sender)
        .map_err(|_| "automation API supervisor is already started".to_string())?;
    tauri::async_runtime::spawn(supervise(receiver));
    Ok(())
}

fn router() -> Router {
    let protected = Router::new()
        .route("/profiles", get(list_profiles).post(create_profile))
        .route("/profiles/temporary", post(create_temporary))
        .route(
            "/profiles/:id",
            get(get_profile).patch(edit_profile).delete(delete_profile),
        )
        .route("/profiles/:id/start", post(start_profile))
        .route("/profiles/:id/stop", post(stop_profile))
        .route(
            "/profiles/:id/cookies",
            get(export_cookies).post(import_cookies),
        )
        .route("/folders", get(list_folders))
        .route(
            "/folders/:folder",
            patch(rename_folder_ep).delete(delete_folder_ep),
        )
        .route("/folders/:folder/profiles", post(create_profile_in_folder))
        .route("/fingerprint/new", get(new_fingerprint))
        .route("/fingerprint/new/:platform", get(new_fingerprint_for))
        .route("/fingerprints", get(list_fingerprints))
        .route("/running", get(list_running))
        .route("/proxies", get(list_proxies).post(add_proxy))
        .route("/proxies/:id", delete(delete_proxy))
        .route_layer(middleware::from_fn(auth));

    Router::new().route("/health", get(health)).merge(protected)
}

async fn wait_for_listener_change(mut receiver: watch::Receiver<ApiConfig>, active: ApiConfig) {
    while receiver.changed().await.is_ok() {
        let next = receiver.borrow();
        if next.enabled != active.enabled || next.port != active.port {
            return;
        }
    }
}

async fn supervise(mut receiver: watch::Receiver<ApiConfig>) {
    loop {
        let config = receiver.borrow().clone();
        set_secret(&config.secret);

        if !config.enabled {
            set_state(ApiState {
                running: false,
                port: None,
                error: None,
            });
            if receiver.changed().await.is_err() {
                return;
            }
            continue;
        }

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(error) => {
                let message = error.to_string();
                eprintln!("[launcher] API bind {addr} failed: {message}");
                set_state(ApiState {
                    running: false,
                    port: None,
                    error: Some(message),
                });
                if receiver.changed().await.is_err() {
                    return;
                }
                continue;
            }
        };

        eprintln!("[launcher] automation API listening on http://{addr}");
        set_state(ApiState {
            running: true,
            port: Some(config.port),
            error: None,
        });

        let shutdown = wait_for_listener_change(receiver.clone(), config);
        if let Err(error) = axum::serve(listener, router())
            .with_graceful_shutdown(shutdown)
            .await
        {
            eprintln!("[launcher] API server error: {error}");
            set_state(ApiState {
                running: false,
                port: None,
                error: Some(error.to_string()),
            });
        } else {
            set_state(ApiState {
                running: false,
                port: None,
                error: None,
            });
        }

        // Synchronize the outer receiver with changes consumed by the shutdown clone.
        if receiver.has_changed().unwrap_or(false) && receiver.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn config(enabled: bool, port: u16, secret: &str) -> ApiConfig {
        ApiConfig {
            enabled,
            port,
            secret: secret.to_string(),
        }
    }

    #[test]
    fn accepts_valid_enabled_configuration() {
        assert!(validate_config(&config(true, 40325, "secret")).is_ok());
    }

    #[test]
    fn rejects_empty_secret() {
        let error = validate_config(&config(false, 40325, "")).unwrap_err();
        assert!(error.contains("secret"));
    }

    #[test]
    fn rejects_zero_port_when_enabled() {
        let error = validate_config(&config(true, 0, "secret")).unwrap_err();
        assert!(error.contains("port"));
    }

    #[test]
    fn allows_zero_port_when_disabled() {
        assert!(validate_config(&config(false, 0, "secret")).is_ok());
    }
}
