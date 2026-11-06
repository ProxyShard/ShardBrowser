//! The HTTP face of the team server.
//!
//! The launcher enforces the permission it holds, which is what makes the team
//! usable on a slow link. But a member token sent straight at this API has to
//! get the same answers, so every member route asks [`decide`] again here. The
//! launcher is the fast copy of the rules; this is the one that counts.
//!
//! Two habits run through the file. A profile a member may not See is reported
//! as missing rather than refused — that it exists at all is something they are
//! not allowed to know. And the data blob is only ever carried, never opened:
//! it arrives base64 inside JSON and leaves the same way.
//!
//! Two kinds of space. The team's is shared and governed by grants. Each
//! member also has their own, under `/me`, kept here so they can use it from
//! any of their devices: only they reach it, and grants say nothing about it.
//! An id in the other space reads as missing from this one, whichever way.
//!
//! The admin token also opens every member route as the principal [`ADMIN_ID`]:
//! every right, its own personal space, leases in that name, and the whole team's news.
//!
//! Deleting from the team moves the item to the team's trash for a number of
//! days; deleting from a personal space is final (the launcher keeps its own trash).
//!
//! Every refusal is `{"error": <fixed text>, "code": <stable code>}`; the
//! launcher keys off `code`.
//!
//! A write's `base_revision` is refused as `stale` once the item moved on or
//! is gone (`current_revision` 0); a base of 0 means "only if new".
//!
//! An id waiting in the team's trash answers 409 `in_trash` to whoever may see it there.
//!
//! `GET /permission` carries only the rules about what the member can see, and
//! their epoch moves, with `grants-changed`, whenever that set of rules does.
//!
//! Every profile row carries `created_at`, unix seconds, null for one an older
//! server kept. A PUT may send it: a new profile keeps it if it is plausible
//! (past 0, at most a day ahead), else is dated now; an existing one keeps the
//! date it has, and only one with none takes the writer's. Moves and the trash keep it.
//!
//! A profile's `card` (`proxy_id`, `color`, `notes`, `tags`) travels beside its
//! sealed `data` on PUT and shows in every row; absent on PUT keeps the stored
//! one. Its `proxy_id` is the proxy binding: a new one must be in the profile's
//! space, and in the team usable by the writer (See, and Start, Edit or
//! TakeOut), else 400 `bad_proxy`. A move into the team is judged the same way.
//!
//! A profile's session (cookies, storage, logins, packed by the launcher) is
//! its own object with its own `session_rev`, moved only by the device holding
//! the lease, raw bytes up to 256 MB at `/profiles/:id/session` (and `/me/…`):
//!
//! * `PUT ?base_session_rev=N` (0 before the first) answers
//!   `{session_rev, size, updated_at, updated_by}`; 409 `stale` with
//!   `current_session_rev` once it moved on. The lease is renewed while it comes in.
//! * `GET` also needs Start, and answers a `u32` big-endian length N, N bytes
//!   of JSON `{session_rev, size, updated_at, updated_by, proxy_id, proxy}`, then
//!   the session bytes; the length is declared, so fewer means it broke off.
//!   `proxy` is the bound proxy with its data, so Start alone launches it
//!   right; null beside a `proxy_id` means that proxy is not there.
//!
//! Bookmarks, extensions, automations and tags live at `/items/:kind[/:id]`
//! (and `/me/items/…`), with revisions like profiles; the team's rights on them
//! are team-wide. `?profile=<id>` on a team list or package answers what starting
//! that profile needs to whoever may Start it: its folder's bookmarks and the
//! extensions its settings name. An extension's package is its own blob at `…/:id/package`,
//! up to 64 MB; every other body but a session's is held to 16 MB. A member's
//! own space also keeps the kind `sync` (`/me/items/sync/…`): how their devices sync it.
//!
//! `POST /me/profiles/:id/lease?takeover=true` takes the lease from another of
//! the member's own devices, which hears `leased` at its next renewal.
//!
//! `POST /admin/members/:id/token` re-issues a member's token: the old one
//! answers 401 `token_replaced` from then on (not `unauthorized`, which is a
//! removal), and their live sockets hear `token-replaced` and are closed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade};
use axum::middleware::{self, Next};
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;

use crate::auth::{self, PermissionKey, PERMISSION_TTL_SECS};
use crate::live::{Event, Hub, Seat, MAX_SOCKETS};
use crate::view::{self, proxy_target};
use crate::model::{
    admin_grants, decide, Action, Card, Grant, Member, Target, TeamProfile, ADMIN_ID, ITEM_KINDS,
};
use crate::store::{
    personal_space, BlobMeta, BodyRef, Folder, FolderDelete, FolderRename, Item, Lease, Listed,
    ProfileFields, ProxyFields, Refusal, Restored, SessionHead, Standing, Store, StoredItem,
    TeamProxy, TrashItem, TrashKind, Upload, Writer, CHUNK, META_TEAM_NAME, TEAM,
};
use hyper::body::{Frame, SizeHint};

/// Every body but a session or a package: settings and small items. Past axum's
/// default, not far past it.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// A session, and only that: read by the handler itself, after the gate has
/// turned strangers away.
pub const BLOB_MAX: usize = 256 * 1024 * 1024;

/// An extension's package: read like a session, with a limit of its own.
pub const PACKAGE_MAX: usize = 64 * 1024 * 1024;

/// What a session download is: a length, that much JSON, then the bytes.
const SESSION_TYPE: &str = "application/vnd.shardx.session";

/// The revision of the package a download carries.
const PACKAGE_REV: &str = "x-package-rev";

/// An item's id, in characters.
const MAX_ITEM_ID: usize = 128;

/// How long a session or package body may stall between pieces.
const BODY_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a lease lasts unless renewed. The launcher renews about every
/// 30 s while the browser runs, so this rides out a couple of missed renewals
/// on a bad link, and a launcher that died frees the profile within a minute
/// and a half.
pub(crate) const LEASE_TTL_SECS: u64 = 90;

/// The most ids one `GET /profiles?ids=` may ask about.
const MAX_IDS: usize = 500;

/// A team's name, in characters, once trimmed.
const MAX_TEAM_NAME: usize = 64;

/// What the team is called until somebody names it.
pub const DEFAULT_TEAM_NAME: &str = "Team";

/// What the admin is called where a member's name would be shown.
const ADMIN_NAME: &str = "Admin";

/// The admin's rights never change, so its permission is always this edition.
const ADMIN_EPOCH: u64 = 1;

/// What a launcher may send down the live channel: control frames, nothing of
/// size. A frame or message past this closes the socket.
const MAX_LIVE_IN: usize = 4096;

/// The device a member request comes from, as the launcher names it.
const DEVICE_ID: &str = "x-device-id";
const DEVICE_NAME: &str = "x-device-name";

/// Names and labels are stored, sent in every list and shown on other
/// members' screens, so they are bounded like the device name.
const MAX_LABEL: usize = 200;

/// The rights that say something about one profile: all but Create.
const ON_A_PROFILE: [Action; 6] = [
    Action::See,
    Action::Start,
    Action::Edit,
    Action::Delete,
    Action::Cookies,
    Action::TakeOut,
];

/// Seeing an item is knowing it is there. Its data is the session and the
/// logins themselves, for someone who will open, change, export or take it.
const USES_A_PROFILE: [Action; 4] = [Action::Start, Action::Edit, Action::Cookies, Action::TakeOut];
const USES_A_PROXY: [Action; 3] = [Action::Start, Action::Edit, Action::TakeOut];

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub key: Arc<PermissionKey>,
    pub admin_secret: Arc<str>,
    /// This server's team id. It travels in every permission so a launcher
    /// bound to two teams cannot mix them up.
    pub team: Arc<str>,
    pub hub: Hub,
    /// How long a deleted team item waits in the trash before it is purged.
    pub trash_ttl_secs: u64,
    /// The largest session taken in: [`BLOB_MAX`] outside tests.
    pub session_max: usize,
    /// The largest package taken in: [`PACKAGE_MAX`] outside tests.
    pub package_max: usize,
    /// `SHARDBROWSER_TEAM_PUBLIC_URL` as the owner gave it: what `/me` tells the
    /// admin's launcher to start member invites with.
    pub public_url: Option<Arc<str>>,
}

pub fn router(state: AppState) -> Router {
    // The gates run on the request's head, before any handler reads its body.
    // A handler still checks the token itself — that is what decides — but a
    // caller with no valid token at all is turned away here, without the
    // server first taking in up to MAX_BODY bytes from someone it does not
    // know.
    let admin = Router::new()
        .route("/admin/members", get(members_list).post(members_create))
        .route("/admin/members/:id", patch(member_set_active).delete(member_delete))
        .route("/admin/members/:id/grants", get(member_grants).put(member_set_grants))
        .route("/admin/members/:id/token", post(member_reissue))
        .route("/admin/folders", post(admin_folder_create))
        .route(
            "/admin/folders/:name",
            patch(admin_folder_change).delete(admin_folder_delete),
        )
        .route("/admin/team", patch(team_rename))
        .route_layer(middleware::from_fn_with_state(state.clone(), admin_gate));

    // The admin token passes this door too, as the admin principal.
    let member = Router::new()
        .route("/me", get(me_get))
        .route("/permission", get(permission_get))
        .route("/live", get(live_ws))
        .route("/profiles", get(profiles_list))
        .route("/trash", get(trash_list))
        .route("/trash/:id", axum::routing::delete(trash_purge))
        .route("/trash/:id/restore", post(trash_restore))
        .route(
            "/profiles/:id",
            get(profile_get).put(profile_put).delete(profile_delete),
        )
        .route("/profiles/:id/lease", post(lease_take).delete(lease_give_back))
        .route("/profiles/:id/session", get(session_get).put(session_put))
        .route("/profiles/:id/to-me", post(profile_to_me))
        .route("/proxies", get(proxies_list))
        .route(
            "/proxies/:id",
            get(proxy_get).put(proxy_put).delete(proxy_delete),
        )
        .route("/proxies/:id/to-me", post(proxy_to_me))
        .route("/folders", get(folders_list))
        .route("/me/profiles", get(my_profiles_list))
        .route(
            "/me/profiles/:id",
            get(my_profile_get).put(my_profile_put).delete(my_profile_delete),
        )
        .route("/me/profiles/:id/lease", post(my_lease_take).delete(my_lease_give_back))
        .route("/me/profiles/:id/session", get(my_session_get).put(my_session_put))
        .route("/me/profiles/:id/to-team", post(my_profile_to_team))
        .route("/me/proxies", get(my_proxies_list))
        .route(
            "/me/proxies/:id",
            get(my_proxy_get).put(my_proxy_put).delete(my_proxy_delete),
        )
        .route("/me/proxies/:id/to-team", post(my_proxy_to_team))
        .route("/me/folders", get(my_folders_list).post(my_folder_create))
        .route(
            "/me/folders/:name",
            patch(my_folder_change).delete(my_folder_delete),
        )
        .route("/items/:kind", get(items_list))
        .route("/items/:kind/:id", get(item_get).put(item_put).delete(item_delete))
        .route("/items/:kind/:id/package", get(package_get).put(package_put))
        .route("/me/items/:kind", get(my_items_list))
        .route(
            "/me/items/:kind/:id",
            get(my_item_get).put(my_item_put).delete(my_item_delete),
        )
        .route("/me/items/:kind/:id/package", get(my_package_get).put(my_package_put))
        .route_layer(middleware::from_fn_with_state(state.clone(), member_gate));

    Router::new()
        .route("/health", get(health))
        .route(team_wire::VOUCH, post(vouch))
        .merge(admin)
        .merge(member)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(json_errors))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Axum's own refusals (bad JSON, wrong method, unknown path, too large) are
/// plain text and may quote the request; they leave as `{"error"}` like ours.
async fn json_errors(req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    let status = res.status();
    let ours = res
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if !(status.is_client_error() || status.is_server_error()) || ours {
        return res;
    }
    let (message, code) = match status {
        StatusCode::NOT_FOUND => ("no such thing", code::NOT_FOUND),
        StatusCode::METHOD_NOT_ALLOWED => ("this route does not take that method", code::METHOD),
        StatusCode::PAYLOAD_TOO_LARGE => ("the body is too large", code::TOO_LARGE),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ("the body has to be JSON", code::BAD_REQUEST),
        s if s.is_server_error() => ("the server could not do that", code::INTERNAL),
        _ => ("the request is not what this route takes", code::BAD_REQUEST),
    };
    // Status and headers such as Allow stay; only the body is ours.
    let (mut parts, _) = res.into_parts();
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let body = serde_json::json!({ "error": message, "code": code }).to_string();
    Response::from_parts(parts, Body::from(body))
}

/// The machine-readable half of every refusal. Stable: the launcher keys off these.
pub mod code {
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// The token was re-issued: the member is still in the team, under a new one.
    pub const TOKEN_REPLACED: &str = "token_replaced";
    pub const SWITCHED_OFF: &str = "switched_off";
    pub const FORBIDDEN: &str = "forbidden";
    pub const NOT_FOUND: &str = "not_found";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const STALE: &str = "stale";
    pub const LEASED: &str = "leased";
    /// A session or package moved from a device that does not hold the lease.
    pub const NO_LEASE: &str = "no_lease";
    pub const CONFLICT: &str = "conflict";
    /// A card binds a proxy that is not there for this profile, or not the writer's to use.
    pub const BAD_PROXY: &str = "bad_proxy";
    /// The id waits in the team's trash: restore or purge it, or pick another.
    pub const IN_TRASH: &str = "in_trash";
    pub const TOO_LARGE: &str = "too_large";
    pub const METHOD: &str = "method_not_allowed";
    /// A vouch for a token this server never issued, or one since removed.
    pub const UNKNOWN_TOKEN: &str = team_wire::UNKNOWN_TOKEN;
    pub const INTERNAL: &str = "internal";
}

/// Turn away anyone without the admin token before the body is read.
async fn admin_gate(State(st): State<AppState>, req: Request, next: Next) -> Response {
    match require_admin(&st, req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// Turn away anyone who is not an active member before the body is read.
/// The handler reads the member and their grants again; this is only the door.
/// A member let through is noted as seen, at most once a minute.
async fn member_gate(State(st): State<AppState>, req: Request, next: Next) -> Response {
    match require_member(&st, req.headers()) {
        Ok(c) => {
            if !c.admin {
                if let Err(e) = st.store.seen(&c.member.id, &c.token_hash) {
                    tracing::warn!("last seen: {e:#}");
                }
            }
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

// ---- refusals ----

/// What a route says when it will not do the thing. The message is always a
/// fixed string: an error that echoes the request is an error that leaks it.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: &'static str,
    /// One of [`code`]: what the launcher acts on.
    pub code: &'static str,
    /// What is in the way, for the two conflicts a launcher has to act on
    /// rather than just show. Both come out of the store, never the request.
    /// Boxed: most refusals carry none, and the error stays small.
    pub conflict: Option<Box<Conflict>>,
}

#[derive(Debug)]
pub enum Conflict {
    /// Who has it open, and on which device: what the person in front of the
    /// second device needs to see.
    Lease(Lease),
    /// The revision to re-read before trying again.
    Revision(u64),
    /// The same for a session, and for an extension's package.
    SessionRevision(u64),
    PackageRevision(u64),
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        ApiError { status, message, code, conflict: None }
    }
    fn unauthorized() -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, code::UNAUTHORIZED, "this route needs a token")
    }
    /// Told apart from `unauthorized`, which a launcher reads as being removed.
    fn token_replaced() -> Self {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            code::TOKEN_REPLACED,
            "the server does not know this token: it was replaced by a new one",
        )
    }
    /// Told apart from an ordinary refusal: the launcher closes everything of the team's.
    fn switched_off() -> Self {
        ApiError::new(StatusCode::FORBIDDEN, code::SWITCHED_OFF, "this member is switched off")
    }
    fn unknown_token() -> Self {
        ApiError::new(StatusCode::NOT_FOUND, code::UNKNOWN_TOKEN, "this server does not know this token")
    }
    fn forbidden() -> Self {
        ApiError::new(StatusCode::FORBIDDEN, code::FORBIDDEN, "not allowed")
    }
    fn missing() -> Self {
        ApiError::new(StatusCode::NOT_FOUND, code::NOT_FOUND, "no such thing")
    }
    fn bad(message: &'static str) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, code::BAD_REQUEST, message)
    }
    fn conflict(message: &'static str) -> Self {
        ApiError::new(StatusCode::CONFLICT, code::CONFLICT, message)
    }
    fn folder_taken() -> Self {
        ApiError::conflict("a folder of that name is already there")
    }
    fn no_device() -> Self {
        ApiError::bad("a lease needs X-Device-Id")
    }
    fn too_large() -> Self {
        ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, code::TOO_LARGE, "the body is too large")
    }
    fn in_trash() -> Self {
        ApiError::new(StatusCode::CONFLICT, code::IN_TRASH, "the id waits in the trash")
    }
    fn stale(conflict: Conflict) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            message: "changed since you read it",
            code: code::STALE,
            conflict: Some(Box::new(conflict)),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.message, "code": self.code });
        match self.conflict.map(|c| *c) {
            Some(Conflict::Lease(lease)) => {
                body["lease"] = serde_json::to_value(lease).unwrap_or_default();
            }
            Some(Conflict::Revision(n)) => body["current_revision"] = n.into(),
            Some(Conflict::SessionRevision(n)) => body["current_session_rev"] = n.into(),
            Some(Conflict::PackageRevision(n)) => body["current_package_rev"] = n.into(),
            None => {}
        }
        (self.status, Json(body)).into_response()
    }
}

/// Anything the store or the signing key failed at is ours, not the caller's.
/// The real reason goes to the log, where the person running the box can read
/// it; the caller gets told only that it did not work.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, code::INTERNAL, "the server could not do that")
    }
}

/// A write the store would not make. Living in another space reads exactly
/// like not being there: where an id lives is not this caller's business.
impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::Leased(lease) => ApiError {
                status: StatusCode::CONFLICT,
                message: "open on another device",
                code: code::LEASED,
                conflict: Some(Box::new(Conflict::Lease(lease))),
            },
            Refusal::Stale { current } => ApiError::stale(Conflict::Revision(current)),
            Refusal::SessionStale { current } => ApiError::stale(Conflict::SessionRevision(current)),
            Refusal::PackageStale { current } => ApiError::stale(Conflict::PackageRevision(current)),
            Refusal::NoLease => {
                ApiError::new(StatusCode::CONFLICT, code::NO_LEASE, "not open on this device")
            }
            Refusal::BadProxy => {
                ApiError::new(StatusCode::BAD_REQUEST, code::BAD_PROXY, "no such proxy to bind")
            }
            Refusal::Forbidden => ApiError::forbidden(),
            Refusal::SwitchedOff => ApiError::switched_off(),
            Refusal::Removed => ApiError::unauthorized(),
            // Said only where the caller may see it there: see `refused_team_write`.
            Refusal::OtherSpace | Refusal::Missing | Refusal::InTrash(_) => ApiError::missing(),
        }
    }
}

// ---- who is asking ----

/// The token out of `Authorization: Bearer`. Nothing reads a credential from
/// the query string, not even the WebSocket route: a URL ends up in logs.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

fn require_admin(st: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = bearer(headers).ok_or_else(ApiError::unauthorized)?;
    if auth::is_admin(&st.admin_secret, token) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

/// Which of the member's devices is asking. Not a credential — the token says
/// who; this only tells their devices apart, so the one that has a profile
/// open is the one that may write it. No id means a script, which holds no
/// lease and so can never write over a profile that is open somewhere.
struct Device {
    id: Option<String>,
    name: String,
}

/// A header as UTF-8 rather than through `to_str`, which takes ASCII only: a
/// device is named by a person, often in their own language.
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let v = std::str::from_utf8(headers.get(name)?.as_bytes()).ok()?.trim();
    (!v.is_empty()).then_some(v)
}

fn device(headers: &HeaderMap) -> Device {
    // Bounded: both are stored, and the name is shown on somebody else's screen.
    Device {
        id: header_text(headers, DEVICE_ID)
            .filter(|s| s.len() <= 128)
            .map(str::to_owned),
        name: header_text(headers, DEVICE_NAME)
            .map(|s| s.chars().take(64).collect())
            .unwrap_or_default(),
    }
}

/// A member and the rights they have as of this request. The grants are read
/// fresh every time rather than trusted from whatever they are holding — that
/// is the whole point of asking the server.
struct Caller {
    member: Member,
    grants: Vec<Grant>,
    device: Device,
    /// The admin token, acting as [`ADMIN_ID`].
    admin: bool,
    /// The hash of the member's token; empty for the admin.
    token_hash: String,
}

/// The admin principal: every right in the team, no member row behind it.
fn admin_member() -> Member {
    Member {
        id: ADMIN_ID.into(),
        name: ADMIN_NAME.into(),
        active: true,
        created_at: 0,
        epoch: ADMIN_EPOCH,
        last_seen_at: None,
    }
}

/// A member by their token, or the admin by theirs.
fn require_member(st: &AppState, headers: &HeaderMap) -> Result<Caller, ApiError> {
    let token = bearer(headers).ok_or_else(ApiError::unauthorized)?;
    if auth::is_admin(&st.admin_secret, token) {
        return Ok(Caller {
            member: admin_member(),
            grants: admin_grants(),
            device: device(headers),
            admin: true,
            token_hash: String::new(),
        });
    }
    let hash = auth::token_hash(token);
    let member = match st.store.member_by_token_hash(&hash)? {
        Some(m) => m,
        None if st.store.token_replaced(&hash)? => return Err(ApiError::token_replaced()),
        None => return Err(ApiError::unauthorized()),
    };
    if !member.active {
        // Switched off beats everything their table still says.
        return Err(ApiError::switched_off());
    }
    let grants = st.store.grants_of(&member.id)?;
    Ok(Caller { member, grants, device: device(headers), admin: false, token_hash: hash })
}

fn allows(grants: &[Grant], action: Action, target: &Target<'_>) -> bool {
    decide(grants, action, target).is_allowed()
}

/// The rules a guarded write is judged by: as the store read them under the
/// write's own lock, not the copy taken when the request came in.
fn live(s: &Standing) -> Result<&[Grant], Refusal> {
    if !s.known {
        Err(Refusal::Removed)
    } else if !s.active {
        Err(Refusal::SwitchedOff)
    } else {
        Ok(&s.grants)
    }
}

/// A member's own space has no rules; being switched on is all it asks.
fn still_on(s: &Standing) -> Result<(), Refusal> {
    live(s).map(|_| ())
}

impl Caller {
    fn may(&self, action: Action, target: &Target<'_>) -> bool {
        allows(&self.grants, action, target)
    }

    fn writer(&self) -> Writer<'_> {
        Writer { member_id: &self.member.id, device_id: self.device.id.as_deref() }
    }

    /// Their own space. Grants are about the team and say nothing here.
    fn mine(&self) -> String {
        personal_space(&self.member.id)
    }
}

/// Whether a member may read a team proxy's login: See, and a right that uses it.
/// Binding one asks the same, since the binding hands the login to whoever may Start.
fn may_use_proxy(grants: &[Grant], id: &str) -> bool {
    let t = proxy_target(id);
    allows(grants, Action::See, &t) && USES_A_PROXY.iter().any(|a| allows(grants, *a, &t))
}

/// The rights to make an item new where the team already names its id: it is
/// writing over what those names mean, so Edit there too, not just Create.
fn may_take_name(grants: &[Grant], target: &Target<'_>) -> Result<(), Refusal> {
    if allows(grants, Action::See, target) && allows(grants, Action::Edit, target) {
        Ok(())
    } else {
        Err(Refusal::Forbidden)
    }
}

/// The rights check a guarded write runs on an existing row. No See reads as
/// missing, so that it exists does not leak.
fn see_then(grants: &[Grant], action: Action, target: &Target<'_>) -> Result<(), Refusal> {
    if !allows(grants, Action::See, target) {
        Err(Refusal::Missing)
    } else if !allows(grants, action, target) {
        Err(Refusal::Forbidden)
    } else {
        Ok(())
    }
}

/// Moving a profile to another folder is making it there — Create where it
/// lands — and may not hand the mover a right on it they lack where it is.
fn may_move(grants: &[Grant], id: &str, from: &str, to: &str) -> Result<(), Refusal> {
    let (here, there) = (Target::profile(id, from), Target::profile(id, to));
    let widens = ON_A_PROFILE
        .iter()
        .any(|a| allows(grants, *a, &there) && !allows(grants, *a, &here));
    if allows(grants, Action::Create, &Target::folder(to)) && !widens {
        Ok(())
    } else {
        Err(Refusal::Forbidden)
    }
}

/// A write refused as missing, from someone who could not have made the item
/// anyway, reads as what a free id gets: whether it was there is not theirs.
fn refused_write(r: Refusal, may_create: bool) -> ApiError {
    match r {
        Refusal::Missing | Refusal::OtherSpace | Refusal::InTrash(_) if !may_create => ApiError::forbidden(),
        r => r.into(),
    }
}

/// The same for a team write, where an id waiting in the trash says so to
/// one who may see it there; to anyone else it is just taken.
fn refused_team_write(c: &Caller, r: Refusal, may_create: bool) -> ApiError {
    match r {
        Refusal::InTrash(t) if c.may(Action::See, &trash_target(&t)) => ApiError::in_trash(),
        r => refused_write(r, may_create),
    }
}

/// A name or label as sent, refused when it is longer than anyone should see.
fn label(s: &str) -> Result<&str, ApiError> {
    if s.chars().count() > MAX_LABEL {
        Err(ApiError::bad("a name or label is too long"))
    } else {
        Ok(s)
    }
}

/// The folder a profile is written or moved into, trimmed as a folder's own
/// name is: "Ads " is Ads, and blank is no folder.
fn folder_arg(raw: &str) -> Result<&str, ApiError> {
    let name = label(raw.trim())?;
    // A URL drops these as path segments, so /admin/folders/.. could never reach it.
    if matches!(name, "." | "..") {
        return Err(ApiError::bad("a folder cannot be named . or .."));
    }
    Ok(name)
}

// ---- who hears of a change ----

/// The connected members, still on, whose rules pass `sees` as they are now.
async fn who_sees(st: &AppState, sees: impl Fn(&[Grant]) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for id in st.hub.listeners().await {
        match st.store.standing(&id) {
            Ok(s) if s.active && sees(&s.grants) => out.push(id),
            Ok(_) => {}
            // The change itself is made; a launcher that misses the news
            // catches up on its next refetch.
            Err(e) => tracing::error!("{e:#}"),
        }
    }
    out
}

async fn tell(st: &AppState, ids: &[String], what: &'static str, id: &str) {
    for m in ids {
        st.hub.send_to(m, Event::TeamChanged { what, id: id.to_owned() }).await;
    }
}

/// A change in the team goes to the members who may See the item and to
/// nobody else: that something hidden changed is itself not theirs to know.
async fn tell_team(st: &AppState, what: &'static str, id: &str, sees: impl Fn(&[Grant]) -> bool) {
    let ids = who_sees(st, sees).await;
    tell(st, &ids, what, id).await;
}

/// Whether the rules let a member See the profile in any of these folders:
/// before a change and after it, so one who just lost it hears too.
fn sees_profile(grants: &[Grant], id: &str, folders: &[&str]) -> bool {
    folders.iter().any(|f| allows(grants, Action::See, &Target::profile(id, f)))
}

/// Whether a member may know of a team folder, as the folder list has it.
fn sees_folder(grants: &[Grant], name: &str, profiles: &[TeamProfile]) -> bool {
    let ids = profiles.iter().filter(|p| p.folder == name).map(|p| p.id.as_str());
    view::sees_folder(grants, name, ids)
}

async fn who_sees_folder(st: &AppState, name: &str) -> Vec<String> {
    let profiles = st.store.profiles().unwrap_or_else(|e| {
        tracing::error!("{e:#}");
        Vec::new()
    });
    who_sees(st, |g| sees_folder(g, name, &profiles)).await
}

/// A row is born at revision 1, so the store's own answer says whether this
/// write made it — no second read that could disagree with the write.
fn written(revision: u64) -> StatusCode {
    if revision == 1 {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

/// `?base_revision=` on a DELETE: refused as stale once the item has moved past
/// it, like a write. Absent, there is no revision check.
#[derive(Deserialize, Default)]
struct BaseRevision {
    base_revision: Option<u64>,
}

/// A change in someone's own space goes to them alone: their other devices
/// refresh, and the team hears nothing of it.
async fn tell_owner(st: &AppState, member_id: &str, what: &'static str, id: String) {
    st.hub.send_to(member_id, Event::PersonalChanged { what, id }).await;
}

// ---- open ----

/// Alive, and which team: a launcher withdraws on a 401 only from its own team,
/// not from a server that came up empty at the same address.
async fn health(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "team": &*st.team }))
}

/// A launcher that does not hold this server's key yet asks it to prove it
/// issued the token, bound to the key it showed; the token comes only after.
async fn vouch(State(st): State<AppState>, Json(v): Json<team_wire::Vouch>) -> Result<Json<team_wire::Vouched>, ApiError> {
    let challenge = v.challenge_bytes().ok_or_else(|| ApiError::bad("a vouch needs a 32-byte challenge"))?;
    let hash = match (&v.member, &v.admin) {
        (Some(id), None) => st.store.token_hashes()?.into_iter().find(|h| team_wire::key_id(h) == *id),
        (None, Some(signed)) => auth::admin_token_of(&st.admin_secret, signed).map(|t| auth::token_hash(&t)),
        _ => return Err(ApiError::bad("a vouch names one token")),
    };
    let hash = hash.ok_or_else(ApiError::unknown_token)?;
    Ok(Json(team_wire::Vouched::new(&hash, &challenge, &st.key.public_raw())))
}

// ---- admin ----

#[derive(Deserialize)]
struct NewMember {
    name: String,
}

/// The one time a token exists outside the member's own machine: made, or
/// re-issued. No `Debug` on purpose: the easiest way for a secret to reach a
/// log is for something to print the struct that holds it.
#[derive(Serialize)]
struct CreatedMember {
    #[serde(flatten)]
    member: Member,
    token: String,
}

async fn members_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewMember>,
) -> Result<(StatusCode, Json<CreatedMember>), ApiError> {
    require_admin(&st, &headers)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("a member needs a name"));
    }
    let token = auth::new_member_token();
    let id = uuid::Uuid::new_v4().to_string();
    let member = st.store.add_member(&id, name, &auth::token_hash(&token))?;
    Ok((StatusCode::CREATED, Json(CreatedMember { member, token })))
}

/// A member as the admin's list shows them, with their whole rule table.
#[derive(Debug, Serialize)]
struct MemberRow {
    #[serde(flatten)]
    member: Member,
    grants: Vec<Grant>,
}

async fn members_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<MemberRow>>, ApiError> {
    require_admin(&st, &headers)?;
    // `Member` has no token and no hash in it, which is why this route can hand
    // the rows straight out.
    let rows = st.store.members_with_grants()?;
    Ok(Json(rows.into_iter().map(|(member, grants)| MemberRow { member, grants }).collect()))
}

#[derive(Deserialize)]
struct SetActive {
    active: bool,
}

/// The member as they now stand; `id` and `epoch` are where they always were.
async fn member_set_active(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetActive>,
) -> Result<Json<Member>, ApiError> {
    require_admin(&st, &headers)?;
    if st.store.member(&id)?.is_none() {
        return Err(ApiError::missing());
    }
    let (epoch, freed) = st.store.set_member_active(&id, body.active)?;
    // Switching off is the button the admin pressed to stop someone now, so it
    // goes down the socket rather than waiting out their last permission.
    // Switching back on moved the epoch too, so say so.
    let event = if body.active { Event::GrantsChanged { epoch } } else { Event::Revoked };
    st.hub.send_to(&id, event).await;
    tell_freed(&st, &freed).await;
    Ok(Json(st.store.member(&id)?.ok_or_else(ApiError::missing)?))
}

/// A new token for a member: the old one stops working at once, and their
/// sockets hear `token-replaced` and close. Rights, leases and their own
/// space stay: it is the same member. Answers as `POST /admin/members` does.
async fn member_reissue(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<CreatedMember>, ApiError> {
    require_admin(&st, &headers)?;
    let token = auth::new_member_token();
    let member = st.store.reissue_token(&id, &auth::token_hash(&token))?.ok_or_else(ApiError::missing)?;
    st.hub.send_to(&id, Event::TokenReplaced).await;
    Ok(Json(CreatedMember { member, token }))
}

/// Remove a member for good. Everything they made stays in the team — it was
/// never theirs — and the launcher is told at once, so it closes the team's
/// profiles it has open and wipes the copies it holds instead of waiting out
/// its last permission. The store frees whatever they had open with them.
async fn member_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&st, &headers)?;
    let Some(freed) = st.store.delete_member(&id)? else {
        return Err(ApiError::missing());
    };
    st.hub.send_to(&id, Event::Revoked).await;
    tell_freed(&st, &freed).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Tell leases that ran out unrenewed like ones given back, so lists stop
/// showing them open. Run periodically; answers how many there were.
pub async fn reap_leases(st: &AppState) -> anyhow::Result<usize> {
    let gone = st.store.reap_leases()?;
    for (id, lease) in &gone {
        match st.store.profile_meta(id)? {
            Some((p, space)) if space == TEAM => {
                tell_team(st, "lease", id, |g| sees_profile(g, id, &[&p.folder])).await
            }
            Some((_, space)) if space == personal_space(&lease.member_id) => {
                tell_owner(st, &lease.member_id, "lease", id.clone()).await
            }
            _ => {}
        }
    }
    Ok(gone.len())
}

/// Leases freed with a member: the team's profiles among them are no longer open anywhere.
async fn tell_freed(st: &AppState, ids: &[String]) {
    for id in ids {
        match st.store.profile_meta(id) {
            Ok(Some((p, space))) if space == TEAM => {
                tell_team(st, "lease", id, |g| sees_profile(g, id, &[&p.folder])).await
            }
            Ok(_) => {}
            Err(e) => tracing::error!("{e:#}"),
        }
    }
}

#[derive(Deserialize)]
struct TeamRename {
    name: String,
}

/// The team's name, trimmed: 1 to 64 characters, or None.
pub fn team_name_arg(raw: &str) -> Option<&str> {
    let name = raw.trim();
    let n = name.chars().count();
    (1..=MAX_TEAM_NAME).contains(&n).then_some(name)
}

/// What the team is called now.
fn team_name(st: &AppState) -> Result<String, ApiError> {
    Ok(st.store.meta_get(META_TEAM_NAME)?.unwrap_or_else(|| DEFAULT_TEAM_NAME.into()))
}

#[derive(Debug, Serialize)]
struct TeamInfo {
    id: String,
    name: String,
}

/// Rename the team. Everyone connected hears it and refetches `/me`.
async fn team_rename(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TeamRename>,
) -> Result<Json<TeamInfo>, ApiError> {
    require_admin(&st, &headers)?;
    let name = team_name_arg(&body.name)
        .ok_or_else(|| ApiError::bad("a team name is 1 to 64 characters"))?;
    st.store.meta_set_all(&[(META_TEAM_NAME, name)])?;
    let ids = who_sees(&st, |_| true).await;
    tell(&st, &ids, "team", &st.team).await;
    Ok(Json(TeamInfo { id: st.team.to_string(), name: name.into() }))
}

#[derive(Deserialize)]
struct SetGrants {
    grants: Vec<Grant>,
}

/// A table as stored: duplicates merged, a refusal beside an allow kept as the refusal.
#[derive(Debug, Serialize)]
struct GrantsAck {
    id: String,
    epoch: u64,
    grants: Vec<Grant>,
}

/// A member's whole rule table, every scope, in one fixed order.
fn stored_grants(st: &AppState, id: &str) -> Result<Vec<Grant>, ApiError> {
    let mut g = st.store.grants_of(id)?;
    view::sort_rules(&mut g);
    Ok(g)
}

async fn member_grants(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<Grant>>, ApiError> {
    require_admin(&st, &headers)?;
    if st.store.member(&id)?.is_none() {
        return Err(ApiError::missing());
    }
    Ok(Json(stored_grants(&st, &id)?))
}

async fn member_set_grants(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetGrants>,
) -> Result<Json<GrantsAck>, ApiError> {
    require_admin(&st, &headers)?;
    if st.store.member(&id)?.is_none() {
        return Err(ApiError::missing());
    }
    let epoch = st.store.set_grants(&id, &body.grants)?;
    st.hub.send_to(&id, Event::GrantsChanged { epoch }).await;
    let grants = stored_grants(&st, &id)?;
    Ok(Json(GrantsAck { id, epoch, grants }))
}

async fn admin_folder_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewFolder>,
) -> Result<(StatusCode, Json<Folder>), ApiError> {
    require_admin(&st, &headers)?;
    let f = make_folder(&st, TEAM, &body)?;
    tell_regranted(&st).await;
    let ids = who_sees_folder(&st, &f.name).await;
    tell(&st, &ids, "folder", &f.name).await;
    Ok((StatusCode::CREATED, Json(f)))
}

async fn admin_folder_change(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<FolderChange>,
) -> Result<Json<Folder>, ApiError> {
    require_admin(&st, &headers)?;
    let f = change_folder(&st, TEAM, &name, &body).await?;
    // The rules moved with the name, so those who see it now are those who saw it.
    let ids = who_sees_folder(&st, &f.name).await;
    tell(&st, &ids, "folder", &f.name).await;
    Ok(Json(f))
}

async fn admin_folder_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&st, &headers)?;
    // Asked first: the rules that let someone see it go with it.
    let ids = who_sees_folder(&st, &name).await;
    remove_folder(&st, TEAM, &name).await?;
    tell(&st, &ids, "folder", &name).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the permission a launcher enforces ----

/// Also without `Debug`: the signed permission is a credential while it lasts.
#[derive(Serialize)]
struct IssuedPermission {
    permission: String,
    epoch: u64,
    exp: u64,
}

async fn permission_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<IssuedPermission>, ApiError> {
    let c = require_member(&st, &headers)?;
    // Only the rules about what they can see: a refusal naming a hidden
    // folder or profile would tell them it is there.
    let (epoch, grants) = if c.admin {
        (c.member.epoch, c.grants)
    } else {
        match st.store.permission_view(&c.member.id)? {
            None => return Err(ApiError::unauthorized()),
            Some(v) if !v.active => return Err(ApiError::switched_off()),
            Some(v) => (v.epoch, v.rules),
        }
    };
    let p = auth::permission_for(&c.member.id, &st.team, epoch, grants, PERMISSION_TTL_SECS);
    let (epoch, exp) = (p.epoch, p.exp);
    let permission = st.key.sign(&p)?;
    Ok(Json(IssuedPermission { permission, epoch, exp }))
}

// ---- who am I ----

#[derive(Debug, Serialize)]
struct MemberInfo {
    id: String,
    name: String,
}

/// What a launcher binds with: the team, who it is there, and the key that
/// checks every permission it will be given.
#[derive(Debug, Serialize)]
struct Me {
    team: TeamInfo,
    /// None for the admin token, which acts as the admin principal.
    member: Option<MemberInfo>,
    admin: bool,
    perm_key: String,
    /// The address the owner said launchers reach the server at; the admin's only.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_url: Option<String>,
}

async fn me_get(State(st): State<AppState>, headers: HeaderMap) -> Result<Json<Me>, ApiError> {
    let c = require_member(&st, &headers)?;
    let member = (!c.admin).then(|| MemberInfo { id: c.member.id.clone(), name: c.member.name.clone() });
    Ok(Json(Me {
        team: TeamInfo { id: st.team.to_string(), name: team_name(&st)? },
        member,
        admin: c.admin,
        perm_key: st.key.public_b64(),
        public_url: st.public_url.as_deref().filter(|_| c.admin).map(str::to_owned),
    }))
}

// ---- the live channel ----

async fn live_ws(
    State(st): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Checked before the upgrade: a socket that is going to be refused should
    // never become a socket.
    let (rx, seat) = listen(&st, &headers).await?;
    let ws = ws.max_message_size(MAX_LIVE_IN).max_frame_size(MAX_LIVE_IN);
    Ok(ws.on_upgrade(move |socket| pump(socket, rx, seat)))
}

/// Listen first, then check the member again: a switch-off during the upgrade
/// is either caught by that check or already waiting in the channel. The seat
/// keeps a member to [`MAX_SOCKETS`], closing their oldest past it.
async fn listen(st: &AppState, headers: &HeaderMap) -> Result<(broadcast::Receiver<Event>, Seat), ApiError> {
    let c = require_member(st, headers)?;
    let rx = st.hub.subscribe(&c.member.id).await;
    require_member(st, headers)?;
    Ok((rx, st.hub.seat(&c.member.id, MAX_SOCKETS)))
}

async fn pump(mut socket: WebSocket, mut rx: broadcast::Receiver<Event>, seat: Seat) {
    loop {
        tokio::select! {
            // A newer socket of theirs took this one's place.
            _ = seat.closed.notified() => break,
            incoming = socket.recv() => match incoming {
                // Gone, or broken. Either way we are done; the launcher
                // reconnects and asks for a fresh permission.
                None | Some(Err(_)) => break,
                // The launcher has nothing to tell us here. Pings are answered
                // by axum underneath us.
                Some(Ok(_)) => {}
            },
            event = rx.recv() => match event {
                Err(broadcast::error::RecvError::Closed) => break,
                // We fell behind and some events are already gone. There is no
                // way to say "you missed something", so drop the socket: a
                // reconnect refetches everything, which is the honest answer.
                Err(broadcast::error::RecvError::Lagged(_)) => break,
                Ok(event) => {
                    let last = matches!(event, Event::Revoked | Event::TokenReplaced);
                    let Ok(text) = serde_json::to_string(&event) else { break };
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                    if last {
                        // Their channel is cut: switched off, or opened with a
                        // token that no longer works. Said first, then closed.
                        break;
                    }
                }
            },
        }
    }
}

// ---- profiles ----

#[derive(Deserialize)]
struct ProfileWrite {
    name: String,
    /// Absent means no folder, which is a real place for a profile to be.
    #[serde(default)]
    folder: String,
    /// Base64 (standard alphabet, padded) of the profile's settings; the
    /// session is not in it but at `/session`. Sealed at rest by the store.
    data: String,
    /// The revision this write was made from. Given, the write is refused once
    /// the profile has moved past it, so a device that slept through someone
    /// else's save cannot undo it. Absent, only the lease is checked.
    base_revision: Option<u64>,
    /// What lists show; its `proxy_id` is the binding. Absent keeps the stored one.
    card: Option<Card>,
    /// When the profile was made, unix seconds, as the writer knows it.
    #[serde(default)]
    created_at: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ProfileWithData {
    #[serde(flatten)]
    row: ProfileRow,
    data: String,
}

fn data_arg(data: &str) -> Result<Vec<u8>, ApiError> {
    STANDARD
        .decode(data.as_bytes())
        .map_err(|_| ApiError::bad("data is not base64"))
}

/// A card within its limits, with a blank binding read as none.
fn card_arg(card: Option<&Card>) -> Result<Option<Card>, ApiError> {
    let Some(c) = card else { return Ok(None) };
    if let Some(why) = c.problem() {
        return Err(ApiError::bad(why));
    }
    Ok(Some(Card { proxy_id: c.bound_proxy().map(str::to_owned), ..c.clone() }))
}

type ProfileArgs<'a> = (&'a str, &'a str, Vec<u8>, Option<Card>);

fn profile_args(body: &ProfileWrite) -> Result<ProfileArgs<'_>, ApiError> {
    let name = label(body.name.trim())?;
    if name.is_empty() {
        return Err(ApiError::bad("a profile needs a name"));
    }
    let card = card_arg(body.card.as_ref())?;
    Ok((name, folder_arg(&body.folder)?, data_arg(&body.data)?, card))
}

/// Binding a proxy hands its login to whoever may Start: only one the writer may use.
fn may_bind(grants: &[Grant], bound: Option<&str>) -> Result<(), Refusal> {
    match bound {
        Some(x) if !may_use_proxy(grants, x) => Err(Refusal::BadProxy),
        _ => Ok(()),
    }
}

/// Find a team profile the member is allowed to know about. Missing, invisible
/// and somebody's own all come back the same way on purpose.
fn visible_profile(st: &AppState, c: &Caller, id: &str) -> Result<TeamProfile, ApiError> {
    match st.store.profile_meta(id)? {
        Some((p, space))
            if space == TEAM && c.may(Action::See, &Target::profile(id, &p.folder)) =>
        {
            Ok(p)
        }
        _ => Err(ApiError::missing()),
    }
}

/// Where a profile is open right now: the lease, and whose it is by name.
#[derive(Debug, Serialize)]
struct OpenOn {
    #[serde(flatten)]
    lease: Lease,
    member_name: String,
}

/// A row of a profile list: its fields, its card, its session's revision (0
/// for none) and where it is open, if anywhere. Never the session itself.
#[derive(Debug, Serialize)]
struct ProfileRow {
    #[serde(flatten)]
    profile: TeamProfile,
    card: Card,
    session_rev: u64,
    session_size: u64,
    open_on: Option<OpenOn>,
}

/// Who is called what, the admin principal included.
fn member_names(st: &AppState) -> Result<HashMap<String, String>, ApiError> {
    let mut names: HashMap<String, String> =
        st.store.members()?.into_iter().map(|m| (m.id, m.name)).collect();
    names.insert(ADMIN_ID.into(), ADMIN_NAME.into());
    Ok(names)
}

fn with_open_on(st: &AppState, listed: Vec<Listed>) -> Result<Vec<ProfileRow>, ApiError> {
    let mut leases: HashMap<String, Lease> = st.store.live_leases()?.into_iter().collect();
    let names = member_names(st)?;
    Ok(listed
        .into_iter()
        .map(|l| {
            let open_on = leases.remove(&l.profile.id).map(|lease| OpenOn {
                member_name: names.get(&lease.member_id).cloned().unwrap_or_default(),
                lease,
            });
            ProfileRow {
                session_rev: l.session.as_ref().map_or(0, |s| s.rev),
                session_size: l.session.as_ref().map_or(0, |s| s.size),
                card: l.card,
                profile: l.profile,
                open_on,
            }
        })
        .collect())
}

/// One profile's row as the lists have it; missing unless it is in `space`.
fn one_row(st: &AppState, space: &str, id: &str) -> Result<ProfileRow, ApiError> {
    match st.store.listed_one(id)? {
        Some((l, sp)) if sp == space => with_open_on(st, vec![l])?.pop().ok_or_else(ApiError::missing),
        _ => Err(ApiError::missing()),
    }
}

/// `?ids=a,b,c`: only these, at most [`MAX_IDS`]. Absent: the whole list.
#[derive(Deserialize, Default)]
struct ListQuery {
    ids: Option<String>,
}

fn ids_arg(raw: &str) -> Result<Vec<&str>, ApiError> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.len() > MAX_IDS {
        return Err(ApiError::bad("at most 500 ids at a time"));
    }
    Ok(parts.into_iter().collect::<BTreeSet<_>>().into_iter().collect())
}

async fn profiles_list(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<ProfileRow>>, ApiError> {
    let c = require_member(&st, &headers)?;
    let ids = q.ids.as_deref().map(ids_arg).transpose()?;
    let rows = st.store.listed_in(TEAM, ids.as_deref())?;
    // An id the caller may not See is left out, as one that is not there.
    let visible = rows
        .into_iter()
        .filter(|l| c.may(Action::See, &Target::profile(&l.profile.id, &l.profile.folder)))
        .collect();
    // Metadata only: the data bytes come one at a time, from the route below.
    Ok(Json(with_open_on(&st, visible)?))
}

async fn profile_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProfileWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    let profile = visible_profile(&st, &c, &id)?;
    let t = Target::profile(&id, &profile.folder);
    if !USES_A_PROFILE.iter().any(|a| c.may(*a, &t)) {
        return Err(ApiError::forbidden());
    }
    let row = one_row(&st, TEAM, &id)?;
    let data = st.store.profile_data(&id)?.ok_or_else(ApiError::missing)?;
    Ok(Json(ProfileWithData { row, data: STANDARD.encode(data) }))
}

async fn profile_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProfileWrite>,
) -> Result<(StatusCode, Json<TeamProfile>), ApiError> {
    let c = require_member(&st, &headers)?;
    let (name, folder, data, card) = profile_args(&body)?;
    let fields = ProfileFields { name, folder, data: &data, card: card.as_ref(), created_at: body.created_at };

    // Rights, lease and revision are all decided inside the write's transaction,
    // so a profile made a moment ago by someone else is an Edit, not a Create.
    let mut was: Option<String> = None;
    let r = st.store.put_profile_with(
        TEAM,
        &id,
        &fields,
        c.writer(),
        body.base_revision,
        |s, existing, found| {
            let g = live(s)?;
            was = existing.map(|p| p.folder.clone());
            match existing {
                // Against the folder it is in now, never the one in the body;
                // and a move asks about where it lands too.
                Some(p) => {
                    see_then(g, Action::Edit, &Target::profile(&id, &p.folder))?;
                    if p.folder != folder {
                        may_move(g, &id, &p.folder, folder)?;
                    }
                }
                // New: Create where it lands, and Edit there if a rule already names it.
                None if allows(g, Action::Create, &Target::folder(folder)) => {
                    if found.named {
                        may_take_name(g, &Target::profile(&id, folder))?;
                    }
                }
                None => return Err(Refusal::Forbidden),
            }
            may_bind(g, found.bound)
        },
    )?;
    let p = r.map_err(|e| refused_team_write(&c, e, c.may(Action::Create, &Target::folder(folder))))?;
    tell_regranted(&st).await;
    let folders: Vec<&str> = was.iter().map(String::as_str).chain([folder]).collect();
    tell_team(&st, "profile", &id, |g| sees_profile(g, &id, &folders)).await;
    Ok((written(p.revision), Json(p)))
}

async fn profile_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    let mut was = String::new();
    st.store.delete_profile_checked(TEAM, &id, c.writer(), at.base_revision, |s, p| {
        was = p.folder.clone();
        see_then(live(s)?, Action::Delete, &Target::profile(&id, &p.folder))
    })??;
    tell_regranted(&st).await;
    // Gone from the list, and into the trash, for the same people.
    let ids = who_sees(&st, |g| sees_profile(g, &id, &[&was])).await;
    tell(&st, &ids, "profile", &id).await;
    tell(&st, &ids, "trash", &id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- leases ----

/// Open a profile on the caller's device, or renew the lease that device
/// already holds. Busy is answered with who has it open and where. The space
/// and `authorize` are judged again with the lease, under one lock.
async fn take_lease(
    st: &AppState,
    c: &Caller,
    space: &str,
    id: &str,
    authorize: impl FnOnce(&Standing, &TeamProfile) -> Result<(), Refusal>,
) -> Result<Json<Lease>, ApiError> {
    let device_id = c.device.id.as_deref().ok_or_else(ApiError::no_device)?;
    let mut folder = String::new();
    let taken = st.store.lease_take(
        space,
        id,
        &c.member.id,
        device_id,
        &c.device.name,
        LEASE_TTL_SECS,
        |s, p| {
            folder = p.folder.clone();
            authorize(s, p)
        },
    )??;
    // An open is news for the lists; a renewal every half minute is not.
    if taken.fresh {
        tell_lease(st, c, space, id, &folder).await;
    }
    Ok(Json(taken.lease))
}

/// A lease taken or given back: those who may See the profile hear of it, or
/// in a personal space its owner's other devices.
async fn tell_lease(st: &AppState, c: &Caller, space: &str, id: &str, folder: &str) {
    if space == TEAM {
        tell_team(st, "lease", id, |g| sees_profile(g, id, &[folder])).await;
    } else {
        tell_owner(st, &c.member.id, "lease", id.to_owned()).await;
    }
}

/// Give a lease back. Only the device holding it can; anyone else is told who
/// does, as when taking one. No right is asked for giving back your own: a
/// launcher whose Start was just withdrawn closes the profile and has to be
/// able to say so, rather than keep everyone out for the rest of the lease.
async fn give_back(
    st: &AppState,
    c: &Caller,
    space: &str,
    id: &str,
) -> Result<StatusCode, ApiError> {
    let p = match st.store.profile_meta(id)? {
        Some((p, sp)) if sp == space => p,
        _ => return Err(ApiError::missing()),
    };
    // Whether they may know it is there comes before anything else the
    // answer could tell them.
    let hidden = space == TEAM && !c.may(Action::See, &Target::profile(id, &p.folder));
    let Some(device_id) = c.device.id.as_deref() else {
        return Err(if hidden { ApiError::missing() } else { ApiError::no_device() });
    };
    if st.store.lease_release(id, &c.member.id, device_id)? {
        tell_lease(st, c, space, id, &p.folder).await;
        return Ok(StatusCode::NO_CONTENT);
    }
    // Not theirs. Who does hold it is only for someone who may know the
    // profile is there at all.
    if hidden {
        return Err(ApiError::missing());
    }
    match st.store.lease_live(id)? {
        Some(held) => Err(Refusal::Leased(held).into()),
        // Nobody has it open, which is already what was asked for.
        None => Ok(StatusCode::NO_CONTENT),
    }
}

async fn lease_take(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Lease>, ApiError> {
    let c = require_member(&st, &headers)?;
    let p = visible_profile(&st, &c, &id)?;
    if !c.may(Action::Start, &Target::profile(&id, &p.folder)) {
        return Err(ApiError::forbidden());
    }
    // Again under the lock: it may have moved, or the right gone, since.
    take_lease(&st, &c, TEAM, &id, |s, p| {
        see_then(live(s)?, Action::Start, &Target::profile(&id, &p.folder))
    })
    .await
}

async fn lease_give_back(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    give_back(&st, &c, TEAM, &id).await
}

/// `?takeover=true`: one of the member's own devices takes the profile from another.
#[derive(Deserialize, Default)]
struct Takeover {
    #[serde(default)]
    takeover: bool,
}

async fn my_lease_take(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Takeover>,
) -> Result<Json<Lease>, ApiError> {
    let c = require_member(&st, &headers)?;
    own_profile(&st, &c, &id)?;
    if !q.takeover {
        return take_lease(&st, &c, &c.mine(), &id, |s, _| still_on(s)).await;
    }
    // The device that had it hears `leased` at its next renewal, and closes it.
    let device_id = c.device.id.as_deref().ok_or_else(ApiError::no_device)?;
    let taken = st.store.lease_take_over(
        &c.mine(),
        &id,
        &c.member.id,
        device_id,
        &c.device.name,
        LEASE_TTL_SECS,
        |s, _| still_on(s),
    )??;
    tell_owner(&st, &c.member.id, "lease", id).await;
    Ok(Json(taken.lease))
}

async fn my_lease_give_back(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    give_back(&st, &c, &c.mine(), &id).await
}

// ---- sessions and other large blobs ----
//
// A session or package moves a chunk at a time, in and out: nothing holds a
// whole one in memory, and the store is held for one chunk at a time.

/// Run a store call that moves a large blob off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    Ok(tokio::task::spawn_blocking(f).await.map_err(anyhow::Error::from)??)
}

/// A body this request holds in the store, let go when dropped; what nothing
/// points at any more is collected then.
struct Held {
    store: Arc<Store>,
    body: i64,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.store.let_go(self.body);
        tidy(&self.store);
    }
}

/// Collect the bodies nothing points at any more, off the async workers.
fn tidy(store: &Arc<Store>) {
    let store = store.clone();
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn_blocking(move || {
            if let Err(e) = store.collect_bodies() {
                tracing::warn!("blobs: {e:#}");
            }
        });
    }
}

/// A stored blob sent as it is read, the front first and then a chunk at a
/// time. Its hold on the body goes once the last chunk is read, or with it.
struct Streamed {
    store: Arc<Store>,
    front: Option<Bytes>,
    body: Option<BodyRef>,
    held: Option<Held>,
    next: u64,
    left: u64,
    reading: Option<tokio::task::JoinHandle<anyhow::Result<Vec<u8>>>>,
}

impl Streamed {
    fn new(store: Arc<Store>, front: Vec<u8>, body: Option<BodyRef>, held: Option<Held>) -> Self {
        let left = front.len() as u64 + body.as_ref().map_or(0, |b| b.size);
        let front = (!front.is_empty()).then(|| Bytes::from(front));
        Streamed { store, front, body, held, next: 0, left, reading: None }
    }
}

impl HttpBody for Streamed {
    type Data = Bytes;
    type Error = anyhow::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        use std::future::Future;
        use std::task::Poll;
        let this = self.get_mut();
        if let Some(front) = this.front.take() {
            this.left -= front.len() as u64;
            return Poll::Ready(Some(Ok(Frame::data(front))));
        }
        loop {
            if let Some(reading) = this.reading.as_mut() {
                let got = std::task::ready!(std::pin::Pin::new(reading).poll(cx));
                this.reading = None;
                let chunk = match got.map_err(anyhow::Error::from).and_then(|r| r) {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        // Cut short: the declared length tells the client it did not get it all.
                        tracing::error!("{e:#}");
                        this.body = None;
                        this.held = None;
                        return Poll::Ready(Some(Err(e)));
                    }
                };
                this.next += 1;
                if chunk.is_empty() {
                    continue;
                }
                this.left -= chunk.len() as u64;
                return Poll::Ready(Some(Ok(Frame::data(Bytes::from(chunk)))));
            }
            let Some(body) = this.body.as_ref().filter(|b| this.next < b.chunks()) else {
                this.held = None;
                return Poll::Ready(None);
            };
            let (store, body, seq) = (this.store.clone(), body.clone(), this.next);
            this.reading = Some(tokio::task::spawn_blocking(move || store.body_chunk(&body, seq)));
        }
    }

    fn is_end_stream(&self) -> bool {
        self.left == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.left)
    }
}

/// How often an upload renews the lease it is made under while it comes in.
const RENEW_EVERY: std::time::Duration = std::time::Duration::from_secs(20);

/// The lease an upload is made under: kept alive while the body comes in.
struct UnderLease {
    id: String,
    member_id: String,
    device_id: String,
}

impl UnderLease {
    /// Renew it, as the launcher would; an error saying why when it is gone.
    fn renew(&self, st: &AppState) -> Result<(), ApiError> {
        if st.store.lease_extend(&self.id, &self.member_id, &self.device_id, LEASE_TTL_SECS)? {
            return Ok(());
        }
        // Removed or switched off took it, or another device has it now, or it ran out.
        let s = st.store.standing(&self.member_id)?;
        Err(match live(&s) {
            Err(r) => r.into(),
            Ok(_) => st.store.lease_live(&self.id)?.map_or(Refusal::NoLease, Refusal::Leased).into(),
        })
    }
}

/// An upload and this request's hold on its body. It moves into each blocking
/// step whole, so a request dropped meanwhile lets go only after that step.
struct Incoming {
    up: Upload,
    _held: Held,
}

impl Incoming {
    fn new(st: &AppState, up: Upload) -> Self {
        let held = Held { store: st.store.clone(), body: up.body };
        Incoming { up, _held: held }
    }
}

/// Keep one chunk of an upload, off the async workers.
async fn put_chunk(st: &AppState, mut inc: Incoming, chunk: Vec<u8>, last: bool) -> Result<Incoming, ApiError> {
    let store = st.store.clone();
    blocking(move || {
        store.upload_put(&mut inc.up, &chunk, last)?;
        Ok(inc)
    })
    .await
}

/// A body that says up front it is past `limit` is refused before anything is made for it.
fn declared_within(headers: &HeaderMap, limit: usize) -> Result<(), ApiError> {
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > limit as u64) {
        Err(ApiError::too_large())
    } else {
        Ok(())
    }
}

/// Take a raw body into an upload a chunk at a time, up to `limit`, pausing at
/// most [`BODY_IDLE`] between pieces; `lease` is renewed while it comes.
async fn receive(
    st: &AppState,
    mut inc: Incoming,
    mut body: Body,
    limit: usize,
    lease: Option<&UnderLease>,
) -> Result<Incoming, ApiError> {
    if let Some(l) = lease {
        l.renew(st)?;
    }
    let mut renew = tokio::time::interval_at(tokio::time::Instant::now() + RENEW_EVERY, RENEW_EVERY);
    let mut idle = tokio::time::Instant::now() + BODY_IDLE;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let next = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx));
        tokio::select! {
            frame = next => {
                let Some(frame) = frame else { break };
                let frame = frame.map_err(|_| ApiError::bad("the body broke off"))?;
                idle = tokio::time::Instant::now() + BODY_IDLE;
                // Anything but data (trailers) carries nothing for us.
                let Ok(data) = frame.into_data() else { continue };
                if inc.up.size + (buf.len() + data.len()) as u64 > limit as u64 {
                    return Err(ApiError::too_large());
                }
                buf.extend_from_slice(&data);
                // Only a chunk with more behind it is whole for sure: the last waits for the end.
                while buf.len() > CHUNK {
                    let rest = buf.split_off(CHUNK);
                    inc = put_chunk(st, inc, std::mem::replace(&mut buf, rest), false).await?;
                }
            }
            _ = tokio::time::sleep_until(idle) => {
                return Err(ApiError::new(StatusCode::REQUEST_TIMEOUT, code::BAD_REQUEST, "the body stalled"));
            }
            _ = renew.tick(), if lease.is_some() => {
                if let Some(l) = lease {
                    l.renew(st)?;
                }
            }
        }
    }
    put_chunk(st, inc, buf, true).await
}

/// `?base_session_rev=`: what the upload was made from, 0 before the first. Required.
#[derive(Deserialize, Default)]
struct BaseSession {
    base_session_rev: Option<u64>,
}

/// A session's own revision, as an upload answers and a download begins.
#[derive(Debug, Serialize)]
struct SessionInfo {
    session_rev: u64,
    size: u64,
    updated_at: u64,
    updated_by: String,
}

fn session_info(m: Option<&BlobMeta>) -> SessionInfo {
    SessionInfo {
        session_rev: m.map_or(0, |m| m.rev),
        size: m.map_or(0, |m| m.size),
        updated_at: m.map_or(0, |m| m.updated_at),
        updated_by: m.map(|m| m.updated_by.clone()).unwrap_or_default(),
    }
}

/// The JSON in front of a downloaded session. No `Debug`: it carries the proxy's login.
#[derive(Serialize)]
struct SessionFront {
    #[serde(flatten)]
    info: SessionInfo,
    proxy_id: Option<String>,
    proxy: Option<ProxyWithData>,
}

/// A `u32` big-endian length, then that much JSON: what goes before the bytes.
fn session_front(h: &SessionHead) -> anyhow::Result<Vec<u8>> {
    let front = SessionFront {
        info: session_info(h.session.as_ref()),
        proxy_id: h.proxy_id.clone(),
        proxy: h.proxy.as_ref().map(|(x, data)| ProxyWithData {
            proxy: x.clone().into(),
            data: STANDARD.encode(data),
        }),
    };
    let json = serde_json::to_vec(&front)?;
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&u32::try_from(json.len())?.to_be_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

/// The session, for the device that has the profile open: Start, and the lease.
async fn session_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let c = require_member(&st, &headers)?;
    let p = visible_profile(&st, &c, &id)?;
    if !c.may(Action::Start, &Target::profile(&id, &p.folder)) {
        return Err(ApiError::forbidden());
    }
    // Again under the read's lock: it may have moved, or the right gone, since.
    let key = id.clone();
    read_session(&st, &c, TEAM, id, move |s, p| {
        see_then(live(s)?, Action::Start, &Target::profile(&key, &p.folder))
    })
    .await
}

async fn my_session_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let c = require_member(&st, &headers)?;
    own_profile(&st, &c, &id)?;
    read_session(&st, &c, &c.mine(), id, |s, _| still_on(s)).await
}

async fn read_session(
    st: &AppState,
    c: &Caller,
    space: &str,
    id: String,
    authorize: impl FnOnce(&Standing, &TeamProfile) -> Result<(), Refusal>,
) -> Result<Response, ApiError> {
    let device = c.device.id.as_deref().ok_or_else(ApiError::no_device)?;
    let reader = Writer { member_id: &c.member.id, device_id: Some(device) };
    // Judged and the body held in one short call: nothing waits for the bytes.
    let (head, body) = st.store.session_open(space, &id, reader, authorize)??;
    let held = body.as_ref().map(|b| Held { store: st.store.clone(), body: b.body });
    let front = session_front(&head)?;
    let stream = Streamed::new(st.store.clone(), front, body, held);
    Ok(([(CONTENT_TYPE, HeaderValue::from_static(SESSION_TYPE))], Body::new(stream)).into_response())
}

async fn session_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<BaseSession>,
    body: Body,
) -> Result<Json<SessionInfo>, ApiError> {
    let c = require_member(&st, &headers)?;
    write_session(&st, &c, TEAM, id, q, &headers, body).await
}

async fn my_session_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<BaseSession>,
    body: Body,
) -> Result<Json<SessionInfo>, ApiError> {
    let c = require_member(&st, &headers)?;
    let mine = c.mine();
    write_session(&st, &c, &mine, id, q, &headers, body).await
}

/// Take a session from the device holding the lease, whatever its rights did
/// since it opened. Refused before the body where that can be told; judged
/// again under the write's lock, where a removed member is refused too.
/// The lease is renewed while the body comes in.
async fn write_session(
    st: &AppState,
    c: &Caller,
    space: &str,
    id: String,
    q: BaseSession,
    headers: &HeaderMap,
    body: Body,
) -> Result<Json<SessionInfo>, ApiError> {
    let team = space == TEAM;
    let p = match st.store.profile_meta(&id)? {
        Some((p, sp)) if sp == space => p,
        _ => return Err(ApiError::missing()),
    };
    let lease = st.store.lease_live(&id)?;
    let holds = lease.as_ref().is_some_and(|l| {
        l.member_id == c.member.id && c.device.id.as_deref() == Some(l.device_id.as_str())
    });
    // Who holds it is only for someone who may know the profile is there.
    if team && !holds && !c.may(Action::See, &Target::profile(&id, &p.folder)) {
        return Err(ApiError::missing());
    }
    let base = q
        .base_session_rev
        .ok_or_else(|| ApiError::bad("an upload needs ?base_session_rev="))?;
    let device = c.device.id.clone().ok_or_else(ApiError::no_device)?;
    if !holds {
        return Err(lease.map_or(Refusal::NoLease, Refusal::Leased).into());
    }

    declared_within(headers, st.session_max)?;
    let lease = UnderLease { id: id.clone(), member_id: c.member.id.clone(), device_id: device.clone() };
    let inc = Incoming::new(st, st.store.session_upload(&id)?);
    let inc = receive(st, inc, body, st.session_max, Some(&lease)).await?;
    let (store, member, sp, key) = (st.store.clone(), c.member.id.clone(), space.to_owned(), id.clone());
    // The hold goes after the commit, whatever happens to this request.
    let (p, meta) = blocking(move || {
        let writer = Writer { member_id: &member, device_id: Some(&device) };
        store.session_commit(&sp, &key, writer, base, &inc.up, |s, p, holds| {
            let g = live(s)?;
            if team && !holds && !allows(g, Action::See, &Target::profile(&p.id, &p.folder)) {
                Err(Refusal::Missing)
            } else {
                Ok(())
            }
        })
    })
    .await??;
    if team {
        tell_team(st, "session", &id, |g| sees_profile(g, &id, &[&p.folder])).await;
    } else {
        tell_owner(st, &c.member.id, "session", id).await;
    }
    Ok(Json(session_info(Some(&meta))))
}

// ---- folders ----

#[derive(Deserialize)]
struct NewFolder {
    name: String,
    #[serde(default)]
    color: String,
}

/// Rename, recolor, or both.
#[derive(Deserialize)]
struct FolderChange {
    name: Option<String>,
    color: Option<String>,
}

fn folder_name(raw: &str) -> Result<&str, ApiError> {
    let name = folder_arg(raw)?;
    // "" is where a profile in no folder already is; it is not one to make.
    if name.is_empty() {
        Err(ApiError::bad("a folder needs a name"))
    } else {
        Ok(name)
    }
}

fn find_folder(st: &AppState, space: &str, name: &str) -> Result<Folder, ApiError> {
    st.store
        .folders_in(space)?
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(ApiError::missing)
}

fn make_folder(st: &AppState, space: &str, body: &NewFolder) -> Result<Folder, ApiError> {
    let name = folder_name(&body.name)?;
    if !st.store.create_folder(space, name, label(&body.color)?)? {
        return Err(ApiError::folder_taken());
    }
    find_folder(st, space, name)
}

async fn change_folder(
    st: &AppState,
    space: &str,
    name: &str,
    body: &FolderChange,
) -> Result<Folder, ApiError> {
    if body.name.is_none() && body.color.is_none() {
        return Err(ApiError::bad("nothing to change"));
    }
    // Both checked before either is made, so a bad colour leaves no rename behind.
    let color = body.color.as_deref().map(label).transpose()?;
    let mut now = name.to_owned();
    // The rename goes first: a name in use is the likely refusal, and this way
    // it leaves nothing half-done.
    if let Some(to) = &body.name {
        let to = folder_name(to)?;
        // The store would call a folder's own name taken.
        if to != name {
            match st.store.rename_folder(space, name, to)? {
                FolderRename::Done => tell_regranted(st).await,
                FolderRename::Missing => return Err(ApiError::missing()),
                FolderRename::NameTaken => return Err(ApiError::folder_taken()),
            }
            now = to.to_owned();
        }
    }
    if let Some(color) = color {
        if !st.store.set_folder_color(space, &now, color)? {
            return Err(ApiError::missing());
        }
    }
    find_folder(st, space, &now)
}

async fn remove_folder(st: &AppState, space: &str, name: &str) -> Result<(), ApiError> {
    match st.store.delete_folder(space, name)? {
        FolderDelete::Done => {
            tell_regranted(st).await;
            Ok(())
        }
        FolderDelete::Missing => Err(ApiError::missing()),
        // Where its profiles should go is the person's call, not ours.
        FolderDelete::NotEmpty(_) => Err(ApiError::conflict("the folder is not empty")),
        // Its rules still judge what was deleted from it.
        FolderDelete::InTrash(_) => Err(ApiError::conflict("the folder's deleted profiles wait in the trash")),
    }
}

/// Tell each member whose permission a change moved their new epoch, so the
/// launcher refetches rather than enforce rules about what it no longer sees.
async fn tell_regranted(st: &AppState) {
    for (m, epoch) in st.store.take_regranted() {
        st.hub.send_to(&m, Event::GrantsChanged { epoch }).await;
    }
}

/// The team's folders this member may know about: one they may See by a team
/// or folder rule, or one holding a profile they may See. Nothing else — a
/// folder whose every profile is hidden would say there is work here they
/// cannot see, and so would a count that took the hidden ones in.
async fn folders_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Folder>>, ApiError> {
    let c = require_member(&st, &headers)?;
    let profiles = st.store.profiles()?;
    // The folder rows, and any name a profile carries without one: profiles
    // written before folders had rows of their own.
    let mut colors: BTreeMap<String, String> = st
        .store
        .folders_in(TEAM)?
        .into_iter()
        .map(|f| (f.name, f.color))
        .collect();
    for p in &profiles {
        colors.entry(p.folder.clone()).or_default();
    }
    let out = colors
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .filter_map(|(name, color)| {
            let seen = profiles
                .iter()
                .filter(|p| p.folder == name)
                .filter(|p| c.may(Action::See, &Target::profile(&p.id, &p.folder)))
                .count() as u64;
            // The same rule a permission keeps a folder's rules by.
            sees_folder(&c.grants, &name, &profiles).then_some(Folder { name, color, profiles: seen })
        })
        .collect();
    Ok(Json(out))
}

// ---- proxies ----

#[derive(Deserialize)]
struct ProxyWrite {
    name: String,
    kind: String,
    #[serde(default)]
    country: String,
    data: String,
    /// As for a profile: refused once the proxy has moved past this revision.
    base_revision: Option<u64>,
}

/// `store::TeamProxy` is not serialisable and that file is not ours to change,
/// so the wire shape lives here.
#[derive(Debug, Serialize)]
struct ProxyRow {
    id: String,
    name: String,
    kind: String,
    country: String,
    revision: u64,
    updated_at: u64,
    updated_by: String,
}

impl From<TeamProxy> for ProxyRow {
    fn from(p: TeamProxy) -> Self {
        ProxyRow {
            id: p.id,
            name: p.name,
            kind: p.kind,
            country: p.country,
            revision: p.revision,
            updated_at: p.updated_at,
            updated_by: p.updated_by,
        }
    }
}

#[derive(Debug, Serialize)]
struct ProxyWithData {
    #[serde(flatten)]
    proxy: ProxyRow,
    data: String,
}

fn proxy_args(body: &ProxyWrite) -> Result<(&str, &str, &str, Vec<u8>), ApiError> {
    let name = label(body.name.trim())?;
    if name.is_empty() {
        return Err(ApiError::bad("a proxy needs a name"));
    }
    let kind = label(body.kind.trim())?;
    if kind.is_empty() {
        return Err(ApiError::bad("a proxy needs a kind"));
    }
    Ok((name, kind, label(&body.country)?, data_arg(&body.data)?))
}

/// A team proxy the member may know about; anything else reads as missing.
fn visible_proxy(st: &AppState, c: &Caller, id: &str) -> Result<TeamProxy, ApiError> {
    match st.store.proxy_meta(id)? {
        Some((x, space)) if space == TEAM && c.may(Action::See, &proxy_target(id)) => Ok(x),
        _ => Err(ApiError::missing()),
    }
}

async fn proxies_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProxyRow>>, ApiError> {
    let c = require_member(&st, &headers)?;
    let out = st
        .store
        .proxies()?
        .into_iter()
        .filter(|p| c.may(Action::See, &proxy_target(&p.id)))
        .map(ProxyRow::from)
        .collect();
    Ok(Json(out))
}

async fn proxy_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProxyWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    let proxy = visible_proxy(&st, &c, &id)?;
    if !may_use_proxy(&c.grants, &id) {
        return Err(ApiError::forbidden());
    }
    let data = st.store.proxy_data(&id)?.ok_or_else(ApiError::missing)?;
    Ok(Json(ProxyWithData {
        proxy: proxy.into(),
        data: STANDARD.encode(data),
    }))
}

/// Who hears of a change to a team proxy: whoever may See it.
fn sees_proxy(grants: &[Grant], id: &str) -> bool {
    allows(grants, Action::See, &proxy_target(id))
}

async fn proxy_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProxyWrite>,
) -> Result<(StatusCode, Json<ProxyRow>), ApiError> {
    let c = require_member(&st, &headers)?;
    let (name, kind, country, data) = proxy_args(&body)?;
    let fields = ProxyFields { name, kind, country, data: &data };

    let r = st.store.put_proxy_with(TEAM, &id, &fields, c.writer(), body.base_revision, |s, existing, found| {
        let g = live(s)?;
        match existing {
            Some(_) => see_then(g, Action::Edit, &proxy_target(&id)),
            // A new proxy lands in the team itself: a folder's Create does not reach it.
            None if allows(g, Action::Create, &Target::team()) => {
                if found.named {
                    may_take_name(g, &proxy_target(&id))
                } else {
                    Ok(())
                }
            }
            None => Err(Refusal::Forbidden),
        }
    })?;
    let p = r.map_err(|e| refused_team_write(&c, e, c.may(Action::Create, &Target::team())))?;
    tell_regranted(&st).await;
    tell_team(&st, "proxy", &id, |g| sees_proxy(g, &id)).await;
    Ok((written(p.revision), Json(p.into())))
}

async fn proxy_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    st.store.delete_proxy_checked(TEAM, &id, c.writer(), at.base_revision, |s, _| {
        see_then(live(s)?, Action::Delete, &proxy_target(&id))
    })??;
    tell_regranted(&st).await;
    let ids = who_sees(&st, |g| sees_proxy(g, &id)).await;
    tell(&st, &ids, "proxy", &id).await;
    tell(&st, &ids, "trash", &id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the team's trash ----
//
// A deleted team item is judged where it was: its folder as deleted (carried
// along by a folder rename). Restoring and purging both ask Delete on it.

fn trash_target(t: &TrashItem) -> Target<'_> {
    match t.kind {
        TrashKind::Profile => Target::profile(&t.id, &t.folder),
        TrashKind::Proxy => proxy_target(&t.id),
        // Bookmarks, extensions and the rest are in no folder.
        _ => Target::team(),
    }
}

/// `?kind=` on a trash route. Needed for a bookmark, extension, automation or
/// tag, whose id is only one of its kind; absent means a profile or a proxy.
#[derive(Deserialize, Default)]
struct TrashQuery {
    kind: Option<String>,
}

fn trash_kind_arg(raw: Option<&str>) -> Result<Option<TrashKind>, ApiError> {
    match raw {
        None => Ok(None),
        Some("profile") => Ok(Some(TrashKind::Profile)),
        Some("proxy") => Ok(Some(TrashKind::Proxy)),
        Some(k) => TrashKind::of_item(k).map(Some).ok_or_else(ApiError::missing),
    }
}

fn sees_trashed(grants: &[Grant], t: &TrashItem) -> bool {
    allows(grants, Action::See, &trash_target(t))
}

/// A trash row as listed: when it will be purged on its own.
#[derive(Debug, Serialize)]
struct TrashRow {
    #[serde(flatten)]
    item: TrashItem,
    purge_at: u64,
}

async fn trash_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TrashRow>>, ApiError> {
    let c = require_member(&st, &headers)?;
    let out = st
        .store
        .trash()?
        .into_iter()
        .filter(|t| c.may(Action::See, &trash_target(t)))
        .map(|item| TrashRow { purge_at: item.deleted_at.saturating_add(st.trash_ttl_secs), item })
        .collect();
    Ok(Json(out))
}

/// What a restore answers: `{"profile": row}`, `{"proxy": row}` or
/// `{"item": row}`, as it is back in the team. Not tagged inline: a proxy row
/// has a `kind` of its own.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RestoredRow {
    Profile(TeamProfile),
    Proxy(ProxyRow),
    Item(Item),
}

/// The trash judges a row by Delete where it was; `?kind=` must match it.
fn may_bring_back(s: &Standing, t: &TrashItem, kind: Option<TrashKind>) -> Result<(), Refusal> {
    if kind.is_some_and(|k| k != t.kind) {
        return Err(Refusal::Missing);
    }
    see_then(live(s)?, Action::Delete, &trash_target(t))
}

async fn trash_restore(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<TrashQuery>,
) -> Result<Json<RestoredRow>, ApiError> {
    let c = require_member(&st, &headers)?;
    let kind = trash_kind_arg(q.kind.as_deref())?;
    let mut seen: Option<TrashItem> = None;
    let mut judge = |s: &Standing, t: &TrashItem| {
        seen = Some(t.clone());
        may_bring_back(s, t, kind)
    };
    let restored = match kind {
        Some(k) if k.is_item() => RestoredRow::Item(st.store.restore_item(k.as_str(), &id, c.writer(), &mut judge)??),
        _ => match st.store.restore_from_trash(&id, c.writer(), &mut judge)?? {
            Restored::Profile(p) => RestoredRow::Profile(p),
            Restored::Proxy(x) => RestoredRow::Proxy(x.into()),
        },
    };
    tell_regranted(&st).await;
    if let Some(t) = &seen {
        let ids = who_sees(&st, |g| sees_trashed(g, t)).await;
        tell(&st, &ids, t.kind.as_str(), &id).await;
        tell(&st, &ids, "trash", &id).await;
    }
    Ok(Json(restored))
}

async fn trash_purge(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<TrashQuery>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    let kind = trash_kind_arg(q.kind.as_deref())?;
    let judge = |s: &Standing, t: &TrashItem| may_bring_back(s, t, kind);
    let t = match kind {
        Some(k) if k.is_item() => st.store.purge_item(k.as_str(), &id, &c.member.id, judge)??,
        _ => st.store.purge_from_trash(&id, &c.member.id, judge)??,
    };
    // A session or package it had is freed after, a little at a time.
    tidy(&st.store);
    tell_regranted(&st).await;
    tell_team(&st, "trash", &id, |g| sees_trashed(g, &t)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Purge what has been in the trash longer than its days, and tell those who
/// could see it. Run at startup and then periodically; it also frees what an
/// upload cut short by a stop left behind.
pub async fn sweep_trash(st: &AppState) -> anyhow::Result<usize> {
    let cutoff = auth::unix_now().saturating_sub(st.trash_ttl_secs);
    let gone = st.store.expire_trash(cutoff)?;
    tidy(&st.store);
    tell_regranted(st).await;
    for t in &gone {
        tell_team(st, "trash", &t.id, |g| sees_trashed(g, t)).await;
    }
    Ok(gone.len())
}

// ---- moving between a member's own space and the team ----

#[derive(Deserialize, Default)]
struct ToTeam {
    /// Absent means no folder, as when writing one.
    #[serde(default)]
    folder: String,
}

/// The body is optional here, as on the other moves: none means no folder.
fn to_team_arg(raw: &[u8]) -> Result<ToTeam, ApiError> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok(ToTeam::default());
    }
    serde_json::from_slice(raw).map_err(|_| ApiError::bad("the request is not what this route takes"))
}

// Each move is judged in the store's transaction, on the row as it is there:
// one checked beforehand could have moved, or the right gone, in between.

async fn my_profile_to_team(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    raw: Bytes,
) -> Result<Json<TeamProfile>, ApiError> {
    let c = require_member(&st, &headers)?;
    let body = to_team_arg(&raw)?;
    let folder = folder_arg(&body.folder)?;
    // It is new to the team, in that folder: the same rights as making it there,
    // its binding included.
    let p = st.store.move_profile(&id, &c.mine(), TEAM, folder, c.writer(), |s, _, found| {
        let g = live(s)?;
        if !allows(g, Action::Create, &Target::folder(folder)) {
            return Err(Refusal::Forbidden);
        }
        if found.named {
            may_take_name(g, &Target::profile(&id, folder))?;
        }
        may_bind(g, found.bound)
    })??;
    tell_regranted(&st).await;
    tell_team(&st, "profile", &id, |g| sees_profile(g, &id, &[folder])).await;
    tell_owner(&st, &c.member.id, "profile", id).await;
    Ok(Json(p))
}

async fn profile_to_me(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<TeamProfile>, ApiError> {
    let c = require_member(&st, &headers)?;
    // It arrives in no folder: the team's folder names are the team's, and
    // the member files it where they like.
    let mut was = String::new();
    let p = st.store.move_profile(&id, TEAM, &c.mine(), "", c.writer(), |s, p, _| {
        was = p.folder.clone();
        see_then(live(s)?, Action::TakeOut, &Target::profile(&id, &p.folder))
    })??;
    tell_regranted(&st).await;
    tell_team(&st, "profile", &id, |g| sees_profile(g, &id, &[&was])).await;
    tell_owner(&st, &c.member.id, "profile", id).await;
    Ok(Json(p))
}

async fn my_proxy_to_team(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProxyRow>, ApiError> {
    let c = require_member(&st, &headers)?;
    // A proxy lands in the team itself, so it is the team's Create; Edit too
    // when the team already names it, as a team profile's binding does.
    let x = st.store.move_proxy(&id, &c.mine(), TEAM, c.writer(), |s, _, found| {
        let g = live(s)?;
        if !allows(g, Action::Create, &Target::team()) {
            Err(Refusal::Forbidden)
        } else if found.named {
            may_take_name(g, &proxy_target(&id))
        } else {
            Ok(())
        }
    })??;
    tell_regranted(&st).await;
    tell_team(&st, "proxy", &id, |g| sees_proxy(g, &id)).await;
    tell_owner(&st, &c.member.id, "proxy", id).await;
    Ok(Json(x.into()))
}

async fn proxy_to_me(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProxyRow>, ApiError> {
    let c = require_member(&st, &headers)?;
    let x = st.store.move_proxy(&id, TEAM, &c.mine(), c.writer(), |s, _, _| {
        see_then(live(s)?, Action::TakeOut, &proxy_target(&id))
    })??;
    tell_regranted(&st).await;
    tell_team(&st, "proxy", &id, |g| sees_proxy(g, &id)).await;
    tell_owner(&st, &c.member.id, "proxy", id).await;
    Ok(Json(x.into()))
}

// ---- a member's own space ----
//
// No grants here: a member may do anything in their own space, and nobody
// else reaches it. What is checked is only that the id really is theirs.

/// One of the caller's own profiles. Anybody else's, or a team one, is missing.
fn own_profile(st: &AppState, c: &Caller, id: &str) -> Result<TeamProfile, ApiError> {
    match st.store.profile_meta(id)? {
        Some((p, space)) if space == c.mine() => Ok(p),
        _ => Err(ApiError::missing()),
    }
}

fn own_proxy(st: &AppState, c: &Caller, id: &str) -> Result<TeamProxy, ApiError> {
    match st.store.proxy_meta(id)? {
        Some((x, space)) if space == c.mine() => Ok(x),
        _ => Err(ApiError::missing()),
    }
}

async fn my_profiles_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProfileRow>>, ApiError> {
    let c = require_member(&st, &headers)?;
    // Which of my own devices has each one open.
    Ok(Json(with_open_on(&st, st.store.listed_in(&c.mine(), None)?)?))
}

async fn my_profile_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProfileWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    let row = one_row(&st, &c.mine(), &id)?;
    let data = st.store.profile_data(&id)?.ok_or_else(ApiError::missing)?;
    Ok(Json(ProfileWithData { row, data: STANDARD.encode(data) }))
}

async fn my_profile_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProfileWrite>,
) -> Result<(StatusCode, Json<TeamProfile>), ApiError> {
    let c = require_member(&st, &headers)?;
    let (name, folder, data, card) = profile_args(&body)?;
    let fields = ProfileFields { name, folder, data: &data, card: card.as_ref(), created_at: body.created_at };
    // An id already in the team or in someone else's space comes back as
    // OtherSpace, which reads as missing. A bound proxy must be one of mine.
    let p = st.store.put_profile_with(
        &c.mine(),
        &id,
        &fields,
        c.writer(),
        body.base_revision,
        |s, _, _| still_on(s),
    )??;
    tell_owner(&st, &c.member.id, "profile", id).await;
    Ok((written(p.revision), Json(p)))
}

async fn my_profile_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    st.store
        .delete_profile_checked(&c.mine(), &id, c.writer(), at.base_revision, |s, _| still_on(s))??;
    tidy(&st.store);
    tell_owner(&st, &c.member.id, "profile", id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn my_proxies_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProxyRow>>, ApiError> {
    let c = require_member(&st, &headers)?;
    let out = st.store.proxies_in(&c.mine())?.into_iter().map(ProxyRow::from).collect();
    Ok(Json(out))
}

async fn my_proxy_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProxyWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    let proxy = own_proxy(&st, &c, &id)?;
    let data = st.store.proxy_data(&id)?.ok_or_else(ApiError::missing)?;
    Ok(Json(ProxyWithData {
        proxy: proxy.into(),
        data: STANDARD.encode(data),
    }))
}

async fn my_proxy_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProxyWrite>,
) -> Result<(StatusCode, Json<ProxyRow>), ApiError> {
    let c = require_member(&st, &headers)?;
    let (name, kind, country, data) = proxy_args(&body)?;
    let fields = ProxyFields { name, kind, country, data: &data };
    let x = st.store.put_proxy_with(&c.mine(), &id, &fields, c.writer(), body.base_revision, |s, _, _| {
        still_on(s)
    })??;
    tell_owner(&st, &c.member.id, "proxy", id).await;
    Ok((written(x.revision), Json(x.into())))
}

async fn my_proxy_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    st.store
        .delete_proxy_checked(&c.mine(), &id, c.writer(), at.base_revision, |s, _| still_on(s))??;
    tell_owner(&st, &c.member.id, "proxy", id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn my_folders_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Folder>>, ApiError> {
    let c = require_member(&st, &headers)?;
    Ok(Json(st.store.folders_in(&c.mine())?))
}

async fn my_folder_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewFolder>,
) -> Result<(StatusCode, Json<Folder>), ApiError> {
    let c = require_member(&st, &headers)?;
    let f = make_folder(&st, &c.mine(), &body)?;
    tell_owner(&st, &c.member.id, "folder", f.name.clone()).await;
    Ok((StatusCode::CREATED, Json(f)))
}

async fn my_folder_change(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<FolderChange>,
) -> Result<Json<Folder>, ApiError> {
    let c = require_member(&st, &headers)?;
    // No grants live in a personal space, so a rename here affects nobody's.
    let f = change_folder(&st, &c.mine(), &name, &body).await?;
    tell_owner(&st, &c.member.id, "folder", f.name.clone()).await;
    Ok(Json(f))
}

async fn my_folder_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    remove_folder(&st, &c.mine(), &name).await?;
    tell_owner(&st, &c.member.id, "folder", name).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- bookmarks, extensions, automations and tags ----
//
// Each kind is its own list, in the team and in each member's own space. They
// live in no folder, so the team's rights on them are team-wide: See to read,
// Create, Edit and Delete as for a profile. An extension's package is a blob of
// its own beside it.

/// Kept only in a member's own space: how their own devices sync it.
const MY_ONLY_KINDS: [&str; 1] = ["sync"];

/// A kind under `/items/` (or `/me/items/` when `mine`); any other reads as a route that is not there.
fn kind_arg(raw: &str, mine: bool) -> Result<&'static str, ApiError> {
    let own: &[&'static str] = if mine { &MY_ONLY_KINDS } else { &[] };
    ITEM_KINDS.iter().chain(own).copied().find(|k| *k == raw).ok_or_else(ApiError::missing)
}

/// An item's id: 1 to 128 characters, none of them control characters.
fn item_id_arg(raw: &str) -> Result<&str, ApiError> {
    let n = raw.chars().count();
    if (1..=MAX_ITEM_ID).contains(&n) && !raw.chars().any(char::is_control) {
        Ok(raw)
    } else {
        Err(ApiError::bad("an item id is 1 to 128 characters"))
    }
}

/// The whole team, where an item's rights are judged.
fn team_wide() -> Target<'static> {
    Target::team()
}

fn sees_items(g: &[Grant]) -> bool {
    allows(g, Action::See, &team_wide())
}

#[derive(Deserialize)]
struct ItemWrite {
    #[serde(default)]
    name: String,
    /// Base64, as a profile's data; sealed at rest.
    data: String,
    base_revision: Option<u64>,
}

#[derive(Debug, Serialize)]
struct PackageInfo {
    package_rev: u64,
    size: u64,
    updated_at: u64,
    updated_by: String,
}

impl From<&BlobMeta> for PackageInfo {
    fn from(m: &BlobMeta) -> Self {
        PackageInfo { package_rev: m.rev, size: m.size, updated_at: m.updated_at, updated_by: m.updated_by.clone() }
    }
}

/// An item as listed: its fields, its data, and an extension's package if any.
#[derive(Debug, Serialize)]
struct ItemWithData {
    #[serde(flatten)]
    item: Item,
    package: Option<PackageInfo>,
    data: String,
}

impl From<StoredItem> for ItemWithData {
    fn from(s: StoredItem) -> Self {
        ItemWithData { package: s.package.as_ref().map(PackageInfo::from), item: s.item, data: STANDARD.encode(s.data) }
    }
}

/// An item changed: the team hears it if they may See items, a personal one its owner.
async fn tell_item(st: &AppState, c: &Caller, space: &str, what: &'static str, id: &str) {
    if space == TEAM {
        tell_team(st, what, id, sees_items).await;
    } else {
        tell_owner(st, &c.member.id, what, id.to_owned()).await;
    }
}

/// `?profile=`: the profile whose start the items are asked for.
#[derive(Deserialize, Default)]
struct ForProfile {
    profile: Option<String>,
}

/// What a team profile's start brings to a member who may Start it, whatever
/// else they may See: its folder's bookmarks, and the extensions it names.
struct Brought {
    id: String,
    folder: String,
    extensions: Vec<String>,
}

impl Brought {
    fn of(st: &AppState, c: &Caller, id: &str) -> Result<Option<Brought>, ApiError> {
        let p = match st.store.profile_meta(id)? {
            Some((p, space)) if space == TEAM => p,
            _ => return Ok(None),
        };
        if see_then(&c.grants, Action::Start, &Target::profile(id, &p.folder)).is_err() {
            return Ok(None);
        }
        let data = st.store.profile_data(id)?.unwrap_or_default();
        let named = serde_json::from_slice::<serde_json::Value>(&data)
            .ok()
            .and_then(|v| v.get("_meta")?.get("extensions")?.as_array().cloned())
            .unwrap_or_default();
        let extensions = named.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect();
        Ok(Some(Brought { id: id.to_owned(), folder: p.folder, extensions }))
    }

    fn wants(&self, kind: &str, item: &StoredItem) -> bool {
        match kind {
            "extension" => self.extensions.iter().any(|e| *e == item.item.id),
            // A bookmark of no folder is for every profile.
            "bookmark" => {
                let folder = serde_json::from_slice::<serde_json::Value>(&item.data)
                    .ok()
                    .and_then(|v| v.get("folder")?.as_str().map(|f| f.trim().to_owned()))
                    .unwrap_or_default();
                folder.is_empty() || folder == self.folder
            }
            _ => false,
        }
    }

    /// Still Start on the profile, by the rules as they are now.
    fn still(&self, g: &[Grant]) -> bool {
        see_then(g, Action::Start, &Target::profile(&self.id, &self.folder)).is_ok()
    }
}

async fn list_items(
    st: &AppState,
    c: &Caller,
    space: &str,
    kind: &str,
    for_profile: Option<&str>,
) -> Result<Json<Vec<ItemWithData>>, ApiError> {
    let kind = kind_arg(kind, space != TEAM)?;
    // Items are seen all together or not at all, but what a start needs comes with Start.
    if space == TEAM && !c.may(Action::See, &team_wide()) {
        let Some(b) = for_profile.map(|id| Brought::of(st, c, id)).transpose()?.flatten() else {
            return Ok(Json(Vec::new()));
        };
        let rows = st.store.items_in(space, kind)?.into_iter().filter(|i| b.wants(kind, i));
        return Ok(Json(rows.map(ItemWithData::from).collect()));
    }
    Ok(Json(st.store.items_in(space, kind)?.into_iter().map(ItemWithData::from).collect()))
}

async fn get_item(st: &AppState, c: &Caller, space: &str, kind: &str, id: &str) -> Result<Json<ItemWithData>, ApiError> {
    let kind = kind_arg(kind, space != TEAM)?;
    if space == TEAM && !c.may(Action::See, &team_wide()) {
        return Err(ApiError::missing());
    }
    let found = st.store.item_get(space, kind, id)?.ok_or_else(ApiError::missing)?;
    Ok(Json(found.into()))
}

async fn put_item(
    st: &AppState,
    c: &Caller,
    space: &str,
    kind: &str,
    id: &str,
    body: &ItemWrite,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let kind = kind_arg(kind, space != TEAM)?;
    let id = item_id_arg(id)?;
    let name = label(body.name.trim())?;
    let data = data_arg(&body.data)?;
    let team = space == TEAM;
    let r = st.store.put_item_checked(space, kind, id, name, &data, c.writer(), body.base_revision, |s, existing| {
        let g = live(s)?;
        match existing {
            _ if !team => Ok(()),
            Some(_) => see_then(g, Action::Edit, &team_wide()),
            None if allows(g, Action::Create, &team_wide()) => Ok(()),
            None => Err(Refusal::Forbidden),
        }
    })?;
    let may_create = !team || c.may(Action::Create, &team_wide());
    let item = r.map_err(|e| {
        if team {
            refused_team_write(c, e, may_create)
        } else {
            refused_write(e, may_create)
        }
    })?;
    tell_item(st, c, space, kind, id).await;
    Ok((written(item.revision), Json(item)))
}

async fn delete_item(
    st: &AppState,
    c: &Caller,
    space: &str,
    kind: &str,
    id: &str,
    base: Option<u64>,
) -> Result<StatusCode, ApiError> {
    let kind = kind_arg(kind, space != TEAM)?;
    let team = space == TEAM;
    st.store.delete_item_checked(space, kind, id, c.writer(), base, |s, _| {
        let g = live(s)?;
        if team {
            see_then(g, Action::Delete, &team_wide())
        } else {
            Ok(())
        }
    })??;
    if !team {
        tidy(&st.store);
    }
    tell_item(st, c, space, kind, id).await;
    if team {
        tell_team(st, "trash", id, sees_items).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `?base_package_rev=`: given, the upload is refused once the package moved past it.
#[derive(Deserialize, Default)]
struct BasePackage {
    base_package_rev: Option<u64>,
}

/// Only an extension has a package; any other kind has no such route.
fn package_kind(kind: &str) -> Result<(), ApiError> {
    if kind_arg(kind, false)? == "extension" {
        Ok(())
    } else {
        Err(ApiError::missing())
    }
}

/// Writing a package is Edit. The first one is part of making the extension:
/// Create will do for its maker, while the item is still as they left it.
fn may_write_package(g: &[Grant], has_one: bool, item: &Item, writer: &str) -> Result<(), Refusal> {
    let making = !has_one && item.revision == 1 && item.updated_by == writer;
    if !allows(g, Action::See, &team_wide()) {
        Err(Refusal::Missing)
    } else if allows(g, Action::Edit, &team_wide()) || (making && allows(g, Action::Create, &team_wide())) {
        Ok(())
    } else {
        Err(Refusal::Forbidden)
    }
}

async fn read_package(
    st: &AppState,
    c: &Caller,
    space: &str,
    kind: &str,
    id: String,
    for_profile: Option<&str>,
) -> Result<Response, ApiError> {
    package_kind(kind)?;
    let team = space == TEAM;
    // Without the team's See, only an extension a profile they may Start names.
    let brought = match for_profile {
        Some(p) if team && !c.may(Action::See, &team_wide()) => Brought::of(st, c, p)?,
        _ => None,
    };
    let brought = brought.filter(|b| b.extensions.iter().any(|e| *e == id));
    if team && !c.may(Action::See, &team_wide()) && brought.is_none() {
        return Err(ApiError::missing());
    }
    let (meta, body) = st.store.package_open(space, &id, &c.member.id, |s| {
        let g = live(s)?;
        if team && !sees_items(g) && !brought.as_ref().is_some_and(|b| b.still(g)) {
            Err(Refusal::Missing)
        } else {
            Ok(())
        }
    })??;
    let held = Held { store: st.store.clone(), body: body.body };
    let headers = [
        (CONTENT_TYPE, HeaderValue::from_static("application/octet-stream")),
        (HeaderName::from_static(PACKAGE_REV), HeaderValue::from(meta.rev)),
    ];
    let stream = Streamed::new(st.store.clone(), Vec::new(), Some(body), Some(held));
    Ok((headers, Body::new(stream)).into_response())
}

#[allow(clippy::too_many_arguments)]
async fn write_package(
    st: &AppState,
    c: &Caller,
    space: &str,
    kind: &str,
    id: String,
    q: BasePackage,
    headers: &HeaderMap,
    body: Body,
) -> Result<Json<PackageInfo>, ApiError> {
    package_kind(kind)?;
    let team = space == TEAM;
    // Before the body: the extension is there for the caller, who may write its package.
    if team && !c.may(Action::See, &team_wide()) {
        return Err(ApiError::missing());
    }
    let found = st.store.item_get(space, "extension", &id)?.ok_or_else(ApiError::missing)?;
    if team {
        may_write_package(&c.grants, found.package.is_some(), &found.item, &c.member.id)?;
    }
    drop(found);

    declared_within(headers, st.package_max)?;
    let inc = Incoming::new(st, st.store.package_upload(space, &id)?);
    let inc = receive(st, inc, body, st.package_max, None).await?;
    let (store, member, device, sp, key) =
        (st.store.clone(), c.member.id.clone(), c.device.id.clone(), space.to_owned(), id.clone());
    let meta = blocking(move || {
        let writer = Writer { member_id: &member, device_id: device.as_deref() };
        store.package_commit(&sp, &key, writer, q.base_package_rev, &inc.up, |s, has_one, item| {
            let g = live(s)?;
            if team {
                may_write_package(g, has_one, item, &member)
            } else {
                Ok(())
            }
        })
    })
    .await??;
    tell_item(st, c, space, "package", &id).await;
    Ok(Json(PackageInfo::from(&meta)))
}

async fn items_list(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    Query(q): Query<ForProfile>,
) -> Result<Json<Vec<ItemWithData>>, ApiError> {
    let c = require_member(&st, &headers)?;
    list_items(&st, &c, TEAM, &kind, q.profile.as_deref()).await
}

async fn item_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<ItemWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    get_item(&st, &c, TEAM, &kind, &id).await
}

async fn item_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Json(body): Json<ItemWrite>,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let c = require_member(&st, &headers)?;
    put_item(&st, &c, TEAM, &kind, &id, &body).await
}

async fn item_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    delete_item(&st, &c, TEAM, &kind, &id, at.base_revision).await
}

async fn package_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(q): Query<ForProfile>,
) -> Result<Response, ApiError> {
    let c = require_member(&st, &headers)?;
    read_package(&st, &c, TEAM, &kind, id, q.profile.as_deref()).await
}

async fn package_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(q): Query<BasePackage>,
    body: Body,
) -> Result<Json<PackageInfo>, ApiError> {
    let c = require_member(&st, &headers)?;
    write_package(&st, &c, TEAM, &kind, id, q, &headers, body).await
}

async fn my_items_list(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
) -> Result<Json<Vec<ItemWithData>>, ApiError> {
    let c = require_member(&st, &headers)?;
    list_items(&st, &c, &c.mine(), &kind, None).await
}

async fn my_item_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<ItemWithData>, ApiError> {
    let c = require_member(&st, &headers)?;
    get_item(&st, &c, &c.mine(), &kind, &id).await
}

async fn my_item_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Json(body): Json<ItemWrite>,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let c = require_member(&st, &headers)?;
    put_item(&st, &c, &c.mine(), &kind, &id, &body).await
}

async fn my_item_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(at): Query<BaseRevision>,
) -> Result<StatusCode, ApiError> {
    let c = require_member(&st, &headers)?;
    delete_item(&st, &c, &c.mine(), &kind, &id, at.base_revision).await
}

async fn my_package_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let c = require_member(&st, &headers)?;
    read_package(&st, &c, &c.mine(), &kind, id, None).await
}

async fn my_package_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(q): Query<BasePackage>,
    body: Body,
) -> Result<Json<PackageInfo>, ApiError> {
    let c = require_member(&st, &headers)?;
    let mine = c.mine();
    write_package(&st, &c, &mine, &kind, id, q, &headers, body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::verify_permission;
    use crate::model::Scope;

    // Most tests call the handlers directly, which keeps them short. The ones
    // under "through the router" below go through routing and the gates the way
    // a real request does, which is the only way to see what runs before the
    // body is read.

    fn state() -> AppState {
        AppState {
            store: Arc::new(Store::open_memory().unwrap()),
            key: Arc::new(PermissionKey::generate()),
            admin_secret: auth::new_admin_secret().into(),
            team: "team-under-test".into(),
            hub: Hub::new(),
            trash_ttl_secs: 30 * 24 * 60 * 60,
            session_max: BLOB_MAX,
            package_max: PACKAGE_MAX,
            public_url: None,
        }
    }

    /// `GET /profiles` with no `?ids=`.
    fn whole_list() -> Query<ListQuery> {
        Query(ListQuery::default())
    }

    fn hdr(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        h
    }

    fn admin(st: &AppState) -> String {
        auth::mint_admin(&st.admin_secret, 3600).unwrap()
    }

    /// A member, their token, and a grant table in one step.
    async fn member(st: &AppState, name: &str, grants: Vec<Grant>) -> (String, String) {
        let a = admin(st);
        // `CreatedMember` has no `Debug`, so this takes the long way round
        // rather than `unwrap`.
        let Ok((_, Json(created))) = members_create(
            State(st.clone()),
            hdr(&a),
            Json(NewMember { name: name.into() }),
        )
        .await
        else {
            panic!("the admin could not make a member");
        };
        let id = created.member.id.clone();
        let _ = member_set_grants(
            State(st.clone()),
            hdr(&a),
            Path(id.clone()),
            Json(SetGrants { grants }),
        )
        .await
        .unwrap();
        (id, created.token)
    }

    fn allow(scope: Scope, action: Action) -> Grant {
        Grant { scope, action, allow: true }
    }
    fn folder(name: &str) -> Scope {
        Scope::Folder { name: name.into() }
    }

    fn write(name: &str, folder: &str) -> ProfileWrite {
        ProfileWrite {
            name: name.into(),
            folder: folder.into(),
            data: STANDARD.encode(b"data-by-the-team"),
            base_revision: None,
            card: None,
            created_at: None,
        }
    }

    /// A DELETE with no `?base_revision=`.
    fn no_base() -> Query<BaseRevision> {
        Query(BaseRevision::default())
    }

    /// The proof a vouch answers with, or the refusal's status and code.
    async fn vouched(st: &AppState, v: team_wire::Vouch) -> Result<team_wire::Vouched, (StatusCode, &'static str)> {
        vouch(State(st.clone()), Json(v)).await.map(|Json(v)| v).map_err(|e| (e.status, e.code))
    }

    #[tokio::test]
    async fn a_vouch_proves_a_token_this_server_issued_and_no_other() {
        let st = state();
        let key = st.key.public_raw();
        let c = team_wire::challenge();
        let (id, vera) = member(&st, "Vera", vec![]).await;
        assert!(vouched(&st, team_wire::Vouch::new(&vera, &c)).await.unwrap().holds(&vera, &c, &key));
        let a = admin(&st);
        assert!(vouched(&st, team_wire::Vouch::new(&a, &c)).await.unwrap().holds(&a, &c, &key));

        // A re-issued token still gets through, to hear "replaced" inside.
        let fresh = auth::new_member_token();
        st.store.reissue_token(&id, &auth::token_hash(&fresh)).unwrap().unwrap();
        for t in [&vera, &fresh] {
            assert!(vouched(&st, team_wire::Vouch::new(t, &c)).await.unwrap().holds(t, &c, &key));
        }

        let unknown = Err((StatusCode::NOT_FOUND, code::UNKNOWN_TOKEN));
        assert_eq!(vouched(&st, team_wire::Vouch::new(&auth::new_member_token(), &c)).await, unknown);
        // Another server's admin token: answered for, but the proof is not one its holder takes.
        let theirs = auth::mint_admin(&auth::new_admin_secret(), 3600).unwrap();
        assert!(!vouched(&st, team_wire::Vouch::new(&theirs, &c)).await.unwrap().holds(&theirs, &c, &key));
        st.store.delete_member(&id).unwrap();
        assert_eq!(vouched(&st, team_wire::Vouch::new(&fresh, &c)).await, unknown);

        let bad = Err((StatusCode::BAD_REQUEST, code::BAD_REQUEST));
        let short = team_wire::Vouch { challenge: "AAAA".into(), ..team_wire::Vouch::new(&a, &c) };
        assert_eq!(vouched(&st, short).await, bad);
        let both = team_wire::Vouch { member: Some("x".into()), ..team_wire::Vouch::new(&a, &c) };
        assert_eq!(vouched(&st, both).await, bad);
    }

    #[tokio::test]
    async fn health_says_it_is_alive_and_which_team_and_nothing_else() {
        let st = state();
        let Json(body) = health(State(st.clone())).await;
        assert_eq!(body, serde_json::json!({ "status": "ok", "team": &*st.team }));
    }

    #[tokio::test]
    async fn an_admin_route_does_not_take_a_member_token() {
        let st = state();
        let (_, token) = member(&st, "Vera", vec![allow(Scope::Team, Action::See)]).await;

        let e = members_list(State(st.clone()), hdr(&token)).await.unwrap_err();
        assert_eq!(e.status, StatusCode::UNAUTHORIZED);

        let e = members_create(
            State(st.clone()),
            hdr(&token),
            Json(NewMember { name: "Smuggled".into() }),
        )
        .await
        .err()
        .expect("a member cannot make members");
        assert_eq!(e.status, StatusCode::UNAUTHORIZED);

        let e = member_set_grants(
            State(st.clone()),
            hdr(&token),
            Path("whoever".into()),
            Json(SetGrants { grants: vec![allow(Scope::Team, Action::Delete)] }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::UNAUTHORIZED);

        // And nobody was made along the way.
        assert_eq!(st.store.members().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_admin_token_opens_member_routes_as_the_admin_principal() {
        let st = state();
        let Ok(Json(issued)) = permission_get(State(st.clone()), hdr(&admin(&st))).await else {
            panic!("the admin gets a permission");
        };
        let p = verify_permission(&st.key.public_b64(), &issued.permission).unwrap();
        assert_eq!((p.sub.as_str(), p.epoch, p.team.as_str()), (ADMIN_ID, ADMIN_EPOCH, "team-under-test"));
        // Every right, wherever the launcher asks.
        for a in Action::ALL {
            assert!(decide(&p.grants, a, &Target::profile("p1", "Ads")).is_allowed(), "{a:?}");
            assert!(decide(&p.grants, a, &Target::team()).is_allowed(), "{a:?}");
        }
        let Json(list) = profiles_list(State(st.clone()), hdr(&admin(&st)), whole_list()).await.unwrap();
        assert!(list.is_empty());
        // A token that is neither is still nobody.
        let e = profiles_list(State(st.clone()), hdr("stm_nobody"), whole_list()).await.unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::UNAUTHORIZED, code::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn no_token_at_all_is_refused_everywhere() {
        let st = state();
        let bare = HeaderMap::new();
        assert_eq!(
            members_list(State(st.clone()), bare.clone()).await.unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            profiles_list(State(st.clone()), bare.clone(), whole_list()).await.unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
        // A token with no scheme, and a scheme with no token, are both nothing.
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, "stm_whatever".parse().unwrap());
        assert_eq!(
            profiles_list(State(st.clone()), h, whole_list()).await.unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, "Bearer  ".parse().unwrap());
        assert_eq!(
            profiles_list(State(st.clone()), h, whole_list()).await.unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_members_list_never_carries_a_token_or_a_hash() {
        let st = state();
        let (_, token) = member(&st, "Vera", vec![]).await;
        let Json(rows) = members_list(State(st.clone()), hdr(&admin(&st))).await.unwrap();
        let json = serde_json::to_string(&rows).unwrap();
        assert!(!json.contains(&token));
        assert!(!json.contains("token"), "no token and no hash of one: {json}");
        assert!(json.contains("Vera"));
    }

    #[tokio::test]
    async fn a_profile_a_member_may_not_see_is_missing_rather_than_refused() {
        let st = state();
        st.store.put_profile("p-ads", "Ad account", "Ads", b"one", "setup").unwrap();
        st.store.put_profile("p-shop", "Etsy", "Shops", b"two", "setup").unwrap();
        let (_, token) = member(
            &st,
            "Vera",
            vec![allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Start)],
        )
        .await;

        // The one they may see and open, with its bytes.
        let Json(got) = profile_get(State(st.clone()), hdr(&token), Path("p-ads".into()))
            .await
            .unwrap();
        assert_eq!(got.row.profile.name, "Ad account");
        assert_eq!(STANDARD.decode(got.data).unwrap(), b"one");

        // The one they may not reads exactly like one that was never there.
        let hidden = profile_get(State(st.clone()), hdr(&token), Path("p-shop".into()))
            .await
            .unwrap_err();
        let absent = profile_get(State(st.clone()), hdr(&token), Path("p-nope".into()))
            .await
            .unwrap_err();
        assert_eq!(hidden.status, StatusCode::NOT_FOUND);
        assert_eq!(hidden.status, absent.status);
        assert_eq!(hidden.message, absent.message);

        // And the list says the same thing.
        let Json(list) = profiles_list(State(st.clone()), hdr(&token), whole_list()).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].profile.id, "p-ads");

        // So does the folder list: a folder shows up because something in it does.
        let Json(folders) = folders_list(State(st.clone()), hdr(&token)).await.unwrap();
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Ads"]);
    }

    #[tokio::test]
    async fn writing_asks_create_for_a_new_profile_and_edit_for_one_that_exists() {
        let st = state();
        // May make things in Ads, may not change them afterwards.
        let (_, maker) = member(
            &st,
            "Maker",
            vec![allow(folder("Ads"), Action::Create), allow(folder("Ads"), Action::See)],
        )
        .await;

        let (code, Json(p)) = profile_put(
            State(st.clone()),
            hdr(&maker),
            Path("p1".into()),
            Json(write("First", "Ads")),
        )
        .await
        .unwrap();
        assert_eq!(code, StatusCode::CREATED);
        assert_eq!(p.revision, 1);

        // The second write is to something that exists, so it is Edit now — and
        // Create does not cover it.
        let e = profile_put(
            State(st.clone()),
            hdr(&maker),
            Path("p1".into()),
            Json(write("First, renamed", "Ads")),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profiles().unwrap()[0].name, "First");

        // The other way round: Edit but no Create.
        let (_, editor) = member(
            &st,
            "Editor",
            vec![allow(folder("Ads"), Action::Edit), allow(folder("Ads"), Action::See)],
        )
        .await;
        let (code, Json(p)) = profile_put(
            State(st.clone()),
            hdr(&editor),
            Path("p1".into()),
            Json(write("Edited", "Ads")),
        )
        .await
        .unwrap();
        assert_eq!(code, StatusCode::OK);
        assert_eq!(p.revision, 2, "an update, not a new row");

        let e = profile_put(
            State(st.clone()),
            hdr(&editor),
            Path("p-new".into()),
            Json(write("Mine now", "Ads")),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profiles().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_folder_in_the_body_cannot_talk_its_way_past_a_folder_rule() {
        let st = state();
        st.store.put_profile("p1", "Ad account", "Ads", b"one", "setup").unwrap();
        // Everything visible, but editing only inside Shops.
        let (_, token) = member(
            &st,
            "Vera",
            vec![allow(Scope::Team, Action::See), allow(folder("Shops"), Action::Edit)],
        )
        .await;

        // The profile is in Ads, whatever the request says it is in.
        let e = profile_put(
            State(st.clone()),
            hdr(&token),
            Path("p1".into()),
            Json(write("Moved", "Shops")),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profiles().unwrap()[0].folder, "Ads");
    }

    #[tokio::test]
    async fn a_hidden_profile_cannot_be_overwritten_by_guessing_its_id() {
        let st = state();
        st.store.put_profile("p-shop", "Etsy", "Shops", b"precious", "setup").unwrap();
        // Free rein in Ads, and no business knowing about Shops at all.
        let (_, token) = member(
            &st,
            "Vera",
            vec![
                allow(Scope::Team, Action::Create),
                allow(folder("Ads"), Action::See),
                allow(folder("Ads"), Action::Edit),
            ],
        )
        .await;

        let e = profile_put(
            State(st.clone()),
            hdr(&token),
            Path("p-shop".into()),
            Json(write("Mine now", "Ads")),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        assert_eq!(
            st.store.profile_data("p-shop").unwrap().unwrap(),
            b"precious",
            "a write they were refused did not land anyway"
        );
        assert_eq!(st.store.profile_folder("p-shop").unwrap().unwrap(), "Shops");
    }

    #[tokio::test]
    async fn deleting_is_its_own_right() {
        let st = state();
        st.store.put_profile("p1", "Ad account", "Ads", b"one", "setup").unwrap();
        let (_, weak) = member(
            &st,
            "Weak",
            vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Edit)],
        )
        .await;
        let e = profile_delete(State(st.clone()), hdr(&weak), Path("p1".into()), no_base())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profiles().unwrap().len(), 1);

        let (_, strong) = member(
            &st,
            "Strong",
            vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Delete)],
        )
        .await;
        let code = profile_delete(State(st.clone()), hdr(&strong), Path("p1".into()), no_base())
            .await
            .unwrap();
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert!(st.store.profiles().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_switched_off_member_is_refused_everywhere() {
        let st = state();
        st.store.put_profile("p1", "Ad account", "Ads", b"one", "setup").unwrap();
        let (id, token) = member(
            &st,
            "Vera",
            Action::ALL
                .iter()
                .map(|a| allow(Scope::Team, *a))
                .collect(),
        )
        .await;
        // Everything worked a moment ago.
        assert!(permission_get(State(st.clone()), hdr(&token)).await.is_ok());

        let _ = member_set_active(
            State(st.clone()),
            hdr(&admin(&st)),
            Path(id),
            Json(SetActive { active: false }),
        )
        .await
        .unwrap();

        let refusals = [
            permission_get(State(st.clone()), hdr(&token)).await.err(),
            profiles_list(State(st.clone()), hdr(&token), whole_list()).await.err(),
            profile_get(State(st.clone()), hdr(&token), Path("p1".into())).await.err(),
            profile_put(
                State(st.clone()),
                hdr(&token),
                Path("p1".into()),
                Json(write("Still here", "Ads")),
            )
            .await
            .err(),
            profile_delete(State(st.clone()), hdr(&token), Path("p1".into()), no_base()).await.err(),
            folders_list(State(st.clone()), hdr(&token)).await.err(),
            proxies_list(State(st.clone()), hdr(&token)).await.err(),
            proxy_put(
                State(st.clone()),
                hdr(&token),
                Path("x1".into()),
                Json(ProxyWrite {
                    name: "DC".into(),
                    kind: "socks5".into(),
                    country: "JP".into(),
                    data: STANDARD.encode(b"s"),
                    base_revision: None,
                }),
            )
            .await
            .err(),
            proxy_delete(State(st.clone()), hdr(&token), Path("x1".into()), no_base()).await.err(),
        ];
        for r in refusals {
            let e = r.expect("a switched-off member gets nothing");
            assert_eq!(e.status, StatusCode::FORBIDDEN, "{}", e.message);
        }
        // Their rights still say yes; being switched off is what says no.
        assert_eq!(st.store.profiles().unwrap()[0].name, "Ad account");
    }

    #[tokio::test]
    async fn a_fresh_permission_carries_the_epoch_the_store_has() {
        let st = state();
        // A rule about a folder that is not there would be left out.
        st.store.create_folder(TEAM, "Ads", "").unwrap();
        let grants = vec![allow(Scope::Team, Action::See), allow(folder("Ads"), Action::Start)];
        let (id, token) = member(&st, "Vera", grants.clone()).await;

        let stored = |id: &str| {
            st.store.members().unwrap().into_iter().find(|m| m.id == id).unwrap().epoch
        };

        let Ok(Json(issued)) = permission_get(State(st.clone()), hdr(&token)).await else {
            panic!("a live member gets a permission");
        };
        let p = verify_permission(&st.key.public_b64(), &issued.permission).unwrap();
        assert_eq!(p.epoch, stored(&id));
        assert_eq!(issued.epoch, p.epoch);
        assert_eq!(p.sub, id);
        assert_eq!(p.team, "team-under-test");
        // The same table, as a set: `decide` reads all the rows that match, so
        // the order they come back in carries nothing.
        assert_eq!(p.grants.len(), grants.len());
        for g in &grants {
            assert!(p.grants.contains(g), "the launcher enforces what we stored: {g:?}");
        }

        // Rewrite the table: the next permission is the next edition.
        let Json(ack) = member_set_grants(
            State(st.clone()),
            hdr(&admin(&st)),
            Path(id.clone()),
            Json(SetGrants { grants: vec![allow(Scope::Team, Action::See)] }),
        )
        .await
        .unwrap();
        assert_eq!(ack.epoch, stored(&id));

        let Ok(Json(issued)) = permission_get(State(st.clone()), hdr(&token)).await else {
            panic!("a live member gets a permission");
        };
        let p = verify_permission(&st.key.public_b64(), &issued.permission).unwrap();
        assert_eq!(p.epoch, ack.epoch);
        assert_eq!(p.grants.len(), 1);
    }

    #[tokio::test]
    async fn rewriting_a_table_tells_that_member_and_only_them() {
        let st = state();
        let (id, _) = member(&st, "Vera", vec![]).await;
        let (other, _) = member(&st, "Sam", vec![]).await;
        let mut vera = st.hub.subscribe(&id).await;
        let mut sam = st.hub.subscribe(&other).await;

        let Json(ack) = member_set_grants(
            State(st.clone()),
            hdr(&admin(&st)),
            Path(id.clone()),
            Json(SetGrants { grants: vec![allow(Scope::Team, Action::See)] }),
        )
        .await
        .unwrap();

        assert_eq!(vera.recv().await.unwrap(), Event::GrantsChanged { epoch: ack.epoch });
        assert!(sam.try_recv().is_err(), "Sam's rights did not change");
    }

    #[tokio::test]
    async fn switching_a_member_off_tells_them_at_once() {
        let st = state();
        let (id, _) = member(&st, "Vera", vec![]).await;
        let mut vera = st.hub.subscribe(&id).await;

        let _ = member_set_active(
            State(st.clone()),
            hdr(&admin(&st)),
            Path(id.clone()),
            Json(SetActive { active: false }),
        )
        .await
        .unwrap();
        assert_eq!(vera.recv().await.unwrap(), Event::Revoked);
    }

    #[tokio::test]
    async fn an_admin_route_on_a_member_who_is_not_there_is_not_our_fault() {
        let st = state();
        let a = admin(&st);
        let e = member_set_active(
            State(st.clone()),
            hdr(&a),
            Path("nobody".into()),
            Json(SetActive { active: false }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        let e = member_set_grants(
            State(st.clone()),
            hdr(&a),
            Path("nobody".into()),
            Json(SetGrants { grants: vec![] }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_write_is_told_to_those_who_may_see_it_and_nobody_else() {
        let st = state();
        let (id, token) = member(&st, "Vera", everything()).await;
        let (lena_id, _) = member(&st, "Lena", vec![allow(Scope::Team, Action::See)]).await;
        let (ads_id, _) = member(&st, "Ads only", vec![allow(folder("Ads"), Action::See)]).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        let mut vera = st.hub.subscribe(&id).await;
        let mut lena = st.hub.subscribe(&lena_id).await;
        let mut ads = st.hub.subscribe(&ads_id).await;
        let mut sam = st.hub.subscribe(&sam_id).await;
        let profile = || Event::TeamChanged { what: "profile", id: "p1".into() };
        let proxy = || Event::TeamChanged { what: "proxy", id: "x1".into() };

        let (code, _) =
            answer(&st, ask("PUT", "/profiles/p1", &token), Some(profile_json("Shared", "Ads", b"v"))).await;
        assert_eq!(code, StatusCode::CREATED);
        // Moved out of Ads: the member who could see it there hears they lost it.
        let (code, _) = answer(
            &st,
            ask("PUT", "/profiles/p1", &token),
            Some(based_on(profile_json("Shared", "Shops", b"v"), 1)),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = answer(&st, ask("DELETE", "/profiles/p1", &token), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let (code, _) = answer(&st, ask("PUT", "/proxies/x1", &token), Some(proxy_json("DC"))).await;
        assert_eq!(code, StatusCode::CREATED);

        let trash = || Event::TeamChanged { what: "trash", id: "p1".into() };
        // The delete is news twice over: gone from the list, and in the trash.
        // A new permission whenever something comes into sight: p1 in Ads, Shops, x1.
        let all = vec![regranted(), profile(), regranted(), profile(), profile(), trash(), regranted(), proxy()];
        assert_eq!(epochless(heard(&mut vera)), all);
        assert_eq!(epochless(heard(&mut lena)), all);
        // Ads only: the profile while it was in Ads and as it left, then nothing.
        // It came into her sight and went out of it: a new permission each time.
        assert_eq!(epochless(heard(&mut ads)), vec![regranted(), profile(), regranted(), profile()]);
        assert!(heard(&mut sam).is_empty(), "no id and no timing of what Sam may not see");
    }

    #[tokio::test]
    async fn a_team_folder_is_announced_only_to_those_who_may_see_it() {
        let st = state();
        let a = admin(&st);
        let (vera_id, _) = member(&st, "Vera", vec![allow(folder("Client Z"), Action::See)]).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        let mut vera = st.hub.subscribe(&vera_id).await;
        let mut sam = st.hub.subscribe(&sam_id).await;

        let (code, _) =
            answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Client Z" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(
            &st,
            ask("PATCH", "/admin/folders/Client%20Z", &a),
            Some(json!({ "name": "Client Y" })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = answer(&st, ask("DELETE", "/admin/folders/Client%20Y", &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);

        let folder = |name: &str| Event::TeamChanged { what: "folder", id: name.into() };
        let news: Vec<Event> =
            heard(&mut vera).into_iter().filter(|e| matches!(e, Event::TeamChanged { .. })).collect();
        assert_eq!(news, vec![folder("Client Z"), folder("Client Y"), folder("Client Y")]);
        assert!(heard(&mut sam).is_empty(), "Sam never learns the names");
    }

    #[tokio::test]
    async fn a_refused_write_tells_nobody() {
        let st = state();
        let (id, token) = member(&st, "Vera", vec![]).await;
        let mut vera = st.hub.subscribe(&id).await;
        profile_put(
            State(st.clone()),
            hdr(&token),
            Path("p1".into()),
            Json(write("Nope", "Ads")),
        )
        .await
        .unwrap_err();
        assert!(vera.try_recv().is_err(), "nothing changed, so nothing was announced");
    }

    #[tokio::test]
    async fn a_proxy_lives_in_the_team_so_a_folder_right_does_not_reach_it() {
        let st = state();
        let body = || ProxyWrite {
            name: "DC Tokyo".into(),
            kind: "socks5".into(),
            country: "JP".into(),
            data: STANDARD.encode(b"data-host-and-password"),
            base_revision: None,
        };

        // Everything, but only inside one folder. A proxy is in no folder.
        let (_, foldered) = member(
            &st,
            "Foldered",
            Action::ALL.iter().map(|a| allow(folder("Ads"), *a)).collect(),
        )
        .await;
        let e = proxy_put(State(st.clone()), hdr(&foldered), Path("x1".into()), Json(body()))
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);

        let (_, team) = member(
            &st,
            "Team",
            vec![
                allow(Scope::Team, Action::Create),
                allow(Scope::Team, Action::See),
                allow(Scope::Team, Action::Edit),
            ],
        )
        .await;
        let (code, _) = proxy_put(State(st.clone()), hdr(&team), Path("x1".into()), Json(body()))
            .await
            .unwrap();
        assert_eq!(code, StatusCode::CREATED);
        let (code, Json(row)) =
            proxy_put(State(st.clone()), hdr(&team), Path("x1".into()), Json(body()))
                .await
                .unwrap();
        assert_eq!(code, StatusCode::OK);
        assert_eq!(row.revision, 2);

        // The data half comes back whole, and the member with only a folder
        // right cannot see the proxy at all.
        let Json(got) = proxy_get(State(st.clone()), hdr(&team), Path("x1".into())).await.unwrap();
        assert_eq!(STANDARD.decode(got.data).unwrap(), b"data-host-and-password");
        let Json(rows) = proxies_list(State(st.clone()), hdr(&foldered)).await.unwrap();
        assert!(rows.is_empty());
        assert_eq!(
            proxy_get(State(st.clone()), hdr(&foldered), Path("x1".into()))
                .await
                .unwrap_err()
                .status,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_data_is_carried_untouched_and_a_broken_one_is_refused() {
        let st = state();
        let (_, token) = member(
            &st,
            "Vera",
            vec![
                allow(Scope::Team, Action::Create),
                allow(Scope::Team, Action::See),
                allow(Scope::Team, Action::Edit),
            ],
        )
        .await;
        // Bytes that are not text and do not survive guessing.
        let raw: Vec<u8> = (0u8..=255).collect();
        let _ = profile_put(
            State(st.clone()),
            hdr(&token),
            Path("p1".into()),
            Json(ProfileWrite {
                name: "Binary".into(),
                folder: String::new(),
                data: STANDARD.encode(&raw),
                base_revision: None,
                card: None,
                created_at: None,
            }),
        )
        .await
        .unwrap();
        let Json(got) = profile_get(State(st.clone()), hdr(&token), Path("p1".into()))
            .await
            .unwrap();
        assert_eq!(STANDARD.decode(got.data).unwrap(), raw);
        // A profile in no folder stays in no folder.
        assert_eq!(got.row.profile.folder, "");
        assert!(folders_list(State(st.clone()), hdr(&token)).await.unwrap().0.is_empty());

        let e = profile_put(
            State(st.clone()),
            hdr(&token),
            Path("p2".into()),
            Json(ProfileWrite {
                name: "Broken".into(),
                folder: String::new(),
                data: "not base64 at all!!".into(),
                base_revision: None,
                card: None,
                created_at: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_new_team_shows_a_member_nothing() {
        let st = state();
        let (_, token) = member(
            &st,
            "Vera",
            Action::ALL.iter().map(|a| allow(Scope::Team, *a)).collect(),
        )
        .await;
        // Every right there is, and still an empty space: the team holds only
        // what has been put in it.
        assert!(profiles_list(State(st.clone()), hdr(&token), whole_list()).await.unwrap().0.is_empty());
        assert!(proxies_list(State(st.clone()), hdr(&token)).await.unwrap().0.is_empty());
        assert!(folders_list(State(st.clone()), hdr(&token)).await.unwrap().0.is_empty());
    }

    // ---- through the router ----

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    async fn call(st: &AppState, req: HttpRequest<Body>) -> StatusCode {
        router(st.clone()).oneshot(req).await.unwrap().status()
    }

    /// A body that is not JSON at all. If anything read it before the token was
    /// checked, the answer would be a parse failure (4xx other than 401); a 401
    /// proves the gate turned the caller away first.
    fn garbage(method: &str, uri: &str, token: Option<&str>) -> HttpRequest<Body> {
        let mut b = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        b.body(Body::from("{ this is not json")).unwrap()
    }

    #[tokio::test]
    async fn a_stranger_is_turned_away_before_the_body_is_read() {
        let st = state();
        assert_eq!(call(&st, garbage("POST", "/admin/members", None)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&st, garbage("PUT", "/profiles/p1", None)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&st, garbage("PUT", "/proxies/x1", None)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(&st, garbage("PUT", "/admin/members/m1/grants", Some("stm_not-a-real-token"))).await,
            StatusCode::UNAUTHORIZED
        );
        // The newer routes sit behind the same two doors.
        for (method, uri) in [
            ("PUT", "/me/profiles/p1"),
            ("PUT", "/me/proxies/x1"),
            ("POST", "/me/folders"),
            ("PATCH", "/me/folders/Ads"),
            ("POST", "/me/profiles/p1/to-team"),
            ("POST", "/admin/folders"),
            ("PATCH", "/admin/folders/Ads"),
        ] {
            assert_eq!(call(&st, garbage(method, uri, None)).await, StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_admin_door_does_not_open_for_a_member() {
        let st = state();
        let (_id, token) = member(&st, "Vera", vec![]).await;
        assert_eq!(
            call(&st, garbage("POST", "/admin/members", Some(&token))).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&st, garbage("POST", "/admin/folders", Some(&token))).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn past_the_door_the_body_is_read_as_usual() {
        let st = state();
        // The admin gets through the gate, so now the broken body is what fails —
        // which shows the gate is a door and not a wall.
        let status = call(&st, garbage("POST", "/admin/members", Some(&admin(&st)))).await;
        assert!(status.is_client_error() && status != StatusCode::UNAUTHORIZED, "got {status}");
    }

    #[tokio::test]
    async fn health_needs_no_token_and_an_unknown_path_is_still_unknown() {
        let st = state();
        let req = HttpRequest::builder().uri("/health").body(Body::empty()).unwrap();
        assert_eq!(call(&st, req).await, StatusCode::OK);
        // The gates sit on matched routes only: a wrong path is a 404, not a
        // 401 that would say "something lives here".
        let req = HttpRequest::builder().uri("/nothing-here").body(Body::empty()).unwrap();
        assert_eq!(call(&st, req).await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_removed_members_profiles_stay_with_the_team() {
        let st = state();
        let see_and_make = vec![
            allow(Scope::Team, Action::See),
            allow(Scope::Team, Action::Create),
        ];
        let (vera_id, vera) = member(&st, "Vera", see_and_make.clone()).await;
        let (_sam_id, sam) = member(
            &st,
            "Sam",
            vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start)],
        )
        .await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;

        // Vera makes a profile in the team.
        let made = profile_put(
            State(st.clone()),
            hdr(&vera),
            Path("p-vera".into()),
            Json(ProfileWrite {
                name: "Vera's shop".into(),
                folder: "Shops".into(),
                data: STANDARD.encode(b"cookies and all"),
                base_revision: None,
                card: None,
                created_at: None,
            }),
        )
        .await;
        assert!(made.is_ok(), "Vera may create in the team");

        // The admin removes her.
        let gone = member_delete(State(st.clone()), hdr(&admin(&st)), Path(vera_id.clone())).await;
        assert_eq!(gone.unwrap(), StatusCode::NO_CONTENT);

        // Her launcher is told at once, so it can close and wipe what it holds.
        // The team's own news (her profile being made) is ahead of it in the
        // queue, so read on until the revocation turns up.
        let mut revoked = false;
        while let Ok(ev) = vera_live.try_recv() {
            if ev == Event::Revoked {
                revoked = true;
                break;
            }
        }
        assert!(revoked, "the removed member's launcher was not told");

        // Her token no longer opens anything...
        let refused = profile_get(State(st.clone()), hdr(&vera), Path("p-vera".into())).await;
        assert!(refused.is_err());

        // ...and what she made is still the team's, byte for byte, for anyone allowed to see it.
        let Ok(Json(got)) = profile_get(State(st.clone()), hdr(&sam), Path("p-vera".into())).await else {
            panic!("Sam can no longer read a profile Vera made");
        };
        assert_eq!(STANDARD.decode(got.data).unwrap(), b"cookies and all");
        assert_eq!(got.row.profile.updated_by, vera_id, "the record of who made it stays");

        // Removing someone who is not there is not an error on our side.
        let again = member_delete(State(st.clone()), hdr(&admin(&st)), Path(vera_id)).await;
        assert_eq!(again.unwrap_err().status, StatusCode::NOT_FOUND);
    }

    // ---- spaces, leases and folders, through the router ----

    use axum::http::request::Builder;
    use serde_json::{json, Value};

    fn ask(method: &str, uri: &str, token: &str) -> Builder {
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, format!("Bearer {token}"))
    }

    /// The same request, from one named device of the member's.
    fn on(b: Builder, id: &str, name: &str) -> Builder {
        b.header(DEVICE_ID, id).header(DEVICE_NAME, name)
    }

    /// Status and JSON body of one request through the whole router.
    async fn answer(st: &AppState, b: Builder, body: Option<Value>) -> (StatusCode, Value) {
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        let res = router(st.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        (status, json)
    }

    fn profile_json(name: &str, folder: &str, data: &[u8]) -> Value {
        json!({ "name": name, "folder": folder, "data": STANDARD.encode(data) })
    }

    fn proxy_json(name: &str) -> Value {
        json!({ "name": name, "kind": "socks5", "country": "JP", "data": STANDARD.encode(b"host:port") })
    }

    fn based_on(mut body: Value, revision: u64) -> Value {
        body["base_revision"] = revision.into();
        body
    }

    fn everything() -> Vec<Grant> {
        Action::ALL.iter().map(|a| allow(Scope::Team, *a)).collect()
    }

    /// Everything that has reached a live channel so far.
    fn heard(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// What was heard, each new permission as [`regranted`] whatever its epoch.
    fn epochless(events: Vec<Event>) -> Vec<Event> {
        events.into_iter().map(|e| if matches!(e, Event::GrantsChanged { .. }) { regranted() } else { e }).collect()
    }

    fn regranted() -> Event {
        Event::GrantsChanged { epoch: 0 }
    }

    #[tokio::test]
    async fn someone_elses_own_profile_is_missing_whichever_way_it_is_asked_for() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        // Every right the team has to give, which is still nothing here.
        let (_, sam) = member(&st, "Sam", everything()).await;

        let (code, _) = answer(
            &st,
            ask("PUT", "/me/profiles/p-own", &vera),
            Some(profile_json("My shop", "Private", b"vera's cookies")),
        )
        .await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, got) = answer(&st, ask("GET", "/me/profiles/p-own", &vera), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(STANDARD.decode(got["data"].as_str().unwrap()).unwrap(), b"vera's cookies");

        for (method, uri) in [
            ("GET", "/me/profiles/p-own"),
            ("DELETE", "/me/profiles/p-own"),
            ("POST", "/me/profiles/p-own/lease"),
            ("GET", "/profiles/p-own"),
            ("DELETE", "/profiles/p-own"),
            ("POST", "/profiles/p-own/lease"),
            ("DELETE", "/profiles/p-own/lease"),
            ("POST", "/profiles/p-own/to-me"),
        ] {
            let (code, body) = answer(&st, on(ask(method, uri, &sam), "sam-pc", "Sam's PC"), None).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{method} {uri}");
            assert_eq!(
                body,
                json!({ "error": "no such thing", "code": "not_found" }),
                "nothing says where it is: {method} {uri}"
            );
        }
        for uri in ["/me/profiles/p-own", "/profiles/p-own"] {
            let (code, _) =
                answer(&st, ask("PUT", uri, &sam), Some(profile_json("Mine now", "", b"sam's"))).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "PUT {uri}");
        }
        let (code, _) =
            answer(&st, ask("POST", "/me/profiles/p-own/to-team", &sam), Some(json!({}))).await;
        assert_eq!(code, StatusCode::NOT_FOUND);

        // Still Vera's, byte for byte, and in none of Sam's lists.
        assert_eq!(st.store.profile_data("p-own").unwrap().unwrap(), b"vera's cookies");
        assert_eq!(st.store.profile_meta("p-own").unwrap().unwrap().1, personal_space(&vera_id));
        for uri in ["/me/profiles", "/profiles", "/folders", "/me/folders"] {
            let (code, list) = answer(&st, ask("GET", uri, &sam), None).await;
            assert_eq!(code, StatusCode::OK);
            assert_eq!(list, json!([]), "{uri}");
        }
    }

    #[tokio::test]
    async fn a_change_to_my_own_space_is_told_to_me_and_not_to_the_team() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (sam_id, _) = member(&st, "Sam", everything()).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;

        let calls = [
            ("PUT", "/me/profiles/p-own", Some(profile_json("My shop", "", b"x")), StatusCode::CREATED),
            ("PUT", "/me/proxies/x-own", Some(proxy_json("Home")), StatusCode::CREATED),
            ("POST", "/me/folders", Some(json!({ "name": "Private" })), StatusCode::CREATED),
            ("DELETE", "/me/profiles/p-own", None, StatusCode::NO_CONTENT),
            ("DELETE", "/me/proxies/x-own", None, StatusCode::NO_CONTENT),
        ];
        for (method, uri, body, want) in calls {
            let (code, _) = answer(&st, ask(method, uri, &vera), body).await;
            assert_eq!(code, want, "{method} {uri}");
        }

        let mine = |what: &'static str, id: &str| Event::PersonalChanged { what, id: id.into() };
        assert_eq!(
            heard(&mut vera_live),
            vec![
                mine("profile", "p-own"),
                mine("proxy", "x-own"),
                mine("folder", "Private"),
                mine("profile", "p-own"),
                mine("proxy", "x-own"),
            ]
        );
        assert!(heard(&mut sam_live).is_empty(), "the team hears nothing of it");
    }

    #[tokio::test]
    async fn a_second_device_is_refused_the_lease_and_told_where_it_is_open() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let start = vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start)];
        let (vera_id, vera) = member(&st, "Vera", start.clone()).await;
        let (_, sam) = member(&st, "Sam", start).await;
        let (_, looker) = member(&st, "Looker", vec![allow(Scope::Team, Action::See)]).await;
        let lease = "/profiles/p1/lease";
        // Named by a person, in their own language: not ASCII.
        let laptop = |b: Builder| on(b, "vera-laptop", "MacBook Веры");

        let (code, got) = answer(&st, laptop(ask("POST", lease, &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(got["device_name"], "MacBook Веры");
        assert_eq!(got["member_id"], vera_id.as_str());
        let ttl = got["expires_at"].as_u64().unwrap() - got["acquired_at"].as_u64().unwrap();
        assert_eq!(ttl, LEASE_TTL_SECS);

        // Her other device, and somebody else: refused, and told where it is open.
        for (token, dev) in [(&vera, "vera-desktop"), (&sam, "sam-pc")] {
            let (code, body) = answer(&st, on(ask("POST", lease, token), dev, "Other"), None).await;
            assert_eq!(code, StatusCode::CONFLICT);
            assert_eq!(body["error"], "open on another device");
            assert_eq!(body["lease"]["device_name"], "MacBook Веры");
        }
        // Opening is Start; seeing is not enough.
        let (code, _) = answer(&st, on(ask("POST", lease, &looker), "l", "L"), None).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        // A script has no device to hold it with.
        let (code, _) = answer(&st, ask("POST", lease, &sam), None).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        // The laptop renews; only it can give the lease back.
        let (code, _) = answer(&st, laptop(ask("POST", lease, &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        let (code, body) =
            answer(&st, on(ask("DELETE", lease, &vera), "vera-desktop", "Vera's PC"), None).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body["lease"]["device_name"], "MacBook Веры");
        let (code, _) = answer(&st, laptop(ask("DELETE", lease, &vera)), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);

        let (code, got) =
            answer(&st, on(ask("POST", lease, &vera), "vera-desktop", "Vera's PC"), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(got["device_name"], "Vera's PC");
    }

    #[tokio::test]
    async fn a_write_from_a_device_without_the_lease_is_refused_with_the_lease() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let laptop = |b: Builder| on(b, "vera-laptop", "Vera's MacBook");
        let desktop = |b: Builder| on(b, "vera-desktop", "Vera's PC");
        let v2 = || Some(based_on(profile_json("Shop", "Ads", b"v2"), 1));

        let (code, _) = answer(&st, laptop(ask("POST", "/profiles/p1/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::OK);

        // Her other device, and a script with no device at all.
        for b in [desktop(ask("PUT", "/profiles/p1", &vera)), ask("PUT", "/profiles/p1", &vera)] {
            let (code, body) = answer(&st, b, v2()).await;
            assert_eq!(code, StatusCode::CONFLICT);
            assert_eq!(body["error"], "open on another device");
            assert_eq!(body["lease"]["device_name"], "Vera's MacBook");
        }
        // Nor can it be deleted or carried off from under the browser.
        for (method, uri) in [("DELETE", "/profiles/p1"), ("POST", "/profiles/p1/to-me")] {
            let (code, body) = answer(&st, desktop(ask(method, uri, &vera)), None).await;
            assert_eq!(code, StatusCode::CONFLICT, "{method} {uri}");
            assert_eq!(body["lease"]["device_name"], "Vera's MacBook");
        }
        assert_eq!(st.store.profile_data("p1").unwrap().unwrap(), b"v1");
        assert_eq!(st.store.profile_meta("p1").unwrap().unwrap().1, TEAM);

        // The device that has it open writes freely.
        let (code, got) = answer(&st, laptop(ask("PUT", "/profiles/p1", &vera)), v2()).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(got["revision"], 2);

        // The same holds in her own space, between her own devices.
        let own = Some(profile_json("Mine", "", b"own"));
        let (code, _) = answer(&st, laptop(ask("PUT", "/me/profiles/p-own", &vera)), own.clone()).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) =
            answer(&st, laptop(ask("POST", "/me/profiles/p-own/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        let (code, body) = answer(&st, desktop(ask("PUT", "/me/profiles/p-own", &vera)), own).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body["lease"]["device_name"], "Vera's MacBook");
        let (code, _) = answer(&st, desktop(ask("DELETE", "/me/profiles/p-own", &vera)), None).await;
        assert_eq!(code, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_write_based_on_an_old_revision_is_refused_with_the_current_one() {
        let st = state();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let stale = json!({ "error": "changed since you read it", "code": "stale", "current_revision": 2 });

        for (uri, body) in [
            ("/profiles/p1", profile_json("Shop", "Ads", b"v")),
            ("/proxies/x1", proxy_json("DC")),
            ("/me/profiles/p-own", profile_json("Mine", "", b"v")),
            ("/me/proxies/x-own", proxy_json("Home")),
        ] {
            let (code, _) = answer(&st, ask("PUT", uri, &vera), Some(body.clone())).await;
            assert_eq!(code, StatusCode::CREATED, "{uri}");
            let (code, got) = answer(&st, ask("PUT", uri, &vera), Some(based_on(body.clone(), 1))).await;
            assert_eq!((code, got["revision"].as_u64()), (StatusCode::OK, Some(2)), "{uri}");

            // A device that read revision 1 and slept through the save above.
            let (code, got) = answer(&st, ask("PUT", uri, &vera), Some(based_on(body, 1))).await;
            assert_eq!(code, StatusCode::CONFLICT, "{uri}");
            assert_eq!(got, stale, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_delete_based_on_an_old_revision_is_refused_with_the_current_one() {
        let st = state();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let stale = json!({ "error": "changed since you read it", "code": "stale", "current_revision": 2 });
        let del = |uri: &str, query: &str| ask("DELETE", &format!("{uri}{query}"), &vera);

        for (uri, body) in [
            ("/profiles/p1", profile_json("Shop", "Ads", b"v")),
            ("/proxies/x1", proxy_json("DC")),
            ("/me/profiles/p-own", profile_json("Mine", "", b"v")),
            ("/me/proxies/x-own", proxy_json("Home")),
        ] {
            for b in [body.clone(), based_on(body, 1)] {
                let (code, _) = answer(&st, ask("PUT", uri, &vera), Some(b)).await;
                assert!(code.is_success(), "{uri}");
            }
            // Read at revision 1, then somebody saved: refused, and it stays.
            let (code, got) = answer(&st, del(uri, "?base_revision=1"), None).await;
            assert_eq!((code, got), (StatusCode::CONFLICT, stale.clone()), "{uri}");
            assert_eq!(answer(&st, ask("GET", uri, &vera), None).await.0, StatusCode::OK, "{uri}");
            // Not a number is the caller's mistake.
            let req = del(uri, "?base_revision=two").body(Body::empty()).unwrap();
            assert_eq!(call(&st, req).await, StatusCode::BAD_REQUEST, "{uri}");

            let (code, _) = answer(&st, del(uri, "?base_revision=2"), None).await;
            assert_eq!(code, StatusCode::NO_CONTENT, "{uri}");
            assert_eq!(answer(&st, ask("GET", uri, &vera), None).await.0, StatusCode::NOT_FOUND, "{uri}");
        }

        // Without one there is no revision check.
        for body in [profile_json("Shop", "", b"v1"), profile_json("Shop", "", b"v2")] {
            let (code, _) = answer(&st, ask("PUT", "/profiles/p2", &vera), Some(body)).await;
            assert!(code.is_success());
        }
        let (code, _) = answer(&st, ask("DELETE", "/profiles/p2", &vera), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn a_refused_write_or_delete_says_nothing_about_the_lease_or_the_revision() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Shops", b"v1", "setup").unwrap();
        st.store.put_profile("p1", "Shop", "Shops", b"v2", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let (code, _) =
            answer(&st, on(ask("POST", "/profiles/p1/lease", &vera), "vera-laptop", "Vera's MacBook"), None)
                .await;
        assert_eq!(code, StatusCode::OK);
        // Sam may do anything but know about Shops; Lena may look and nothing more.
        let no_shops = Grant { scope: folder("Shops"), action: Action::See, allow: false };
        let (_, sam) = member(&st, "Sam", [everything(), vec![no_shops]].concat()).await;
        let (_, lena) = member(&st, "Lena", vec![allow(Scope::Team, Action::See)]).await;

        for (token, want) in [(&sam, StatusCode::NOT_FOUND), (&lena, StatusCode::FORBIDDEN)] {
            for (method, uri, body) in [
                ("PUT", "/profiles/p1", Some(based_on(profile_json("Mine", "Shops", b"x"), 1))),
                ("DELETE", "/profiles/p1?base_revision=1", None),
            ] {
                let (code, got) = answer(&st, on(ask(method, uri, token), "d", "D"), body).await;
                assert_eq!(code, want, "{method} {uri}");
                assert!(got.get("lease").is_none() && got.get("current_revision").is_none(), "{got}");
            }
        }
        assert_eq!(st.store.profile_data("p1").unwrap().unwrap(), b"v2");
    }

    #[tokio::test]
    async fn deleting_a_proxy_asks_see_then_delete_and_tells_the_team() {
        let st = state();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"proxy", "setup").unwrap();
        let hide = Grant { scope: Scope::Profile { id: "x1".into() }, action: Action::See, allow: false };
        let (_, blind) = member(&st, "Blind", [everything(), vec![hide]].concat()).await;
        let (_, looker) = member(&st, "Looker", vec![allow(Scope::Team, Action::See)]).await;
        let (sam_id, sam) = member(
            &st,
            "Sam",
            vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Delete)],
        )
        .await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;

        let (code, _) = answer(&st, ask("DELETE", "/proxies/x1", &blind), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (code, _) = answer(&st, ask("DELETE", "/proxies/x1", &looker), None).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        // Nor through Sam's own space: it is the team's.
        let (code, _) = answer(&st, ask("DELETE", "/me/proxies/x1", &sam), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(st.store.proxy_meta("x1").unwrap().is_some());
        assert!(heard(&mut sam_live).is_empty());

        let (code, _) = answer(&st, ask("DELETE", "/proxies/x1", &sam), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert!(st.store.proxy_meta("x1").unwrap().is_none());
        assert_eq!(
            heard(&mut sam_live),
            vec![
                Event::TeamChanged { what: "proxy", id: "x1".into() },
                Event::TeamChanged { what: "trash", id: "x1".into() },
            ]
        );
        let (code, _) = answer(&st, ask("DELETE", "/proxies/x1", &sam), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn moving_my_profile_into_the_team_needs_create_where_it_lands() {
        let st = state();
        let (vera_id, vera) = member(
            &st,
            "Vera",
            vec![allow(folder("Ads"), Action::Create), allow(folder("Ads"), Action::See)],
        )
        .await;
        let (sam_id, _) = member(&st, "Sam", vec![allow(Scope::Team, Action::See)]).await;
        let to_team = "/me/profiles/p-own/to-team";
        let (code, _) =
            answer(&st, ask("PUT", "/me/profiles/p-own", &vera), Some(profile_json("Mine", "", b"mine"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(&st, ask("PUT", "/me/proxies/x-own", &vera), Some(proxy_json("Home"))).await;
        assert_eq!(code, StatusCode::CREATED);

        // Another folder, and no folder at all, are not Ads.
        for body in [json!({ "folder": "Shops" }), json!({})] {
            let (code, _) = answer(&st, ask("POST", to_team, &vera), Some(body)).await;
            assert_eq!(code, StatusCode::FORBIDDEN);
        }
        // A proxy lands in the team itself, which a folder's Create does not reach.
        let (code, _) = answer(&st, ask("POST", "/me/proxies/x-own/to-team", &vera), None).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profile_meta("p-own").unwrap().unwrap().1, personal_space(&vera_id));
        assert_eq!(st.store.proxy_meta("x-own").unwrap().unwrap().1, personal_space(&vera_id));

        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let (code, moved) = answer(&st, ask("POST", to_team, &vera), Some(json!({ "folder": "Ads" }))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(moved["folder"], "Ads");
        assert_eq!(st.store.profile_meta("p-own").unwrap().unwrap().1, TEAM);
        assert_eq!(st.store.profile_data("p-own").unwrap().unwrap(), b"mine");
        // Now it is the team's, whoever in the team may see it hears of it,
        // and holds a new permission: it came into their sight.
        assert_eq!(
            epochless(heard(&mut sam_live)),
            vec![regranted(), Event::TeamChanged { what: "profile", id: "p-own".into() }]
        );
        // Vera also hears that her own space lost it, and that Ads, made with
        // it, is now there for the rules she holds about it.
        let epoch = st.store.members().unwrap().into_iter().find(|m| m.id == vera_id).unwrap().epoch;
        assert_eq!(
            heard(&mut vera_live),
            vec![
                Event::GrantsChanged { epoch },
                Event::TeamChanged { what: "profile", id: "p-own".into() },
                Event::PersonalChanged { what: "profile", id: "p-own".into() },
            ]
        );

        // Taking it back out is TakeOut, which she was not given.
        let (code, _) = answer(&st, ask("POST", "/profiles/p-own/to-me", &vera), None).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn taking_out_of_the_team_needs_take_out_and_lands_in_my_space() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"team's", "setup").unwrap();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"proxy", "setup").unwrap();
        let see = vec![allow(Scope::Team, Action::See)];
        let (_, looker) = member(&st, "Looker", see.clone()).await;
        let (taker_id, taker) =
            member(&st, "Taker", [see, vec![allow(Scope::Team, Action::TakeOut)]].concat()).await;
        let (_, blind) = member(&st, "Blind", vec![allow(Scope::Team, Action::TakeOut)]).await;

        for uri in ["/profiles/p1/to-me", "/proxies/x1/to-me"] {
            // Not there to take for someone who may not see it; seeing is not taking.
            let (code, _) = answer(&st, ask("POST", uri, &blind), None).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{uri}");
            let (code, _) = answer(&st, ask("POST", uri, &looker), None).await;
            assert_eq!(code, StatusCode::FORBIDDEN, "{uri}");
            let (code, _) = answer(&st, ask("POST", uri, &taker), None).await;
            assert_eq!(code, StatusCode::OK, "{uri}");
        }
        assert_eq!(st.store.profile_meta("p1").unwrap().unwrap().1, personal_space(&taker_id));
        assert_eq!(st.store.proxy_meta("x1").unwrap().unwrap().1, personal_space(&taker_id));
        let (_, mine) = answer(&st, ask("GET", "/me/profiles", &taker), None).await;
        assert_eq!((mine[0]["id"].as_str(), mine[0]["folder"].as_str()), (Some("p1"), Some("")));
        let (_, team) = answer(&st, ask("GET", "/profiles", &looker), None).await;
        assert_eq!(team, json!([]));
    }

    #[tokio::test]
    async fn the_folder_list_hides_a_folder_whose_only_profiles_are_hidden() {
        let st = state();
        let setup = Writer { member_id: "setup", device_id: None };
        for (id, f) in [("p-ads", "Ads"), ("p-ads-hidden", "Ads"), ("p-shop", "Shops")] {
            st.store.put_profile_checked(TEAM, id, id, f, b"x", setup, None, |_, _| Ok(())).unwrap().unwrap();
        }
        st.store.create_folder(TEAM, "Empty", "grey").unwrap();
        st.store.create_folder(TEAM, "Granted", "red").unwrap();

        let (_, vera) = member(
            &st,
            "Vera",
            vec![
                allow(Scope::Profile { id: "p-ads".into() }, Action::See),
                allow(folder("Granted"), Action::See),
            ],
        )
        .await;
        let (_, all) = member(&st, "All", vec![allow(Scope::Team, Action::See)]).await;
        let (_, most) = member(
            &st,
            "Most",
            vec![
                allow(Scope::Team, Action::See),
                Grant { scope: folder("Shops"), action: Action::See, allow: false },
            ],
        )
        .await;
        let f = |name: &str, color: &str, profiles: u64| json!({ "name": name, "color": color, "profiles": profiles });

        // Ads for the one profile in it she may see, and counted as one, not
        // two. Granted by its own rule, though empty. Shops and Empty, never.
        let (_, list) = answer(&st, ask("GET", "/folders", &vera), None).await;
        assert_eq!(list, json!([f("Ads", "", 1), f("Granted", "red", 0)]));

        let (_, list) = answer(&st, ask("GET", "/folders", &all), None).await;
        assert_eq!(
            list,
            json!([f("Ads", "", 2), f("Empty", "grey", 0), f("Granted", "red", 0), f("Shops", "", 1)])
        );
        // Refused on the folder: it goes, and the profile in it with it.
        let (_, list) = answer(&st, ask("GET", "/folders", &most), None).await;
        assert_eq!(list, json!([f("Ads", "", 2), f("Empty", "grey", 0), f("Granted", "red", 0)]));
    }

    #[tokio::test]
    async fn a_folder_rename_tells_the_members_who_see_it_or_whose_rules_named_it() {
        let st = state();
        let a = admin(&st);
        let (code, made) =
            answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Ads", "color": "orange" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        assert_eq!(made, json!({ "name": "Ads", "color": "orange", "profiles": 0 }));

        let (vera_id, vera) = member(&st, "Vera", vec![allow(folder("Ads"), Action::See)]).await;
        let (sam_id, _) = member(&st, "Sam", vec![allow(Scope::Team, Action::See)]).await;
        let hidden = Grant { scope: folder("Ads"), action: Action::See, allow: false };
        let (kim_id, _) = member(&st, "Kim", vec![allow(Scope::Team, Action::See), hidden]).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let mut kim_live = st.hub.subscribe(&kim_id).await;
        let epoch = |id: &str| st.store.members().unwrap().into_iter().find(|m| m.id == id).unwrap().epoch;
        let kim_before = epoch(&kim_id);
        let regranted = |events: Vec<Event>| -> Vec<Event> {
            events.into_iter().filter(|e| matches!(e, Event::GrantsChanged { .. })).collect()
        };

        let (code, renamed) = answer(
            &st,
            ask("PATCH", "/admin/folders/Ads", &a),
            Some(json!({ "name": "Advertising", "color": "blue" })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(renamed, json!({ "name": "Advertising", "color": "blue", "profiles": 0 }));
        assert_eq!(regranted(heard(&mut vera_live)), vec![Event::GrantsChanged { epoch: epoch(&vera_id) }]);
        // Sam's rules never named it, but he sees one folder go and another come.
        assert_eq!(regranted(heard(&mut sam_live)), vec![Event::GrantsChanged { epoch: epoch(&sam_id) }]);
        // Kim's refusal moved with it: hidden before and after, so nothing.
        assert!(regranted(heard(&mut kim_live)).is_empty());
        assert_eq!(epoch(&kim_id), kim_before);
        // Her rule moved with the folder, so she still sees it under its new name.
        let (_, list) = answer(&st, ask("GET", "/folders", &vera), None).await;
        assert_eq!(list[0]["name"], "Advertising");

        // Deleting it takes her rule with it, and tells her the same way.
        let (code, _) = answer(&st, ask("DELETE", "/admin/folders/Advertising", &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert_eq!(regranted(heard(&mut vera_live)), vec![Event::GrantsChanged { epoch: epoch(&vera_id) }]);
        assert_eq!(regranted(heard(&mut sam_live)), vec![Event::GrantsChanged { epoch: epoch(&sam_id) }]);
        assert!(regranted(heard(&mut kim_live)).is_empty());
        assert!(st.store.grants_of(&vera_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn my_folders_and_the_teams_are_kept_apart() {
        let st = state();
        let a = admin(&st);
        let (_, vera) = member(&st, "Vera", vec![allow(Scope::Team, Action::See)]).await;

        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Ads" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Ads" }))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "  " }))).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        // The same name in her own space is another folder.
        let (code, _) =
            answer(&st, ask("POST", "/me/folders", &vera), Some(json!({ "name": "Ads", "color": "green" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) =
            answer(&st, ask("PUT", "/me/profiles/p-own", &vera), Some(profile_json("Mine", "Ads", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(&st, ask("DELETE", "/me/folders/Ads", &vera), None).await;
        assert_eq!(code, StatusCode::CONFLICT, "not while a profile is in it");

        let (code, f) = answer(
            &st,
            ask("PATCH", "/me/folders/Ads", &vera),
            Some(json!({ "name": "Mine", "color": "pink" })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(f, json!({ "name": "Mine", "color": "pink", "profiles": 1 }));
        let (code, _) =
            answer(&st, ask("PATCH", "/me/folders/Nope", &vera), Some(json!({ "color": "x" }))).await;
        assert_eq!(code, StatusCode::NOT_FOUND);

        // Her rename touched only her own; the team's Ads is as it was.
        let (_, mine) = answer(&st, ask("GET", "/me/folders", &vera), None).await;
        assert_eq!(mine, json!([{ "name": "Mine", "color": "pink", "profiles": 1 }]));
        let (_, team) = answer(&st, ask("GET", "/folders", &vera), None).await;
        assert_eq!(team, json!([{ "name": "Ads", "color": "", "profiles": 0 }]));
        let (_, p) = answer(&st, ask("GET", "/me/profiles/p-own", &vera), None).await;
        assert_eq!(p["folder"], "Mine");

        let (code, _) = answer(&st, ask("DELETE", "/me/profiles/p-own", &vera), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let (code, _) = answer(&st, ask("DELETE", "/me/folders/Mine", &vera), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
    }

    fn refuse(scope: Scope, action: Action) -> Grant {
        Grant { scope, action, allow: false }
    }

    #[tokio::test]
    async fn editing_a_profile_cannot_carry_it_into_a_folder_with_more_rights() {
        let st = state();
        st.store.put_profile("p1", "Ad acc", "Ads", b"v1", "setup").unwrap();
        let edit_ads = vec![allow(Scope::Team, Action::See), allow(folder("Ads"), Action::Edit)];
        let plus = |extra: Vec<Grant>| [edit_ads.clone(), extra].concat();
        // Sandbox is where Sam may take things out and delete them.
        let (_, sam) = member(
            &st,
            "Sam",
            plus(vec![allow(folder("Sandbox"), Action::TakeOut), allow(folder("Sandbox"), Action::Delete)]),
        )
        .await;
        // Rina may even make things there, which still is not carrying one in to take it.
        let (_, rina) = member(
            &st,
            "Rina",
            plus(vec![allow(folder("Sandbox"), Action::Create), allow(folder("Sandbox"), Action::TakeOut)]),
        )
        .await;
        let to = |f: &str| Some(based_on(profile_json("Ad acc", f, b"v2"), 1));

        for token in [&sam, &rina] {
            let (code, _) = answer(&st, ask("PUT", "/profiles/p1", token), to("Sandbox")).await;
            assert_eq!(code, StatusCode::FORBIDDEN);
            let (code, _) = answer(&st, ask("POST", "/profiles/p1/to-me", token), None).await;
            assert_eq!(code, StatusCode::FORBIDDEN);
        }
        // Nor into a folder nobody made.
        let (code, _) = answer(&st, ask("PUT", "/profiles/p1", &sam), to("Brand new")).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert_eq!(st.store.profile_meta("p1").unwrap().unwrap().0.folder, "Ads");
        assert!(st.store.folders_in(TEAM).unwrap().iter().all(|f| f.name != "Brand new"));

        // Where the mover may make things and gains nothing by it, a move is an edit.
        let (_, mover) = member(
            &st,
            "Mover",
            plus(vec![allow(folder("Shops"), Action::Edit), allow(folder("Shops"), Action::Create)]),
        )
        .await;
        let (code, got) = answer(&st, ask("PUT", "/profiles/p1", &mover), to("Shops")).await;
        assert_eq!((code, got["folder"].as_str()), (StatusCode::OK, Some("Shops")));
    }

    #[tokio::test]
    async fn seeing_an_item_is_not_reading_its_data() {
        let st = state();
        st.store.put_profile("p1", "Ad acc", "Ads", b"session", "setup").unwrap();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"user:password", "setup").unwrap();
        let see = allow(Scope::Team, Action::See);
        let (_, looker) = member(
            &st,
            "Looker",
            vec![
                see.clone(),
                refuse(Scope::Team, Action::Start),
                refuse(Scope::Team, Action::Cookies),
                refuse(Scope::Team, Action::TakeOut),
            ],
        )
        .await;
        for uri in ["/profiles/p1", "/proxies/x1"] {
            let (code, body) = answer(&st, ask("GET", uri, &looker), None).await;
            assert_eq!(code, StatusCode::FORBIDDEN, "{uri}");
            assert!(body.get("data").is_none(), "{uri}");
        }
        // Still listed: seeing is knowing it is there.
        let (_, list) = answer(&st, ask("GET", "/profiles", &looker), None).await;
        assert_eq!(list[0]["id"], "p1");

        // Any right that uses the data opens it; Cookies says nothing of a proxy.
        for (action, proxy_too) in [
            (Action::Start, true),
            (Action::Edit, true),
            (Action::Cookies, false),
            (Action::TakeOut, true),
        ] {
            let (_, user) = member(&st, "User", vec![see.clone(), allow(Scope::Team, action)]).await;
            let (code, got) = answer(&st, ask("GET", "/profiles/p1", &user), None).await;
            assert_eq!(code, StatusCode::OK, "{action:?}");
            assert_eq!(STANDARD.decode(got["data"].as_str().unwrap()).unwrap(), b"session");
            let (code, _) = answer(&st, ask("GET", "/proxies/x1", &user), None).await;
            assert_eq!(code == StatusCode::OK, proxy_too, "{action:?}");
        }
    }

    #[tokio::test]
    async fn a_hidden_id_answers_as_a_free_one_would() {
        let st = state();
        st.store.put_profile("p-hidden", "Secret", "Shops", b"x", "setup").unwrap();
        st.store.put_proxy("x-hidden", "Secret DC", "socks5", "", b"x", "setup").unwrap();
        let (_, sam) = member(&st, "Sam", vec![]).await;
        let req = |with_device: bool, m: &str, uri: &str| {
            let b = ask(m, uri, &sam);
            if with_device { on(b, "sam-pc", "Sam's PC") } else { b }
        };

        for (with_device, method, hidden, free, body) in [
            (false, "DELETE", "/profiles/p-hidden/lease", "/profiles/p-nope/lease", None),
            (true, "DELETE", "/profiles/p-hidden/lease", "/profiles/p-nope/lease", None),
            (false, "PUT", "/profiles/p-hidden", "/profiles/p-nope", Some(profile_json("Mine", "", b"y"))),
            (false, "PUT", "/proxies/x-hidden", "/proxies/x-nope", Some(proxy_json("Mine"))),
        ] {
            let a = answer(&st, req(with_device, method, hidden), body.clone()).await;
            let z = answer(&st, req(with_device, method, free), body).await;
            assert_eq!(a, z, "{method} {hidden}");
        }
        assert_eq!(st.store.profile_data("p-hidden").unwrap().unwrap(), b"x");
        assert_eq!(st.store.proxy_data("x-hidden").unwrap().unwrap(), b"x");
    }

    #[tokio::test]
    async fn switching_a_member_off_frees_what_they_had_open() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let start = vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start)];
        let (vera_id, vera) = member(&st, "Vera", start.clone()).await;
        let (_, sam) = member(&st, "Sam", start).await;
        let lease = "/profiles/p1/lease";

        let (code, _) = answer(&st, on(ask("POST", lease, &vera), "vera-mac", "Vera's Mac"), None).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = answer(
            &st,
            ask("PATCH", &format!("/admin/members/{vera_id}"), &admin(&st)),
            Some(json!({ "active": false })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        // Her launcher cannot give it back now; nobody has to wait for it to run out.
        let (code, got) = answer(&st, on(ask("POST", lease, &sam), "sam-pc", "Sam's PC"), None).await;
        assert_eq!((code, got["device_name"].as_str()), (StatusCode::OK, Some("Sam's PC")));
    }

    #[tokio::test]
    async fn a_rename_onto_a_name_some_rules_already_use_is_made() {
        let st = state();
        let a = admin(&st);
        // No folder called Clients, only rules naming it.
        let (sam_id, _) = member(
            &st,
            "Sam",
            vec![
                allow(folder("Ads"), Action::See),
                allow(folder("Clients"), Action::See),
                allow(folder("Ads"), Action::Delete),
                refuse(folder("Clients"), Action::Delete),
            ],
        )
        .await;
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Ads" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) =
            answer(&st, ask("PATCH", "/admin/folders/Ads", &a), Some(json!({ "name": "Clients" }))).await;
        assert_eq!(code, StatusCode::OK);
        let g = st.store.grants_of(&sam_id).unwrap();
        assert_eq!(g.len(), 2, "{g:?}");
        assert!(g.contains(&allow(folder("Clients"), Action::See)));
        assert!(g.contains(&refuse(folder("Clients"), Action::Delete)), "the refusal wins: {g:?}");
    }

    #[tokio::test]
    async fn a_table_with_one_rule_twice_is_taken_and_the_refusal_wins() {
        let st = state();
        let (id, _) = member(&st, "Vera", vec![]).await;
        let grants = vec![
            allow(folder("Ads"), Action::Delete),
            refuse(folder("Ads"), Action::Delete),
            allow(Scope::Team, Action::See),
            allow(Scope::Team, Action::See),
        ];
        let (code, _) = answer(
            &st,
            ask("PUT", &format!("/admin/members/{id}/grants"), &admin(&st)),
            Some(json!({ "grants": grants })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let g = st.store.grants_of(&id).unwrap();
        assert_eq!(g.len(), 2, "{g:?}");
        assert!(g.contains(&refuse(folder("Ads"), Action::Delete)));
        assert!(g.contains(&allow(Scope::Team, Action::See)));
    }

    #[tokio::test]
    async fn one_id_names_one_item_across_profiles_and_proxies() {
        let st = state();
        st.store.put_proxy("X", "DC", "socks5", "JP", b"proxy", "setup").unwrap();
        st.store.put_profile("P", "Shop", "", b"profile", "setup").unwrap();
        let (_, sam) = member(&st, "Sam", everything()).await;
        // So a rule naming "that profile" can never also reach a proxy.
        for (uri, body) in [
            ("/me/profiles/X", profile_json("Mine", "", b"x")),
            ("/profiles/X", profile_json("Mine", "", b"x")),
            ("/me/proxies/P", proxy_json("Mine")),
            ("/proxies/P", proxy_json("Mine")),
        ] {
            let (code, _) = answer(&st, ask("PUT", uri, &sam), Some(body)).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{uri}");
        }
        assert!(st.store.profile_meta("X").unwrap().is_none());
        assert!(st.store.proxy_meta("P").unwrap().is_none());
    }

    #[tokio::test]
    async fn names_and_labels_are_bounded() {
        let st = state();
        let a = admin(&st);
        let (_, vera) = member(&st, "Vera", everything()).await;
        let long = "x".repeat(MAX_LABEL + 1);
        for (method, uri, token, body) in [
            ("PUT", "/profiles/p1", &vera, json!({ "name": long, "data": "" })),
            ("PUT", "/profiles/p1", &vera, json!({ "name": "n", "folder": long, "data": "" })),
            ("PUT", "/me/profiles/p2", &vera, json!({ "name": long, "data": "" })),
            ("PUT", "/proxies/x1", &vera, json!({ "name": long, "kind": "socks5", "data": "" })),
            ("PUT", "/proxies/x1", &vera, json!({ "name": "n", "kind": long, "data": "" })),
            ("PUT", "/proxies/x1", &vera, json!({ "name": "n", "kind": "socks5", "country": long, "data": "" })),
            ("POST", "/me/folders", &vera, json!({ "name": long })),
            ("POST", "/admin/folders", &a, json!({ "name": long })),
            ("POST", "/admin/folders", &a, json!({ "name": "Ads", "color": long })),
        ] {
            let (code, _) = answer(&st, ask(method, uri, token), Some(body)).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{method} {uri}");
        }
        assert!(st.store.profiles().unwrap().is_empty());
        assert!(st.store.proxies().unwrap().is_empty());
        assert!(st.store.folders_in(TEAM).unwrap().is_empty());

        // Counted in characters, not bytes: a name in Cyrillic gets as many.
        let fits = "й".repeat(MAX_LABEL);
        let (code, _) =
            answer(&st, ask("PUT", "/profiles/p1", &vera), Some(json!({ "name": fits, "folder": fits, "data": "" })))
                .await;
        assert_eq!(code, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_folder_in_a_profile_write_is_trimmed_like_a_folder_name() {
        let st = state();
        let (_, vera) = member(
            &st,
            "Vera",
            vec![
                allow(folder("Ads"), Action::Create),
                allow(folder("Ads"), Action::See),
                allow(folder(""), Action::Create),
                allow(folder(""), Action::See),
            ],
        )
        .await;

        let (code, got) =
            answer(&st, ask("PUT", "/profiles/p-blank", &vera), Some(profile_json("Blank", "   ", b"x"))).await;
        assert_eq!((code, got["folder"].as_str()), (StatusCode::CREATED, Some("")));
        let (code, got) =
            answer(&st, ask("PUT", "/profiles/p-ads", &vera), Some(profile_json("Ad", "Ads ", b"x"))).await;
        assert_eq!((code, got["folder"].as_str()), (StatusCode::CREATED, Some("Ads")));

        let (code, _) =
            answer(&st, ask("PUT", "/me/profiles/p-own", &vera), Some(profile_json("Own", " Mine ", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, got) =
            answer(&st, ask("POST", "/me/profiles/p-own/to-team", &vera), Some(json!({ "folder": " Ads " })))
                .await;
        assert_eq!((code, got["folder"].as_str()), (StatusCode::OK, Some("Ads")));

        let names: Vec<String> = st.store.folders_in(TEAM).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["Ads".to_string()], "no blank folder and no second Ads");
    }

    #[tokio::test]
    async fn a_folder_is_never_named_as_a_path_would_drop_it() {
        let st = state();
        let a = admin(&st);
        let (_, vera) = member(&st, "Vera", vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Create)]).await;
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Ads" }))).await;
        assert_eq!(code, StatusCode::CREATED);
        for bad in [".", "..", " .. "] {
            let refused = |(code, got): (StatusCode, Value)| {
                assert_eq!((code, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("a folder cannot be named . or ..")), "{bad:?}");
            };
            refused(answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": bad }))).await);
            refused(answer(&st, ask("PATCH", "/admin/folders/Ads", &a), Some(json!({ "name": bad }))).await);
            refused(answer(&st, ask("POST", "/me/folders", &vera), Some(json!({ "name": bad }))).await);
            refused(answer(&st, ask("PUT", "/profiles/p1", &vera), Some(profile_json("P", bad, b"x"))).await);
        }
        // Dots inside a name are fine: only a whole "." or ".." is a path's.
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "..Ads." }))).await;
        assert_eq!(code, StatusCode::CREATED);
        let names: Vec<String> = st.store.folders_in(TEAM).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["..Ads.", "Ads"]);
    }

    /// Status, content type and body, as the launcher gets them.
    async fn raw(st: &AppState, req: HttpRequest<Body>) -> (StatusCode, String, String) {
        let res = router(st.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let ct = res
            .headers()
            .get(CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, ct, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn every_refusal_is_json_with_a_fixed_message() {
        let st = state();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let put = |uri: &str, body: String| {
            ask("PUT", uri, &vera)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        };
        let bare = |method: &str, uri: &str| ask(method, uri, &vera).body(Body::empty()).unwrap();
        let cases = [
            (put("/profiles/p1", "{ not json".into()), StatusCode::BAD_REQUEST, "bad_request"),
            (
                ask("PUT", "/profiles/p1", &vera).body(Body::from("{}")).unwrap(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "bad_request",
            ),
            (
                put("/profiles/p1", r#"{"name":"n","data":"","base_revision":"secret-value-123"}"#.into()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_request",
            ),
            (bare("DELETE", "/profiles/p1?base_revision=abc"), StatusCode::BAD_REQUEST, "bad_request"),
            (put("/profiles/p1", "x".repeat(MAX_BODY + 1)), StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            (bare("GET", "/profiles/%FF"), StatusCode::BAD_REQUEST, "bad_request"),
            (bare("POST", "/profiles"), StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            (bare("GET", "/nope"), StatusCode::NOT_FOUND, "not_found"),
            (bare("GET", "/profiles/p-nope"), StatusCode::NOT_FOUND, "not_found"),
        ];
        for (req, want, want_code) in cases {
            let what = format!("{} {}", req.method(), req.uri());
            let (code, ct, body) = raw(&st, req).await;
            assert_eq!(code, want, "{what}");
            assert_eq!(ct, "application/json", "{what}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert!(v["error"].is_string() && v.as_object().unwrap().len() == 2, "{what}: {body}");
            assert_eq!(v["code"], want_code, "{what}");
            assert!(!body.contains("secret-value-123"), "{what}: the request is not quoted back");
        }

        // A move into the team needs no body, as a move out of it does not.
        let (code, _) =
            answer(&st, ask("PUT", "/me/profiles/p-own", &vera), Some(profile_json("Own", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, got) = answer(&st, ask("POST", "/me/profiles/p-own/to-team", &vera), None).await;
        assert_eq!((code, got["folder"].as_str()), (StatusCode::OK, Some("")));
    }

    #[tokio::test]
    async fn a_live_channel_listens_before_the_member_is_checked_the_last_time() {
        let st = state();
        let (id, token) = member(&st, "Vera", vec![]).await;
        // What the upgrade carries over to the socket is already listening.
        let (mut rx, _seat) = listen(&st, &hdr(&token)).await.unwrap();
        let _ = member_set_active(
            State(st.clone()),
            hdr(&admin(&st)),
            Path(id),
            Json(SetActive { active: false }),
        )
        .await
        .unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::Revoked);
        // Off already: no channel at all.
        assert_eq!(listen(&st, &hdr(&token)).await.unwrap_err().status, StatusCode::FORBIDDEN);
    }

    // ---- who am I ----

    #[tokio::test]
    async fn me_says_which_team_and_who_is_asking() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (sam_id, sam) = member(&st, "Sam", vec![]).await;
        let (lena_id, lena) = member(&st, "Lena", vec![]).await;
        let key = st.key.public_b64();
        let team = json!({ "id": "team-under-test", "name": "Team" });

        let (code, me) = answer(&st, ask("GET", "/me", &vera), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            me,
            json!({ "team": team, "member": { "id": vera_id, "name": "Vera" }, "admin": false, "perm_key": key })
        );
        // The key is the one every permission checks out with.
        let (_, issued) = answer(&st, ask("GET", "/permission", &vera), None).await;
        let p = verify_permission(me["perm_key"].as_str().unwrap(), issued["permission"].as_str().unwrap());
        assert_eq!(p.unwrap().sub, vera_id);

        let (code, me) = answer(&st, ask("GET", "/me", &admin(&st)), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(me, json!({ "team": team, "member": null, "admin": true, "perm_key": key }));

        // Switched off and removed answer differently, so the launcher can tell.
        let a = admin(&st);
        let (code, _) =
            answer(&st, ask("PATCH", &format!("/admin/members/{sam_id}"), &a), Some(json!({ "active": false }))).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = answer(&st, ask("DELETE", &format!("/admin/members/{lena_id}"), &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let (code, got) = answer(&st, ask("GET", "/me", &sam), None).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::FORBIDDEN, Some("switched_off")));
        let (code, got) = answer(&st, ask("GET", "/me", &lena), None).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("unauthorized")));
        let req = HttpRequest::builder().uri("/me").body(Body::empty()).unwrap();
        assert_eq!(call(&st, req).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_admin_is_told_the_public_url_the_owner_gave() {
        let mut st = state();
        st.public_url = Some("https://team.example.com".into());
        let (_, vera) = member(&st, "Vera", vec![]).await;
        let (_, me) = answer(&st, ask("GET", "/me", &admin(&st)), None).await;
        assert_eq!(me["public_url"], "https://team.example.com");
        // A member reaches it already, and needs no address to invite anyone.
        let (_, me) = answer(&st, ask("GET", "/me", &vera), None).await;
        assert!(me.get("public_url").is_none(), "{me}");
    }

    #[tokio::test]
    async fn renaming_the_team_is_the_admins_and_everyone_connected_hears_it() {
        let st = state();
        let a = admin(&st);
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let (mut admin_live, _seat) = listen(&st, &hdr(&a)).await.unwrap();
        let (code, _) =
            answer(&st, ask("PATCH", &format!("/admin/members/{sam_id}"), &a), Some(json!({ "active": false }))).await;
        assert_eq!(code, StatusCode::OK);
        heard(&mut sam_live);

        let (code, _) = answer(&st, ask("PATCH", "/admin/team", &vera), Some(json!({ "name": "Mine" }))).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        for bad in ["   ".to_string(), "x".repeat(MAX_TEAM_NAME + 1)] {
            let (code, got) = answer(&st, ask("PATCH", "/admin/team", &a), Some(json!({ "name": bad }))).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        }
        assert!(heard(&mut vera_live).is_empty(), "nothing changed yet");

        let (code, got) = answer(&st, ask("PATCH", "/admin/team", &a), Some(json!({ "name": "  Ads crew  " }))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(got, json!({ "id": "team-under-test", "name": "Ads crew" }));
        let (_, me) = answer(&st, ask("GET", "/me", &vera), None).await;
        assert_eq!(me["team"]["name"], "Ads crew");

        let renamed = vec![Event::TeamChanged { what: "team", id: "team-under-test".into() }];
        assert_eq!(heard(&mut vera_live), renamed);
        assert_eq!(heard(&mut admin_live), renamed);
        assert!(heard(&mut sam_live).is_empty(), "switched off hears nothing more");

        // Counted in characters: a Cyrillic name of the full length fits.
        let fits = "й".repeat(MAX_TEAM_NAME);
        let (code, got) = answer(&st, ask("PATCH", "/admin/team", &a), Some(json!({ "name": fits }))).await;
        assert_eq!((code, got["name"].as_str()), (StatusCode::OK, Some(fits.as_str())));
    }

    #[tokio::test]
    async fn the_admin_works_in_the_team_and_in_a_space_of_its_own() {
        let st = state();
        let a = admin(&st);
        let start = vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start), allow(Scope::Team, Action::Create)];
        let (_, vera) = member(&st, "Vera", start).await;
        let admin_mac = |b: Builder| on(b, "admin-mac", "Owner's Mac");

        let (code, got) = answer(&st, ask("PUT", "/profiles/p1", &a), Some(profile_json("Shop", "Ads", b"v1"))).await;
        assert_eq!((code, got["updated_by"].as_str()), (StatusCode::CREATED, Some(ADMIN_ID)));
        let (code, got) = answer(&st, admin_mac(ask("POST", "/profiles/p1/lease", &a)), None).await;
        assert_eq!((code, got["member_id"].as_str()), (StatusCode::OK, Some(ADMIN_ID)));

        // One device per open profile holds for the admin as for anyone.
        let (code, got) = answer(&st, on(ask("POST", "/profiles/p1/lease", &vera), "v", "Vera's PC"), None).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("leased")));
        assert_eq!(got["lease"]["member_id"], ADMIN_ID);
        let (_, list) = answer(&st, ask("GET", "/profiles", &vera), None).await;
        assert_eq!(list[0]["open_on"]["member_name"], ADMIN_NAME);
        assert_eq!(list[0]["open_on"]["device_name"], "Owner's Mac");

        // The admin's own space is its own, as a member's is theirs.
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/own", &a), Some(profile_json("Mine", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        assert_eq!(st.store.profile_meta("own").unwrap().unwrap().1, personal_space(ADMIN_ID));
        let (_, mine) = answer(&st, ask("GET", "/me/profiles", &a), None).await;
        assert_eq!(mine[0]["id"], "own");
        let (_, theirs) = answer(&st, ask("GET", "/me/profiles", &vera), None).await;
        assert_eq!(theirs, json!([]));
        let (code, _) = answer(&st, ask("GET", "/me/profiles/own", &vera), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);

        // Its live channel hears the whole team.
        let (mut admin_live, _seat) = listen(&st, &hdr(&a)).await.unwrap();
        let (code, _) = answer(&st, ask("PUT", "/profiles/p2", &vera), Some(profile_json("Vera's", "Shops", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        assert_eq!(heard(&mut admin_live), vec![Event::TeamChanged { what: "profile", id: "p2".into() }]);

        // And it deletes from the device holding the lease, into the trash, in its own name.
        let (code, _) = answer(&st, admin_mac(ask("DELETE", "/profiles/p1", &a)), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert_eq!(st.store.trash().unwrap()[0].deleted_by, ADMIN_ID);
    }

    // ---- codes ----

    #[tokio::test]
    async fn every_refusal_carries_the_code_the_launcher_acts_on() {
        let st = state();
        let a = admin(&st);
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let (_, looker) = member(&st, "Looker", vec![allow(Scope::Team, Action::See)]).await;
        let (off_id, off) = member(&st, "Off", everything()).await;
        let (gone_id, gone) = member(&st, "Gone", everything()).await;
        answer(&st, ask("PATCH", &format!("/admin/members/{off_id}"), &a), Some(json!({ "active": false }))).await;
        answer(&st, ask("DELETE", &format!("/admin/members/{gone_id}"), &a), None).await;
        let (code, _) = answer(&st, on(ask("POST", "/profiles/p1/lease", &vera), "mac", "Mac"), None).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = answer(&st, ask("POST", "/admin/folders", &a), Some(json!({ "name": "Shops" }))).await;
        assert_eq!(code, StatusCode::CREATED);

        let cases = [
            (ask("GET", "/profiles", &off), None, StatusCode::FORBIDDEN, "switched_off"),
            (ask("PUT", "/profiles/p9", &off), Some(profile_json("x", "", b"x")), StatusCode::FORBIDDEN, "switched_off"),
            (ask("GET", "/profiles", &gone), None, StatusCode::UNAUTHORIZED, "unauthorized"),
            (ask("GET", "/profiles", "stm_nobody"), None, StatusCode::UNAUTHORIZED, "unauthorized"),
            (ask("GET", "/admin/members", &vera), None, StatusCode::UNAUTHORIZED, "unauthorized"),
            (ask("DELETE", "/profiles/p1", &looker), None, StatusCode::FORBIDDEN, "forbidden"),
            (ask("GET", "/profiles/p-nope", &vera), None, StatusCode::NOT_FOUND, "not_found"),
            (ask("PUT", "/profiles/p1", &vera), Some(based_on(profile_json("x", "Ads", b"x"), 1)), StatusCode::CONFLICT, "leased"),
            (ask("POST", "/profiles/p1/lease", &vera), None, StatusCode::BAD_REQUEST, "bad_request"),
            (ask("POST", "/admin/folders", &a), Some(json!({ "name": "Shops" })), StatusCode::CONFLICT, "conflict"),
            (ask("DELETE", "/admin/folders/Ads", &a), None, StatusCode::CONFLICT, "conflict"),
        ];
        for (b, body, want, want_code) in cases {
            let (code, got) = answer(&st, b, body).await;
            assert_eq!(code, want, "{got}");
            assert_eq!(got["code"], want_code, "{got}");
            assert!(got["error"].is_string(), "{got}");
        }
        // Stale, from a device that has it open.
        let (code, got) = answer(
            &st,
            on(ask("PUT", "/profiles/p1", &vera), "mac", "Mac"),
            Some(based_on(profile_json("x", "Ads", b"x"), 7)),
        )
        .await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("stale")));
        // Off already: no live channel, and the reason is the same code.
        let e = listen(&st, &hdr(&off)).await.unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::FORBIDDEN, code::SWITCHED_OFF));
    }

    #[test]
    fn a_member_switched_off_or_removed_under_a_write_is_told_which() {
        let removed = Standing::default();
        let off = Standing { known: true, active: false, grants: everything() };
        let on = Standing { known: true, active: true, grants: everything() };
        let e: ApiError = live(&removed).unwrap_err().into();
        assert_eq!((e.status, e.code), (StatusCode::UNAUTHORIZED, code::UNAUTHORIZED));
        let e: ApiError = live(&off).unwrap_err().into();
        assert_eq!((e.status, e.code), (StatusCode::FORBIDDEN, code::SWITCHED_OFF));
        assert_eq!(live(&on).unwrap().len(), Action::ALL.len());
        // The same through a write's own check: still the code that says why.
        let e = refused_write(still_on(&off).unwrap_err(), false);
        assert_eq!(e.code, code::SWITCHED_OFF);
    }

    // ---- the team's trash ----

    #[tokio::test]
    async fn a_team_delete_goes_to_the_trash_listed_by_the_folder_it_was_in() {
        let st = state();
        st.store.put_profile("p-ads", "Ad acc", "Ads", b"ads", "setup").unwrap();
        st.store.put_profile("p-shop", "Etsy", "Shops", b"shop", "setup").unwrap();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"proxy", "setup").unwrap();
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        let (_, ads) = member(&st, "Ads", vec![allow(folder("Ads"), Action::See)]).await;
        let (_, nobody) = member(&st, "Nobody", vec![]).await;

        for uri in ["/profiles/p-ads", "/profiles/p-shop", "/proxies/x1"] {
            let (code, _) = answer(&st, ask("DELETE", uri, &vera), None).await;
            assert_eq!(code, StatusCode::NO_CONTENT, "{uri}");
            let (code, _) = answer(&st, ask("GET", uri, &vera), None).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{uri}");
        }

        let (code, list) = answer(&st, ask("GET", "/trash", &vera), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 3);
        let row = |id: &str| list.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
        let p = row("p-ads");
        assert_eq!((p["kind"].as_str(), p["folder"].as_str(), p["name"].as_str()), (Some("profile"), Some("Ads"), Some("Ad acc")));
        assert_eq!(p["deleted_by"], vera_id.as_str());
        assert_eq!(
            p["purge_at"].as_u64().unwrap() - p["deleted_at"].as_u64().unwrap(),
            st.trash_ttl_secs,
            "says when it goes on its own"
        );
        assert!(p.get("data").is_none() && p.get("sealed").is_none(), "no data in a list");
        let x = row("x1");
        assert_eq!((x["kind"].as_str(), x["proxy_kind"].as_str(), x["country"].as_str()), (Some("proxy"), Some("socks5"), Some("JP")));

        // Each sees what they could see where it was.
        let (_, list) = answer(&st, ask("GET", "/trash", &ads), None).await;
        assert_eq!(list.as_array().unwrap().iter().map(|r| r["id"].clone()).collect::<Vec<_>>(), vec![json!("p-ads")]);
        let (_, list) = answer(&st, ask("GET", "/trash", &nobody), None).await;
        assert_eq!(list, json!([]));
        let (_, list) = answer(&st, ask("GET", "/trash", &admin(&st)), None).await;
        assert_eq!(list.as_array().unwrap().len(), 3);

        // The id stays taken while it waits, and says so to whoever may see it there.
        let (code, got) = answer(&st, ask("PUT", "/profiles/p-ads", &vera), Some(profile_json("New", "Ads", b"x"))).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("in_trash")));

        // A member's own space has no trash on the server: gone is gone.
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(profile_json("Mine", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(&st, ask("DELETE", "/me/profiles/own", &vera), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert_eq!(st.store.trash().unwrap().len(), 3);
        let (code, _) = answer(&st, ask("POST", "/trash/own/restore", &vera), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn restoring_and_purging_ask_delete_on_the_item_as_it_was() {
        let st = state();
        let a = admin(&st);
        st.store.put_profile("p1", "Ad acc", "Ads", b"session", "setup").unwrap();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"login", "setup").unwrap();
        let see_ads = allow(folder("Ads"), Action::See);
        let (_, looker) = member(&st, "Looker", vec![allow(Scope::Team, Action::See)]).await;
        let (_, blind) = member(&st, "Blind", vec![allow(Scope::Team, Action::Delete)]).await;
        let (deleter_id, deleter) =
            member(&st, "Deleter", vec![see_ads.clone(), allow(folder("Ads"), Action::Delete)]).await;
        let (_, kept_out) = member(
            &st,
            "Kept out",
            vec![see_ads, allow(folder("Ads"), Action::Delete), refuse(Scope::Profile { id: "p1".into() }, Action::Delete)],
        )
        .await;
        let mut deleter_live = st.hub.subscribe(&deleter_id).await;

        for uri in ["/profiles/p1", "/proxies/x1"] {
            let (code, _) = answer(&st, ask("DELETE", uri, &a), None).await;
            assert_eq!(code, StatusCode::NO_CONTENT);
        }
        heard(&mut deleter_live);

        for (method, uri) in [("POST", "/trash/p1/restore"), ("DELETE", "/trash/p1")] {
            let (code, got) = answer(&st, ask(method, uri, &looker), None).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")), "{method} {uri}");
            let (code, _) = answer(&st, ask(method, uri, &kept_out), None).await;
            assert_eq!(code, StatusCode::FORBIDDEN, "narrowest wins: {method} {uri}");
            // Not there for someone who could not see it where it was.
            let (code, _) = answer(&st, ask(method, uri, &blind), None).await;
            assert_eq!(code, StatusCode::NOT_FOUND, "{method} {uri}");
        }
        // A proxy is in no folder: a folder's Delete does not reach it.
        let (code, _) = answer(&st, ask("POST", "/trash/x1/restore", &deleter), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);

        let (code, got) = answer(&st, ask("POST", "/trash/p1/restore", &deleter), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!((got["profile"]["folder"].as_str(), got["profile"]["revision"].as_u64()), (Some("Ads"), Some(2)));
        assert_eq!(got["profile"]["updated_by"], deleter_id.as_str());
        assert_eq!(st.store.profile_data("p1").unwrap().unwrap(), b"session");
        assert_eq!(
            heard(&mut deleter_live),
            vec![
                Event::TeamChanged { what: "profile", id: "p1".into() },
                Event::TeamChanged { what: "trash", id: "p1".into() },
            ]
        );
        let (code, _) = answer(&st, ask("POST", "/trash/p1/restore", &deleter), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "nothing to restore twice");

        // Deleted again, then purged for good by the same right.
        let (code, _) = answer(&st, ask("DELETE", "/profiles/p1", &deleter), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        heard(&mut deleter_live);
        let (code, _) = answer(&st, ask("DELETE", "/trash/p1", &deleter), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        // Gone from sight too: a new permission.
        assert_eq!(
            epochless(heard(&mut deleter_live)),
            vec![regranted(), Event::TeamChanged { what: "trash", id: "p1".into() }]
        );
        assert!(st.store.trash().unwrap().iter().all(|t| t.id != "p1"));
        assert!(st.store.profile_meta("p1").unwrap().is_none());

        // The admin always can.
        let (code, got) = answer(&st, ask("POST", "/trash/x1/restore", &a), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!((got["proxy"]["kind"].as_str(), got["proxy"]["revision"].as_u64()), (Some("socks5"), Some(2)));
        assert_eq!(st.store.proxy_data("x1").unwrap().unwrap(), b"login");
        let (code, _) = answer(&st, ask("DELETE", "/proxies/x1", &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let (code, _) = answer(&st, ask("DELETE", "/trash/x1", &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert!(st.store.trash().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_trashed_profile_keeps_its_rights_through_a_folder_rename() {
        let st = state();
        let a = admin(&st);
        st.store.put_profile("p1", "Ad acc", "Ads", b"x", "setup").unwrap();
        st.store.create_folder(TEAM, "Ads", "").unwrap();
        let (_, deleter) =
            member(&st, "Deleter", vec![allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Delete)]).await;
        let (code, _) = answer(&st, ask("DELETE", "/profiles/p1", &a), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let (code, _) =
            answer(&st, ask("PATCH", "/admin/folders/Ads", &a), Some(json!({ "name": "Clients" }))).await;
        assert_eq!(code, StatusCode::OK);

        let (_, list) = answer(&st, ask("GET", "/trash", &deleter), None).await;
        assert_eq!(list[0]["folder"], "Clients");
        let (code, got) = answer(&st, ask("POST", "/trash/p1/restore", &deleter), None).await;
        assert_eq!((code, got["profile"]["folder"].as_str()), (StatusCode::OK, Some("Clients")));
    }

    #[tokio::test]
    async fn the_trash_lets_go_after_its_days_and_tells_who_could_see() {
        let mut st = state();
        st.store.put_profile("old", "Old", "Ads", b"x", "setup").unwrap();
        st.store.put_profile("new", "New", "Ads", b"x", "setup").unwrap();
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        for uri in ["/profiles/old", "/profiles/new"] {
            let (code, _) = answer(&st, ask("DELETE", uri, &vera), None).await;
            assert_eq!(code, StatusCode::NO_CONTENT);
        }
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;

        // Deleted 31 days ago, with 30 to keep it.
        let now = auth::unix_now();
        st.store.backdate_trash("old", now - 31 * 24 * 60 * 60).unwrap();
        assert_eq!(sweep_trash(&st).await.unwrap(), 1);
        assert_eq!(
            epochless(heard(&mut vera_live)),
            vec![regranted(), Event::TeamChanged { what: "trash", id: "old".into() }]
        );
        assert!(heard(&mut sam_live).is_empty());
        let (_, list) = answer(&st, ask("GET", "/trash", &vera), None).await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], "new");
        assert_eq!(sweep_trash(&st).await.unwrap(), 0, "the rest is not due");

        // No days to keep: it all goes at the next sweep.
        st.trash_ttl_secs = 0;
        assert_eq!(sweep_trash(&st).await.unwrap(), 1);
        assert!(st.store.trash().unwrap().is_empty());
    }

    // ---- what lists need ----

    #[tokio::test]
    async fn profiles_by_id_are_only_those_the_caller_may_see() {
        let st = state();
        for (id, name, f) in [("p1", "B", "Ads"), ("p2", "A", "Shops"), ("p3", "C", "Ads"), ("p-gone", "D", "Ads")] {
            st.store.put_profile(id, name, f, b"x", "setup").unwrap();
        }
        let (_, ads) = member(&st, "Ads", vec![allow(folder("Ads"), Action::See)]).await;
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(profile_json("Own", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, _) = answer(&st, ask("DELETE", "/profiles/p-gone", &vera), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        let ids_of = |v: &Value| v.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let q = "/profiles?ids=p1,p2,own,nope,p-gone,p1";

        let (code, got) = answer(&st, ask("GET", q, &ads), None).await;
        assert_eq!((code, ids_of(&got)), (StatusCode::OK, vec!["p1".to_string()]));
        let (_, got) = answer(&st, ask("GET", q, &vera), None).await;
        assert_eq!(ids_of(&got), vec!["p2".to_string(), "p1".to_string()], "by name; not her own, not the trash");
        assert_eq!(st.store.profile_meta("own").unwrap().unwrap().1, personal_space(&vera_id));
        let (_, got) = answer(&st, ask("GET", "/profiles?ids=", &vera), None).await;
        assert_eq!(got, json!([]));

        let many = |n: usize| (0..n).map(|i| format!("id{i}")).collect::<Vec<_>>().join(",");
        let (code, got) = answer(&st, ask("GET", &format!("/profiles?ids={}", many(MAX_IDS)), &vera), None).await;
        assert_eq!((code, got), (StatusCode::OK, json!([])));
        let (code, got) = answer(&st, ask("GET", &format!("/profiles?ids={}", many(MAX_IDS + 1)), &vera), None).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
    }

    #[tokio::test]
    async fn opening_and_closing_a_profile_is_told_to_those_who_may_see_it() {
        let st = state();
        let a = admin(&st);
        st.store.put_profile("p1", "Shop", "Ads", b"x", "setup").unwrap();
        let start = vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start)];
        let (vera_id, vera) = member(&st, "Vera", start).await;
        let (ads_id, ads) = member(&st, "Ads", vec![allow(folder("Ads"), Action::See)]).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut ads_live = st.hub.subscribe(&ads_id).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let lease = || Event::TeamChanged { what: "lease", id: "p1".into() };
        let mac = |b: Builder| on(b, "vera-mac", "MacBook Веры");

        let (code, _) = answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(heard(&mut vera_live), vec![lease()]);
        assert_eq!(heard(&mut ads_live), vec![lease()]);
        // A renewal is not news.
        let (code, _) = answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        assert!(heard(&mut ads_live).is_empty());

        let (_, list) = answer(&st, ask("GET", "/profiles?ids=p1", &ads), None).await;
        let open = &list[0]["open_on"];
        assert_eq!(
            (open["device_name"].as_str(), open["member_id"].as_str(), open["member_name"].as_str()),
            (Some("MacBook Веры"), Some(vera_id.as_str()), Some("Vera"))
        );
        assert_eq!(open["device_id"], "vera-mac");

        let (code, _) = answer(&st, mac(ask("DELETE", "/profiles/p1/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert_eq!(heard(&mut ads_live), vec![lease()]);
        let (_, list) = answer(&st, ask("GET", "/profiles", &ads), None).await;
        assert!(list[0]["open_on"].is_null());

        // Freed by a switch-off: the lists hear that too.
        let (code, _) = answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await;
        assert_eq!(code, StatusCode::OK);
        heard(&mut ads_live);
        let (code, _) =
            answer(&st, ask("PATCH", &format!("/admin/members/{vera_id}"), &a), Some(json!({ "active": false }))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(heard(&mut ads_live), vec![lease()]);
        assert!(heard(&mut sam_live).is_empty(), "Sam may not see it");

        // In a personal space only the owner hears, and sees which device has it.
        let (lena_id, lena) = member(&st, "Lena", vec![]).await;
        let mut lena_live = st.hub.subscribe(&lena_id).await;
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/own", &lena), Some(profile_json("Own", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        heard(&mut lena_live);
        let (code, _) = answer(&st, on(ask("POST", "/me/profiles/own/lease", &lena), "lena-pc", "Lena's PC"), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(heard(&mut lena_live), vec![Event::PersonalChanged { what: "lease", id: "own".into() }]);
        assert!(heard(&mut ads_live).is_empty() && heard(&mut sam_live).is_empty());
        let (_, mine) = answer(&st, ask("GET", "/me/profiles", &lena), None).await;
        assert_eq!(mine[0]["open_on"]["device_name"], "Lena's PC");
    }

    // ---- sessions, cards, items and packages ----

    /// Status, headers and raw body of one request with a binary body.
    async fn bytes_answer(st: &AppState, b: Builder, body: Vec<u8>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let res = router(st.clone()).oneshot(b.body(Body::from(body)).unwrap()).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        (parts.status, parts.headers, bytes.to_vec())
    }

    fn json_of(raw: &[u8]) -> Value {
        serde_json::from_slice(raw).unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(raw)))
    }

    /// The code of a refusal, as the launcher reads it.
    fn code_of(raw: &[u8]) -> String {
        json_of(raw)["code"].as_str().unwrap_or_default().to_owned()
    }

    /// A downloaded session: the JSON in front, then the bytes.
    fn split_session(raw: &[u8]) -> (Value, Vec<u8>) {
        let n = u32::from_be_bytes(raw[..4].try_into().unwrap()) as usize;
        (json_of(&raw[4..4 + n]), raw[4 + n..].to_vec())
    }

    async fn set_grants(st: &AppState, id: &str, grants: Vec<Grant>) {
        let _ = member_set_grants(State(st.clone()), hdr(&admin(st)), Path(id.into()), Json(SetGrants { grants }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_new_routes_sit_behind_the_same_door() {
        let st = state();
        for (method, uri) in [
            ("GET", "/profiles/p1/session"),
            ("PUT", "/profiles/p1/session?base_session_rev=0"),
            ("PUT", "/me/profiles/p1/session?base_session_rev=0"),
            ("GET", "/items/bookmark"),
            ("PUT", "/items/bookmark/b1"),
            ("PUT", "/items/extension/e1/package"),
            ("PUT", "/me/items/tag/t1"),
            ("PUT", "/me/items/extension/e1/package"),
        ] {
            assert_eq!(call(&st, garbage(method, uri, None)).await, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn a_session_goes_only_to_the_device_holding_the_lease_with_its_bound_proxy() {
        let st = state();
        let a = admin(&st);
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"user:pass@dc", "setup").unwrap();
        let mut body = profile_json("Shop", "Ads", b"settings");
        body["card"] = json!({ "proxy_id": "x1" });
        assert_eq!(answer(&st, ask("PUT", "/profiles/p1", &a), Some(body)).await.0, StatusCode::CREATED);
        // Start in Ads, and nothing at all on the proxy.
        let ads = |x| allow(folder("Ads"), x);
        let (vera_id, vera) = member(&st, "Vera", vec![ads(Action::See), ads(Action::Start)]).await;
        let (_, looker) = member(&st, "Looker", vec![ads(Action::See)]).await;
        let (_, nobody) = member(&st, "Nobody", vec![]).await;
        let uri = "/profiles/p1/session";
        let mac = |b: Builder| on(b, "vera-mac", "Vera's Mac");

        let (code, _, raw) = bytes_answer(&st, on(ask("GET", uri, &nobody), "n", "N"), vec![]).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::NOT_FOUND, "not_found".into()));
        let (code, _, raw) = bytes_answer(&st, on(ask("GET", uri, &looker), "l", "L"), vec![]).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::FORBIDDEN, "forbidden".into()), "seeing is not starting");
        let (code, _, _) = bytes_answer(&st, ask("GET", uri, &vera), vec![]).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "no device");
        let (code, _, raw) = bytes_answer(&st, mac(ask("GET", uri, &vera)), vec![]).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::CONFLICT, "no_lease".into()));

        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        let (code, _, raw) = bytes_answer(&st, on(ask("GET", uri, &vera), "vera-pc", "PC"), vec![]).await;
        let e = json_of(&raw);
        assert_eq!((code, e["code"].as_str()), (StatusCode::CONFLICT, Some("leased")));
        assert_eq!(e["lease"]["device_name"], "Vera's Mac");

        // The holder: nothing uploaded yet, and the proxy she could not read otherwise.
        let (code, headers, raw) = bytes_answer(&st, mac(ask("GET", uri, &vera)), vec![]).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], SESSION_TYPE);
        let (front, rest) = split_session(&raw);
        assert_eq!((front["session_rev"].as_u64(), front["size"].as_u64(), rest.len()), (Some(0), Some(0), 0));
        assert_eq!((front["proxy_id"].as_str(), front["proxy"]["kind"].as_str()), (Some("x1"), Some("socks5")));
        assert_eq!(STANDARD.decode(front["proxy"]["data"].as_str().unwrap()).unwrap(), b"user:pass@dc");
        assert_eq!(answer(&st, ask("GET", "/proxies/x1", &vera), None).await.0, StatusCode::NOT_FOUND);

        // After an upload the bytes follow, exactly as sent.
        let jar: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        let put = mac(ask("PUT", &format!("{uri}?base_session_rev=0"), &vera));
        let (code, _, raw) = bytes_answer(&st, put, jar.clone()).await;
        assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&raw));
        let (_, _, raw) = bytes_answer(&st, mac(ask("GET", uri, &vera)), vec![]).await;
        let (front, rest) = split_session(&raw);
        assert_eq!((front["session_rev"].as_u64(), front["size"].as_u64()), (Some(1), Some(70_000)));
        assert_eq!(front["updated_by"], vera_id.as_str());
        assert!(rest == jar, "the bytes as sent");

        // Start withdrawn while open: no more downloads, but her upload still lands.
        set_grants(&st, &vera_id, vec![ads(Action::See)]).await;
        let (code, _, _) = bytes_answer(&st, mac(ask("GET", uri, &vera)), vec![]).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let put = mac(ask("PUT", &format!("{uri}?base_session_rev=1"), &vera));
        let (code, _, raw) = bytes_answer(&st, put, b"closing".to_vec()).await;
        assert_eq!((code, json_of(&raw)["session_rev"].as_u64()), (StatusCode::OK, Some(2)));

        // In my own space my own proxy comes along; a team proxy named there does not.
        let (code, _) = answer(&st, ask("PUT", "/me/proxies/x-own", &looker), Some(proxy_json("Home"))).await;
        assert_eq!(code, StatusCode::CREATED);
        for (id, proxy) in [("own", "x-own"), ("own2", "x1")] {
            let mut own = profile_json("Mine", "", b"s");
            own["card"] = json!({ "proxy_id": proxy });
            let (code, _) = answer(&st, ask("PUT", &format!("/me/profiles/{id}"), &looker), Some(own)).await;
            assert_eq!(code, if id == "own" { StatusCode::CREATED } else { StatusCode::BAD_REQUEST }, "{id}");
        }
        let lm = |b: Builder| on(b, "looker-pc", "PC");
        assert_eq!(answer(&st, lm(ask("POST", "/me/profiles/own/lease", &looker)), None).await.0, StatusCode::OK);
        let (code, _, raw) = bytes_answer(&st, lm(ask("GET", "/me/profiles/own/session", &looker)), vec![]).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(split_session(&raw).0["proxy"]["name"], "Home");
        // Someone else's own profile is not there for anyone else.
        let (code, _, _) = bytes_answer(&st, mac(ask("GET", "/me/profiles/own/session", &vera)), vec![]).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (code, _, _) = bytes_answer(&st, lm(ask("GET", "/profiles/own/session", &looker)), vec![]).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "not through the team's door either");
    }

    #[tokio::test]
    async fn a_session_upload_needs_the_lease_and_the_revision_it_was_made_from() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        let (sam_id, _) = member(&st, "Sam", vec![allow(folder("Ads"), Action::See)]).await;
        let (_, stranger) = member(&st, "Stranger", vec![]).await;
        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let mac = |b: Builder| on(b, "mac", "Vera's Mac");
        let put = |q: &str| format!("/profiles/p1/session{q}");
        let jar = b"the session jar".to_vec();

        let (code, _, raw) = bytes_answer(&st, mac(ask("PUT", &put(""), &vera)), jar.clone()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::BAD_REQUEST, "bad_request".into()), "no base");
        let (code, _, _) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=x"), &vera)), jar.clone()).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let (code, _, raw) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=0"), &vera)), jar.clone()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::CONFLICT, "no_lease".into()));

        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        // Who has it open is not the stranger's to know.
        let (code, _, raw) =
            bytes_answer(&st, on(ask("PUT", &put("?base_session_rev=0"), &stranger), "s", "S"), jar.clone()).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{}", String::from_utf8_lossy(&raw));
        // Her other device is told where it is open; a script has no device to hold it with.
        let (code, _, raw) = bytes_answer(&st, on(ask("PUT", &put("?base_session_rev=0"), &vera), "pc", "PC"), jar.clone()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::CONFLICT, "leased".into()));
        assert_eq!(json_of(&raw)["lease"]["device_name"], "Vera's Mac");
        let (code, _, _) = bytes_answer(&st, ask("PUT", &put("?base_session_rev=0"), &vera), jar.clone()).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(heard(&mut sam_live).iter().all(|e| *e != Event::TeamChanged { what: "session", id: "p1".into() }));

        let (code, _, raw) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=0"), &vera)), jar.clone()).await;
        assert_eq!(code, StatusCode::OK);
        let got = json_of(&raw);
        assert_eq!((got["session_rev"].as_u64(), got["size"].as_u64()), (Some(1), Some(jar.len() as u64)));
        assert_eq!(got["updated_by"], vera_id.as_str());
        assert_eq!(heard(&mut sam_live), vec![Event::TeamChanged { what: "session", id: "p1".into() }]);
        // A laptop that read revision 0 and slept through the upload above.
        let (code, _, raw) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=0"), &vera)), b"old".to_vec()).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(
            json_of(&raw),
            json!({ "error": "changed since you read it", "code": "stale", "current_session_rev": 1 })
        );
        let (code, _, _) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=1"), &vera)), jar.clone()).await;
        assert_eq!(code, StatusCode::OK);

        // Not the profile's revision, and never in its rows.
        let (_, one) = answer(&st, ask("GET", "/profiles/p1", &vera), None).await;
        assert_eq!(
            (one["revision"].as_u64(), one["session_rev"].as_u64(), one["session_size"].as_u64()),
            (Some(1), Some(2), Some(jar.len() as u64))
        );
        let (_, list) = answer(&st, ask("GET", "/profiles", &vera), None).await;
        assert_eq!(list[0]["session_rev"], 2);
        for v in [&one, &list] {
            let text = v.to_string();
            assert!(!text.contains("session jar") && !text.contains(&STANDARD.encode(&jar)), "{text}");
        }
        // A settings save from revision 1 is not stale because of it.
        let (code, got) =
            answer(&st, mac(ask("PUT", "/profiles/p1", &vera)), Some(based_on(profile_json("Shop", "Ads", b"v2"), 1))).await;
        assert_eq!((code, got["revision"].as_u64()), (StatusCode::OK, Some(2)));

        // An empty session is a session too.
        let (code, _, _) = bytes_answer(&st, mac(ask("PUT", &put("?base_session_rev=2"), &vera)), vec![]).await;
        assert_eq!(code, StatusCode::OK);
        let (_, _, raw) = bytes_answer(&st, mac(ask("GET", &put(""), &vera)), vec![]).await;
        let (front, rest) = split_session(&raw);
        assert_eq!((front["session_rev"].as_u64(), rest.len()), (Some(3), 0));
    }

    #[tokio::test]
    async fn a_session_has_a_limit_of_its_own_and_every_other_route_keeps_its_own() {
        let st = state();
        st.store.put_profile("p1", "Shop", "", b"v1", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let mac = |b: Builder| on(b, "mac", "Mac");
        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        let put = |q: &str| mac(ask("PUT", &format!("/profiles/p1/session{q}"), &vera));
        assert_eq!(BLOB_MAX, 256 * 1024 * 1024);

        // Declared past it: refused before a byte is read.
        let b = put("?base_session_rev=0").header(CONTENT_LENGTH, (BLOB_MAX + 1).to_string());
        let (code, _, raw) = bytes_answer(&st, b, b"tiny".to_vec()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::PAYLOAD_TOO_LARGE, "too_large".into()));
        // Past what every other route takes, and still taken here.
        let big = vec![7u8; MAX_BODY + 1024 * 1024];
        let (code, _, raw) = bytes_answer(&st, put("?base_session_rev=0"), big.clone()).await;
        assert_eq!((code, json_of(&raw)["size"].as_u64()), (StatusCode::OK, Some(big.len() as u64)));
        for uri in ["/profiles/p2", "/items/bookmark/b1", "/me/items/tag/t1"] {
            let req = ask("PUT", uri, &vera)
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(MAX_BODY + 1)))
                .unwrap();
            assert_eq!(call(&st, req).await, StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
        }

        // With no length declared it is counted as it comes, and nothing is kept.
        let mut small = st.clone();
        small.session_max = 1024;
        let (code, _, raw) = bytes_answer(&small, put("?base_session_rev=1"), vec![1u8; 2000]).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::PAYLOAD_TOO_LARGE, "too_large".into()));
        let (code, _, _) = bytes_answer(&small, put("?base_session_rev=1"), vec![1u8; 1024]).await;
        assert_eq!(code, StatusCode::OK, "right at the limit");
        let (listed, _) = st.store.listed_one("p1").unwrap().unwrap();
        assert_eq!(listed.session.map(|m| (m.rev, m.size)), Some((2, 1024)));
    }

    /// A body that never sends another byte.
    struct Stalled;
    impl http_body::Body for Stalled {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_stalls_is_dropped_rather_than_waited_on() {
        let st = state();
        let inc = Incoming::new(&st, st.store.package_upload("personal:m1", "e1").unwrap());
        let e = receive(&st, inc, Body::new(Stalled), 1024, None).await.err().unwrap();
        assert_eq!((e.status, e.code), (StatusCode::REQUEST_TIMEOUT, code::BAD_REQUEST));
    }

    #[tokio::test]
    async fn a_removed_member_cannot_upload_even_with_the_lease_still_live() {
        let st = state();
        let a = admin(&st);
        st.store.put_profile("p1", "Shop", "", b"v1", "setup").unwrap();
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        let (off_id, off) = member(&st, "Off", everything()).await;
        let base = || BaseSession { base_session_rev: Some(0) };
        let with_device = |token: &str| {
            let mut h = hdr(token);
            h.insert(DEVICE_ID, HeaderValue::from_static("mac"));
            h
        };

        // Her request is past the door; then she is removed, and the lease goes with her.
        assert_eq!(answer(&st, on(ask("POST", "/profiles/p1/lease", &vera), "mac", "Mac"), None).await.0, StatusCode::OK);
        let h = with_device(&vera);
        let c = require_member(&st, &h).unwrap();
        member_delete(State(st.clone()), hdr(&a), Path(vera_id.clone())).await.unwrap();
        // Even a lease still live at that instant does not let the upload in.
        st.store.lease_acquire("p1", &vera_id, "mac", "Mac", 90).unwrap().unwrap();
        let e = write_session(&st, &c, TEAM, "p1".into(), base(), &h, Body::from("jar")).await.unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::UNAUTHORIZED, code::UNAUTHORIZED));
        let (code, _, raw) =
            bytes_answer(&st, on(ask("PUT", "/profiles/p1/session?base_session_rev=0", &vera), "mac", "Mac"), b"jar".to_vec()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::UNAUTHORIZED, "unauthorized".into()));
        assert!(st.store.session_sealed("p1").unwrap().is_none(), "nothing of hers was kept");

        // Switched off: refused too, with the code that says why.
        st.store.lease_release("p1", &vera_id, "mac").unwrap();
        assert_eq!(answer(&st, on(ask("POST", "/profiles/p1/lease", &off), "mac", "Mac"), None).await.0, StatusCode::OK);
        let h = with_device(&off);
        let c = require_member(&st, &h).unwrap();
        answer(&st, ask("PATCH", &format!("/admin/members/{off_id}"), &a), Some(json!({ "active": false }))).await;
        st.store.lease_acquire("p1", &off_id, "mac", "Mac", 90).unwrap().unwrap();
        let e = write_session(&st, &c, TEAM, "p1".into(), base(), &h, Body::from("jar")).await.unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::FORBIDDEN, code::SWITCHED_OFF));
        assert!(st.store.session_sealed("p1").unwrap().is_none());
    }

    #[tokio::test]
    async fn every_profile_row_says_when_it_was_made() {
        let st = state();
        let a = admin(&st);
        let dated = |mut b: Value, at: Value| {
            b["created_at"] = at;
            b
        };
        let first = dated(profile_json("Shop", "Ads", b"s"), json!(1_700_000_000));
        let (code, made) = answer(&st, ask("PUT", "/profiles/p1", &a), Some(first)).await;
        assert_eq!((code, made["created_at"].clone()), (StatusCode::CREATED, json!(1_700_000_000)));
        let (_, list) = answer(&st, ask("GET", "/profiles", &a), None).await;
        let (_, one) = answer(&st, ask("GET", "/profiles/p1", &a), None).await;
        assert_eq!((list[0]["created_at"].clone(), one["created_at"].clone()), (json!(1_700_000_000), json!(1_700_000_000)));
        // An edit's own date changes nothing.
        let later = dated(based_on(profile_json("Shop", "Ads", b"s2"), 1), json!(1_800_000_000));
        let (_, edited) = answer(&st, ask("PUT", "/profiles/p1", &a), Some(later)).await;
        assert_eq!(edited["created_at"], json!(1_700_000_000));
        // None sent: dated by the server, and the date goes with a move into the team.
        let (_, own) = answer(&st, ask("PUT", "/me/profiles/p2", &a), Some(profile_json("Mine", "", b"m"))).await;
        let at = own["created_at"].as_u64().expect("dated now");
        let (code, moved) = answer(&st, ask("POST", "/me/profiles/p2/to-team", &a), None).await;
        assert_eq!((code, moved["created_at"].as_u64()), (StatusCode::OK, Some(at)));
    }

    #[tokio::test]
    async fn a_rows_card_and_where_it_is_open_are_for_those_who_may_see_it() {
        let st = state();
        let a = admin(&st);
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"login", "setup").unwrap();
        let ads: Vec<Grant> =
            [Action::See, Action::Start, Action::Edit, Action::Create].iter().map(|x| allow(folder("Ads"), *x)).collect();
        let (_, vera) = member(&st, "Vera", ads).await;
        let (_, sam) = member(&st, "Sam", vec![allow(folder("Shops"), Action::See)]).await;
        let card = json!({ "proxy_id": "x1", "color": "#c33", "notes": "warm up first", "tags": ["fb", "ads"] });
        let with_card = |c: Value, rev: Option<u64>| {
            let mut b = profile_json("Shop", "Ads", b"s");
            b["card"] = c;
            match rev {
                Some(r) => based_on(b, r),
                None => b,
            }
        };

        // Vera may not use the proxy, so she may not hand it to whoever may Start.
        let (code, got) = answer(&st, ask("PUT", "/profiles/p1", &vera), Some(with_card(card.clone(), None))).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_proxy")));
        assert_eq!(answer(&st, ask("PUT", "/profiles/p1", &a), Some(with_card(card.clone(), None))).await.0, StatusCode::CREATED);
        // Nobody binds a proxy that is not there, or one from somebody's own space.
        assert_eq!(answer(&st, ask("PUT", "/me/proxies/x-own", &a), Some(proxy_json("Home"))).await.0, StatusCode::CREATED);
        for x in ["x-nope", "x-own"] {
            let (code, _) = answer(&st, ask("PUT", "/profiles/p1", &a), Some(with_card(json!({ "proxy_id": x }), Some(1)))).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{x}");
        }
        // Keeping the binding asks nothing of her; no card at all keeps it.
        let (code, _) = answer(&st, ask("PUT", "/profiles/p1", &vera), Some(with_card(card.clone(), Some(1)))).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) =
            answer(&st, ask("PUT", "/profiles/p1", &vera), Some(based_on(profile_json("Shop", "Ads", b"s2"), 2))).await;
        assert_eq!(code, StatusCode::OK);

        let mac = |b: Builder| on(b, "vera-mac", "MacBook Веры");
        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        let (_, list) = answer(&st, ask("GET", "/profiles", &vera), None).await;
        assert_eq!(list[0]["card"], card);
        let open = &list[0]["open_on"];
        assert_eq!((open["device_name"].as_str(), open["member_name"].as_str()), (Some("MacBook Веры"), Some("Vera")));
        let (_, one) = answer(&st, ask("GET", "/profiles/p1", &vera), None).await;
        assert_eq!((one["card"].clone(), one["open_on"]["member_name"].clone()), (card.clone(), json!("Vera")));
        // Sam may not see it: no row, so no card and nothing of where it is open.
        for uri in ["/profiles", "/profiles?ids=p1"] {
            assert_eq!(answer(&st, ask("GET", uri, &sam), None).await.1, json!([]), "{uri}");
        }

        // The limits, in characters, before anything is written.
        let bad = [
            json!({ "notes": "n".repeat(2001) }),
            json!({ "color": "c".repeat(33) }),
            json!({ "tags": vec!["t"; 51] }),
            json!({ "tags": ["t".repeat(65)] }),
            json!({ "proxy_id": "x".repeat(129) }),
        ];
        for c in bad {
            let (code, got) = answer(&st, mac(ask("PUT", "/profiles/p1", &vera)), Some(with_card(c.clone(), Some(3)))).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")), "{c}");
        }
        let full = json!({ "proxy_id": "x1", "notes": "н".repeat(2000), "color": "c".repeat(32), "tags": vec!["т".repeat(64); 50] });
        let (code, _) = answer(&st, mac(ask("PUT", "/profiles/p1", &vera)), Some(with_card(full, Some(3)))).await;
        assert_eq!(code, StatusCode::OK, "right at the limits");
        // A blank binding is none.
        let (code, _) = answer(&st, mac(ask("PUT", "/profiles/p1", &vera)), Some(with_card(json!({ "proxy_id": " " }), Some(4)))).await;
        assert_eq!(code, StatusCode::OK);
        let (_, one) = answer(&st, ask("GET", "/profiles/p1", &vera), None).await;
        assert_eq!(one["card"], json!({ "proxy_id": null, "color": "", "notes": "", "tags": [] }));
    }

    #[tokio::test]
    async fn team_items_ask_team_wide_rights_and_keep_revisions() {
        let st = state();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let (looker_id, looker) = member(&st, "Looker", vec![allow(Scope::Team, Action::See)]).await;
        let (_, ads) = member(&st, "Ads", Action::ALL.iter().map(|x| allow(folder("Ads"), *x)).collect()).await;
        let (_, nobody) = member(&st, "Nobody", vec![]).await;
        let mut looker_live = st.hub.subscribe(&looker_id).await;

        for kind in ITEM_KINDS {
            let uri = format!("/items/{kind}/i1");
            let list = format!("/items/{kind}");
            let body = json!({ "name": format!("A {kind}"), "data": STANDARD.encode(format!("{kind} data")) });
            heard(&mut looker_live);

            // A folder's rights do not reach what is in no folder; seeing is not making.
            for who in [&ads, &looker] {
                assert_eq!(answer(&st, ask("PUT", &uri, who), Some(body.clone())).await.0, StatusCode::FORBIDDEN, "{kind}");
            }
            let (code, got) = answer(&st, ask("PUT", &uri, &vera), Some(body.clone())).await;
            assert_eq!((code, got["revision"].as_u64(), got["kind"].as_str()), (StatusCode::CREATED, Some(1), Some(kind)));
            assert!(got.get("data").is_none());
            assert_eq!(heard(&mut looker_live), vec![Event::TeamChanged { what: kind, id: "i1".into() }]);

            // See reads; nobody else learns it is there.
            let (_, rows) = answer(&st, ask("GET", &list, &looker), None).await;
            assert_eq!(rows.as_array().unwrap().len(), 1, "{kind}");
            assert_eq!(STANDARD.decode(rows[0]["data"].as_str().unwrap()).unwrap(), format!("{kind} data").as_bytes());
            let (code, one) = answer(&st, ask("GET", &uri, &looker), None).await;
            assert_eq!((code, one["name"].clone()), (StatusCode::OK, json!(format!("A {kind}"))));
            for who in [&ads, &nobody] {
                assert_eq!(answer(&st, ask("GET", &list, who), None).await.1, json!([]), "{kind}");
                assert_eq!(answer(&st, ask("GET", &uri, who), None).await.0, StatusCode::NOT_FOUND, "{kind}");
            }

            // Edits by revision, and only with Edit.
            assert_eq!(answer(&st, ask("PUT", &uri, &looker), Some(based_on(body.clone(), 1))).await.0, StatusCode::FORBIDDEN);
            let (code, got) = answer(&st, ask("PUT", &uri, &vera), Some(based_on(body.clone(), 1))).await;
            assert_eq!((code, got["revision"].as_u64()), (StatusCode::OK, Some(2)));
            let (code, got) = answer(&st, ask("PUT", &uri, &vera), Some(based_on(body.clone(), 1))).await;
            assert_eq!((code, got["code"].as_str(), got["current_revision"].as_u64()), (StatusCode::CONFLICT, Some("stale"), Some(2)));

            // Deleting asks Delete, goes to the trash and keeps the id taken.
            assert_eq!(answer(&st, ask("DELETE", &uri, &looker), None).await.0, StatusCode::FORBIDDEN);
            let (code, _) = answer(&st, ask("DELETE", &format!("{uri}?base_revision=1"), &vera), None).await;
            assert_eq!(code, StatusCode::CONFLICT);
            assert_eq!(answer(&st, ask("DELETE", &uri, &vera), None).await.0, StatusCode::NO_CONTENT);
            assert_eq!(answer(&st, ask("GET", &uri, &vera), None).await.0, StatusCode::NOT_FOUND);
            let (code, got) = answer(&st, ask("PUT", &uri, &vera), Some(body.clone())).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("in_trash")));
            let (_, trash) = answer(&st, ask("GET", "/trash", &looker), None).await;
            let row = trash.as_array().unwrap().iter().find(|t| t["kind"] == kind && t["id"] == "i1").unwrap().clone();
            assert_eq!((row["name"].clone(), row["folder"].clone()), (json!(format!("A {kind}")), json!("")));
            assert!(row["purge_at"].is_u64());
            assert!(answer(&st, ask("GET", "/trash", &ads), None).await.1.as_array().unwrap().iter().all(|t| t["kind"] != kind));

            // Brought back by its kind, with Delete.
            assert_eq!(answer(&st, ask("POST", "/trash/i1/restore", &vera), None).await.0, StatusCode::NOT_FOUND);
            let restore = format!("/trash/i1/restore?kind={kind}");
            assert_eq!(answer(&st, ask("POST", &restore, &looker), None).await.0, StatusCode::FORBIDDEN);
            let (code, got) = answer(&st, ask("POST", &restore, &vera), None).await;
            assert_eq!((code, got["item"]["revision"].as_u64(), got["item"]["kind"].as_str()), (StatusCode::OK, Some(3), Some(kind)));
            assert_eq!(answer(&st, ask("GET", &uri, &looker), None).await.0, StatusCode::OK);
        }

        // Purged for good by its kind.
        assert_eq!(answer(&st, ask("DELETE", "/items/tag/i1", &vera), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("DELETE", "/trash/i1?kind=bookmark", &vera), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(answer(&st, ask("DELETE", "/trash/i1?kind=tag", &vera), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("POST", "/trash/i1/restore?kind=tag", &vera), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(answer(&st, ask("GET", "/items/bookmark/i1", &vera), None).await.0, StatusCode::OK, "the others stay");

        // Only the kinds there are, and ids of a sane length.
        for uri in ["/items/note", "/items/note/i1", "/me/items/note"] {
            assert_eq!(answer(&st, ask("GET", uri, &vera), None).await.0, StatusCode::NOT_FOUND, "{uri}");
        }
        assert_eq!(answer(&st, ask("DELETE", "/trash/i1?kind=note", &vera), None).await.0, StatusCode::NOT_FOUND);
        let long = format!("/items/tag/{}", "i".repeat(129));
        let (code, _) = answer(&st, ask("PUT", &long, &vera), Some(json!({ "data": "" }))).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn my_items_are_mine_alone_and_go_for_good() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (_, sam) = member(&st, "Sam", everything()).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let body = json!({ "name": "Mine", "data": STANDARD.encode(b"own") });

        // No grants asked in my own space.
        assert_eq!(answer(&st, ask("PUT", "/me/items/tag/t1", &vera), Some(body.clone())).await.0, StatusCode::CREATED);
        assert_eq!(heard(&mut vera_live), vec![Event::PersonalChanged { what: "tag", id: "t1".into() }]);
        assert_eq!(answer(&st, ask("GET", "/me/items/tag", &sam), None).await.1, json!([]));
        assert_eq!(answer(&st, ask("GET", "/me/items/tag/t1", &sam), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(answer(&st, ask("GET", "/items/tag", &sam), None).await.1, json!([]));
        // The team may hold the same id: one per kind per space.
        assert_eq!(answer(&st, ask("PUT", "/items/tag/t1", &sam), Some(json!({ "data": "" }))).await.0, StatusCode::CREATED);
        let (_, mine) = answer(&st, ask("GET", "/me/items/tag", &vera), None).await;
        assert_eq!((mine.as_array().unwrap().len(), mine[0]["name"].clone()), (1, json!("Mine")));

        let (code, _) = answer(&st, ask("PUT", "/me/items/tag/t1", &vera), Some(based_on(body.clone(), 1))).await;
        assert_eq!(code, StatusCode::OK);
        let (code, got) = answer(&st, ask("PUT", "/me/items/tag/t1", &vera), Some(based_on(body.clone(), 1))).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("stale")));

        assert_eq!(answer(&st, ask("DELETE", "/me/items/tag/t1", &vera), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("GET", "/me/items/tag/t1", &vera), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(answer(&st, ask("DELETE", "/me/items/tag/t1", &vera), None).await.0, StatusCode::NOT_FOUND);
        assert!(st.store.trash().unwrap().is_empty(), "no trash for my own");
        assert_eq!(answer(&st, ask("GET", "/items/tag/t1", &sam), None).await.0, StatusCode::OK, "the team's stays");
    }

    #[tokio::test]
    async fn one_of_my_devices_takes_my_profile_over_and_the_other_loses_it() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (_, sam) = member(&st, "Sam", everything()).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(profile_json("Own", "", b"x"))).await;
        assert_eq!(code, StatusCode::CREATED);
        let laptop = |b: Builder| on(b, "vera-laptop", "Vera's MacBook");
        let desktop = |b: Builder| on(b, "vera-desktop", "Vera's PC");
        let lease = "/me/profiles/own/lease";
        let over = "/me/profiles/own/lease?takeover=true";
        assert_eq!(answer(&st, laptop(ask("POST", lease, &vera)), None).await.0, StatusCode::OK);
        heard(&mut vera_live);

        // Asked plainly, the desktop is told where it is open; asked to take it, it has it.
        let (code, body) = answer(&st, desktop(ask("POST", lease, &vera)), None).await;
        assert_eq!((code, body["lease"]["device_name"].as_str()), (StatusCode::CONFLICT, Some("Vera's MacBook")));
        let (code, got) = answer(&st, desktop(ask("POST", over, &vera)), None).await;
        assert_eq!((code, got["device_id"].as_str()), (StatusCode::OK, Some("vera-desktop")));
        assert_eq!(heard(&mut vera_live), vec![Event::PersonalChanged { what: "lease", id: "own".into() }]);

        // The laptop's next renewal, and its upload, meet the desktop's lease.
        let (code, body) = answer(&st, laptop(ask("POST", lease, &vera)), None).await;
        assert_eq!((code, body["lease"]["device_name"].as_str()), (StatusCode::CONFLICT, Some("Vera's PC")));
        let put = "/me/profiles/own/session?base_session_rev=0";
        let (code, _, raw) = bytes_answer(&st, laptop(ask("PUT", put, &vera)), b"jar".to_vec()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::CONFLICT, "leased".into()));
        // Taken again by the device holding it, it is a renewal.
        assert_eq!(answer(&st, desktop(ask("POST", over, &vera)), None).await.0, StatusCode::OK);

        // Only in one's own space, and only one's own: nobody takes a team profile or another's.
        st.store.put_profile("p1", "Shop", "Ads", b"v1", "setup").unwrap();
        let sam_pc = |b: Builder| on(b, "sam-pc", "Sam's PC");
        assert_eq!(answer(&st, sam_pc(ask("POST", "/profiles/p1/lease", &sam)), None).await.0, StatusCode::OK);
        let (code, body) = answer(&st, on(ask("POST", "/profiles/p1/lease?takeover=true", &sam), "sam-mac", "M"), None).await;
        assert_eq!((code, body["code"].as_str()), (StatusCode::CONFLICT, Some("leased")), "not in the team");
        let (code, _) = answer(&st, desktop(ask("POST", "/profiles/p1/lease?takeover=true", &vera)), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "not hers to see");
        assert_eq!(answer(&st, sam_pc(ask("POST", over, &sam)), None).await.0, StatusCode::NOT_FOUND);
        // A script has no device to take it with.
        assert_eq!(answer(&st, ask("POST", over, &vera), None).await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_members_own_space_keeps_how_their_devices_sync_and_the_team_has_none() {
        let st = state();
        let (_, vera) = member(&st, "Vera", vec![]).await;
        let (_, sam) = member(&st, "Sam", everything()).await;
        let doc = json!({ "name": "sync", "data": STANDARD.encode(br#"{"generation":"g1"}"#) });
        assert_eq!(answer(&st, ask("PUT", "/me/items/sync/space", &vera), Some(doc.clone())).await.0, StatusCode::CREATED);
        let (code, got) = answer(&st, ask("GET", "/me/items/sync/space", &vera), None).await;
        assert_eq!((code, got["revision"].as_u64()), (StatusCode::OK, Some(1)));
        assert_eq!(answer(&st, ask("GET", "/me/items/sync", &sam), None).await.1, json!([]));
        for uri in ["/items/sync", "/items/sync/space"] {
            assert_eq!(answer(&st, ask("GET", uri, &sam), None).await.0, StatusCode::NOT_FOUND, "{uri}");
        }
        assert_eq!(answer(&st, ask("PUT", "/items/sync/space", &sam), Some(doc)).await.0, StatusCode::NOT_FOUND);
        let (code, _, _) = bytes_answer(&st, ask("PUT", "/me/items/sync/space/package", &vera), b"x".to_vec()).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "no package but an extension's");
        assert_eq!(answer(&st, ask("DELETE", "/me/items/sync/space", &vera), None).await.0, StatusCode::NO_CONTENT);
        assert!(st.store.trash().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_extensions_package_is_a_blob_of_its_own() {
        let st = state();
        let see = allow(Scope::Team, Action::See);
        let (editor_id, editor) = member(&st, "Editor", vec![see.clone(), allow(Scope::Team, Action::Edit)]).await;
        let (_, maker) = member(&st, "Maker", vec![see.clone(), allow(Scope::Team, Action::Create)]).await;
        let (looker_id, looker) = member(&st, "Looker", vec![see]).await;
        let (_, nobody) = member(&st, "Nobody", vec![]).await;
        let mut looker_live = st.hub.subscribe(&looker_id).await;
        let ext = json!({ "name": "uBlock", "data": STANDARD.encode(br#"{"version":"1.0"}"#) });
        assert_eq!(answer(&st, ask("PUT", "/items/extension/e1", &maker), Some(ext.clone())).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("PUT", "/items/bookmark/b1", &maker), Some(ext)).await.0, StatusCode::CREATED);
        let pkg = |q: &str| format!("/items/extension/e1/package{q}");
        let zip: Vec<u8> = (0..50_000u32).map(|i| (i % 253) as u8).collect();
        let zip2: Vec<u8> = zip.iter().rev().copied().collect();

        // The first is part of making it; changing it after is Edit.
        let (code, _, raw) = bytes_answer(&st, ask("PUT", &pkg(""), &maker), zip.clone()).await;
        assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&raw));
        assert_eq!((json_of(&raw)["package_rev"].as_u64(), json_of(&raw)["size"].as_u64()), (Some(1), Some(50_000)));
        heard(&mut looker_live);
        for (who, want) in [(&maker, StatusCode::FORBIDDEN), (&looker, StatusCode::FORBIDDEN), (&nobody, StatusCode::NOT_FOUND)] {
            assert_eq!(bytes_answer(&st, ask("PUT", &pkg(""), who), zip2.clone()).await.0, want);
        }
        let (code, _, raw) = bytes_answer(&st, ask("PUT", &pkg("?base_package_rev=0"), &editor), zip2.clone()).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(json_of(&raw), json!({ "error": "changed since you read it", "code": "stale", "current_package_rev": 1 }));
        let (code, _, raw) = bytes_answer(&st, ask("PUT", &pkg("?base_package_rev=1"), &editor), zip2.clone()).await;
        assert_eq!((code, json_of(&raw)["updated_by"].clone()), (StatusCode::OK, json!(editor_id)));
        assert_eq!(heard(&mut looker_live), vec![Event::TeamChanged { what: "package", id: "e1".into() }]);

        // Reading is See.
        let (code, headers, raw) = bytes_answer(&st, ask("GET", &pkg(""), &looker), vec![]).await;
        assert_eq!((code, headers[PACKAGE_REV].to_str().unwrap()), (StatusCode::OK, "2"));
        assert!(raw == zip2, "the bytes as sent");
        assert_eq!(bytes_answer(&st, ask("GET", &pkg(""), &nobody), vec![]).await.0, StatusCode::NOT_FOUND);
        let (_, rows) = answer(&st, ask("GET", "/items/extension", &looker), None).await;
        assert_eq!((rows[0]["package"]["package_rev"].as_u64(), rows[0]["package"]["size"].as_u64()), (Some(2), Some(50_000)));
        let (_, rows) = answer(&st, ask("GET", "/items/bookmark", &looker), None).await;
        assert!(rows[0]["package"].is_null());

        // Only an extension that is there has one.
        for uri in ["/items/bookmark/b1/package", "/items/extension/e-nope/package", "/items/note/e1/package"] {
            assert_eq!(bytes_answer(&st, ask("PUT", uri, &editor), b"zip".to_vec()).await.0, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(bytes_answer(&st, ask("GET", uri, &editor), vec![]).await.0, StatusCode::NOT_FOUND, "{uri}");
        }
        // Its own limit, as for a session.
        let mut small = st.clone();
        small.package_max = 1024;
        let (code, _, raw) = bytes_answer(&small, ask("PUT", &pkg(""), &editor), vec![0u8; 2000]).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::PAYLOAD_TOO_LARGE, "too_large".into()));

        // Mine too, and no one else's to read.
        let own = json!({ "name": "Own", "data": "" });
        assert_eq!(answer(&st, ask("PUT", "/me/items/extension/e1", &nobody), Some(own)).await.0, StatusCode::CREATED);
        let (code, _, _) = bytes_answer(&st, ask("PUT", "/me/items/extension/e1/package", &nobody), b"own zip".to_vec()).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _, raw) = bytes_answer(&st, ask("GET", "/me/items/extension/e1/package", &nobody), vec![]).await;
        assert_eq!((code, raw), (StatusCode::OK, b"own zip".to_vec()));
        assert_eq!(bytes_answer(&st, ask("GET", "/me/items/extension/e1/package", &looker), vec![]).await.0, StatusCode::NOT_FOUND);
    }

    /// A member with rights on one folder only gets what starting a profile
    /// there needs: its folder's bookmarks and the extensions it names, nothing else.
    #[tokio::test]
    async fn a_start_brings_its_bookmarks_and_extensions_to_a_folder_member() {
        let st = state();
        let a = admin(&st);
        let ads = |action| allow(folder("Ads"), action);
        let (_, vera) = member(&st, "Vera", vec![ads(Action::See), ads(Action::Start)]).await;
        let (_, sam) = member(&st, "Sam", vec![ads(Action::See)]).await;
        let settings = br#"{"_meta":{"extensions":["e1"]}}"#;
        for (id, f) in [("p1", "Ads"), ("p2", "Other")] {
            assert_eq!(answer(&st, ask("PUT", &format!("/profiles/{id}"), &a), Some(profile_json(id, f, settings))).await.0, StatusCode::CREATED);
        }
        let item = |data: Value| json!({ "name": "x", "data": STANDARD.encode(data.to_string()) });
        for (id, f) in [("b-ads", "Ads"), ("b-other", "Other"), ("b-all", "")] {
            let b = item(json!({ "id": id, "title": id, "url": "https://example.com/", "folder": f }));
            assert_eq!(answer(&st, ask("PUT", &format!("/items/bookmark/{id}"), &a), Some(b)).await.0, StatusCode::CREATED);
        }
        for id in ["e1", "e2"] {
            assert_eq!(answer(&st, ask("PUT", &format!("/items/extension/{id}"), &a), Some(item(json!({})))).await.0, StatusCode::CREATED);
            let uri = format!("/items/extension/{id}/package");
            assert_eq!(bytes_answer(&st, ask("PUT", &uri, &a), id.as_bytes().to_vec()).await.0, StatusCode::OK);
        }
        let ids = |rows: &Value| rows.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();

        // Without a profile asked about: nothing, as ever.
        assert_eq!(ids(&answer(&st, ask("GET", "/items/bookmark", &vera), None).await.1), Vec::<String>::new());
        let mut marks = ids(&answer(&st, ask("GET", "/items/bookmark?profile=p1", &vera), None).await.1);
        marks.sort();
        assert_eq!(marks, ["b-ads", "b-all"]);
        assert_eq!(ids(&answer(&st, ask("GET", "/items/extension?profile=p1", &vera), None).await.1), ["e1"]);
        let (code, _, raw) = bytes_answer(&st, ask("GET", "/items/extension/e1/package?profile=p1", &vera), vec![]).await;
        assert_eq!((code, raw), (StatusCode::OK, b"e1".to_vec()));
        for uri in ["/items/extension/e2/package?profile=p1", "/items/extension/e1/package", "/items/extension/e1/package?profile=p2"] {
            assert_eq!(bytes_answer(&st, ask("GET", uri, &vera), vec![]).await.0, StatusCode::NOT_FOUND, "{uri}");
        }
        // Not hers to start, or no Start at all: nothing comes with it.
        assert!(ids(&answer(&st, ask("GET", "/items/bookmark?profile=p2", &vera), None).await.1).is_empty());
        assert!(ids(&answer(&st, ask("GET", "/items/extension?profile=p1", &sam), None).await.1).is_empty());
        assert_eq!(bytes_answer(&st, ask("GET", "/items/extension/e1/package?profile=p1", &sam), vec![]).await.0, StatusCode::NOT_FOUND);
    }

    // ---- rights at the edges: bindings, moves, the trash ----

    #[tokio::test]
    async fn binding_a_proxy_asks_for_the_right_to_use_it_not_only_to_see_it() {
        let st = state();
        let a = admin(&st);
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"user:pass@dc", "setup").unwrap();
        let ads = |x| allow(folder("Ads"), x);
        // Sees every proxy and may use none: a folder's rights never reach a proxy.
        let (_, vera) = member(&st, "Vera", vec![allow(Scope::Team, Action::See), ads(Action::Create), ads(Action::Start)]).await;
        assert_eq!(answer(&st, ask("GET", "/proxies/x1", &vera), None).await.0, StatusCode::FORBIDDEN);
        let mut body = profile_json("Shop", "Ads", b"s");
        body["card"] = json!({ "proxy_id": "x1" });
        let (code, got) = answer(&st, ask("PUT", "/profiles/pz", &vera), Some(body.clone())).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_proxy")));
        // One who may start through it binds it.
        let team_start = vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start), ads(Action::Create)];
        let (_, sam) = member(&st, "Sam", team_start).await;
        assert_eq!(answer(&st, ask("PUT", "/profiles/pz", &sam), Some(body)).await.0, StatusCode::CREATED);

        // Out of the team and back into a folder with Start does not hand the login over either.
        let mut bound = profile_json("Bound", "Ads", b"s");
        bound["card"] = json!({ "proxy_id": "x1" });
        assert_eq!(answer(&st, ask("PUT", "/profiles/p1", &a), Some(bound)).await.0, StatusCode::CREATED);
        let bobs = |x| allow(folder("Bob"), x);
        let bob_rights = vec![ads(Action::See), ads(Action::TakeOut), bobs(Action::See), bobs(Action::Create), bobs(Action::Start)];
        let (bob_id, bob) = member(&st, "Bob", bob_rights).await;
        assert_eq!(answer(&st, ask("POST", "/profiles/p1/to-me", &bob), None).await.0, StatusCode::OK);
        let (code, got) = answer(&st, ask("POST", "/me/profiles/p1/to-team", &bob), Some(json!({ "folder": "Bob" }))).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_proxy")));
        assert_eq!(st.store.profile_meta("p1").unwrap().unwrap().1, personal_space(&bob_id));
    }

    #[tokio::test]
    async fn a_folder_with_profiles_waiting_in_the_trash_is_not_empty() {
        let st = state();
        let a = admin(&st);
        let walled: Vec<Grant> = [Action::See, Action::Start, Action::Edit, Action::Delete, Action::Cookies, Action::TakeOut]
            .iter()
            .map(|x| refuse(folder("Secret"), *x))
            .collect();
        let (_, mallory) = member(&st, "Mallory", everything().into_iter().chain(walled).collect()).await;
        let (_, vera) = member(&st, "Vera", vec![allow(folder("Ads"), Action::See), allow(folder("Ads"), Action::Delete)]).await;
        for (id, f) in [("p-sec", "Secret"), ("p1", "Ads")] {
            let (code, _) = answer(&st, ask("PUT", &format!("/profiles/{id}"), &a), Some(profile_json("Payroll", f, b"x"))).await;
            assert_eq!(code, StatusCode::CREATED);
        }
        assert_eq!(answer(&st, ask("DELETE", "/profiles/p-sec", &a), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("DELETE", "/profiles/p1", &vera), None).await.0, StatusCode::NO_CONTENT);

        // Its rules still judge what waits there, so the folder stays, and its name with it.
        for f in ["Secret", "Ads"] {
            let (code, got) = answer(&st, ask("DELETE", &format!("/admin/folders/{f}"), &a), None).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("conflict")), "{f}");
        }
        st.store.create_folder(TEAM, "Open", "").unwrap();
        let (code, _) = answer(&st, ask("PATCH", "/admin/folders/Open", &a), Some(json!({ "name": "Secret" }))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        let (_, trash) = answer(&st, ask("GET", "/trash", &mallory), None).await;
        assert!(trash.as_array().unwrap().iter().all(|t| t["id"] != "p-sec"), "{trash}");
        assert_eq!(answer(&st, ask("POST", "/trash/p-sec/restore", &mallory), None).await.0, StatusCode::NOT_FOUND);
        let (_, trash) = answer(&st, ask("GET", "/trash", &vera), None).await;
        assert_eq!(trash[0]["id"], "p1");
        assert_eq!(answer(&st, ask("POST", "/trash/p1/restore", &vera), None).await.0, StatusCode::OK);

        // Once nothing waits there it goes.
        assert_eq!(answer(&st, ask("DELETE", "/trash/p-sec", &a), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("DELETE", "/admin/folders/Secret", &a), None).await.0, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn the_first_package_on_create_alone_is_for_the_one_who_made_the_extension() {
        let st = state();
        let a = admin(&st);
        let ext = json!({ "name": "uBlock", "data": "" });
        assert_eq!(answer(&st, ask("PUT", "/items/extension/e1", &a), Some(ext.clone())).await.0, StatusCode::CREATED);
        let (_, bob) = member(&st, "Bob", vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Create)]).await;
        let (code, _) = answer(&st, ask("PUT", "/items/extension/e1", &bob), Some(based_on(ext.clone(), 1))).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let (code, _, _) = bytes_answer(&st, ask("PUT", "/items/extension/e1/package", &bob), b"evil zip".to_vec()).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "somebody else's extension");
        // His own, just made: its first package is part of making it.
        assert_eq!(answer(&st, ask("PUT", "/items/extension/e2", &bob), Some(ext)).await.0, StatusCode::CREATED);
        let (code, _, _) = bytes_answer(&st, ask("PUT", "/items/extension/e2/package", &bob), b"zip".to_vec()).await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn out_of_the_team_and_back_is_not_a_way_around_edit() {
        let st = state();
        let a = admin(&st);
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"good-host", "setup").unwrap();
        st.store.put_proxy("x9", "Old", "socks5", "JP", b"old-host", "setup").unwrap();
        for (id, x) in [("p1", "x1"), ("p2", "x9")] {
            let mut bound = profile_json("Shop", "Ads", b"s");
            bound["card"] = json!({ "proxy_id": x });
            assert_eq!(answer(&st, ask("PUT", &format!("/profiles/{id}"), &a), Some(bound)).await.0, StatusCode::CREATED);
        }
        let team = |x| allow(Scope::Team, x);
        let (_, bob) = member(&st, "Bob", vec![team(Action::See), team(Action::Create), team(Action::TakeOut)]).await;
        let (_, _vera) = member(&st, "Vera", vec![allow(Scope::Profile { id: "p3".into() }, Action::See)]).await;

        // A proxy a team profile still binds: back in, it is writing over what that profile uses.
        let (code, _) = answer(&st, ask("PUT", "/proxies/x1", &bob), Some(based_on(proxy_json("Evil"), 1))).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert_eq!(answer(&st, ask("POST", "/proxies/x1/to-me", &bob), None).await.0, StatusCode::OK);
        let (code, _) = answer(&st, ask("PUT", "/me/proxies/x1", &bob), Some(based_on(proxy_json("Evil"), 2))).await;
        assert_eq!(code, StatusCode::OK);
        let (code, got) = answer(&st, ask("POST", "/me/proxies/x1/to-team", &bob), None).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
        // Nor is a purged one made again under the id the binding waits on.
        assert_eq!(answer(&st, ask("DELETE", "/proxies/x9", &a), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("DELETE", "/trash/x9", &a), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(answer(&st, ask("PUT", "/proxies/x9", &bob), Some(proxy_json("Evil"))).await.0, StatusCode::FORBIDDEN);
        // A fresh one goes in on Create alone, made or moved.
        assert_eq!(answer(&st, ask("PUT", "/proxies/x-new", &bob), Some(proxy_json("New"))).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("PUT", "/me/proxies/x-own", &bob), Some(proxy_json("Own"))).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("POST", "/me/proxies/x-own/to-team", &bob), None).await.0, StatusCode::OK);

        // A profile somebody's rules name, the same way.
        assert_eq!(answer(&st, ask("PUT", "/profiles/p3", &a), Some(profile_json("Named", "", b"s"))).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("POST", "/profiles/p3/to-me", &bob), None).await.0, StatusCode::OK);
        let (code, _) = answer(&st, ask("POST", "/me/profiles/p3/to-team", &bob), None).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/p-own", &bob), Some(profile_json("Own", "", b"s"))).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("POST", "/me/profiles/p-own/to-team", &bob), None).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn an_id_waiting_in_the_trash_says_so_to_those_who_may_see_it_there() {
        let st = state();
        st.store.put_proxy("x1", "DC", "socks5", "JP", b"login", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let (_, maker) = member(&st, "Maker", vec![allow(Scope::Team, Action::Create)]).await;
        let tag = json!({ "name": "VIP", "data": "" });
        assert_eq!(answer(&st, ask("PUT", "/items/tag/vip", &vera), Some(tag.clone())).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("PUT", "/profiles/p1", &vera), Some(profile_json("Shop", "Ads", b"s"))).await.0, StatusCode::CREATED);
        for uri in ["/items/tag/vip", "/profiles/p1", "/proxies/x1"] {
            assert_eq!(answer(&st, ask("DELETE", uri, &vera), None).await.0, StatusCode::NO_CONTENT, "{uri}");
        }
        for (uri, body) in [("/items/tag/vip", tag.clone()), ("/profiles/p1", profile_json("Shop", "Ads", b"s")), ("/proxies/x1", proxy_json("DC"))] {
            let (code, got) = answer(&st, ask("PUT", uri, &vera), Some(body.clone())).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::CONFLICT, Some("in_trash")), "{uri}");
            // Not to one who could not see it there: that reads as before.
            let (code, got) = answer(&st, ask("PUT", uri, &maker), Some(body)).await;
            assert_eq!((code, got["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{uri}");
        }
        // Nor through somebody's own space.
        let (code, _) = answer(&st, ask("PUT", "/me/profiles/p1", &vera), Some(profile_json("Mine", "", b"s"))).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_write_based_on_a_revision_of_something_since_deleted_is_stale() {
        let st = state();
        let (_, vera) = member(&st, "Vera", vec![]).await;
        let own = |d: &[u8]| profile_json("Own", "", d);
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(own(b"1"))).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(based_on(own(b"2"), 1))).await.0, StatusCode::OK);
        assert_eq!(answer(&st, ask("DELETE", "/me/profiles/own?base_revision=2", &vera), None).await.0, StatusCode::NO_CONTENT);
        // A device that was offline sends what it queued at revision 2.
        let (code, got) = answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(based_on(own(b"queued"), 2))).await;
        assert_eq!((code, got["code"].as_str(), got["current_revision"].as_u64()), (StatusCode::CONFLICT, Some("stale"), Some(0)));
        assert!(st.store.profile_meta("own").unwrap().is_none(), "not brought back");
        let (code, got) = answer(&st, ask("PUT", "/me/items/bookmark/b1", &vera), Some(based_on(json!({ "data": "" }), 7))).await;
        assert_eq!((code, got["current_revision"].as_u64()), (StatusCode::CONFLICT, Some(0)));
        let (code, _) = answer(&st, ask("PUT", "/me/proxies/x1", &vera), Some(based_on(proxy_json("Home"), 3))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        // Zero is "new": made while free, stale once there.
        let (code, _) = answer(&st, ask("PUT", "/me/items/bookmark/b1", &vera), Some(based_on(json!({ "data": "" }), 0))).await;
        assert_eq!(code, StatusCode::CREATED);
        let (code, got) = answer(&st, ask("PUT", "/me/items/bookmark/b1", &vera), Some(based_on(json!({ "data": "" }), 0))).await;
        assert_eq!((code, got["current_revision"].as_u64()), (StatusCode::CONFLICT, Some(1)));
    }

    #[tokio::test]
    async fn a_profile_moves_into_the_team_only_with_a_binding_that_works_there() {
        let st = state();
        let team = |x| allow(Scope::Team, x);
        let vera_rights = vec![team(Action::See), team(Action::Start), team(Action::Edit), allow(folder("Ads"), Action::Create)];
        let (vera_id, vera) = member(&st, "Vera", vera_rights).await;
        assert_eq!(answer(&st, ask("PUT", "/me/proxies/x-own", &vera), Some(proxy_json("Home"))).await.0, StatusCode::CREATED);
        let mut own = profile_json("Mine", "", b"s");
        own["card"] = json!({ "proxy_id": "x-own" });
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/p1", &vera), Some(own)).await.0, StatusCode::CREATED);

        // Her proxy is not the team's, and she may not bring it: the profile stays hers.
        let (code, got) = answer(&st, ask("POST", "/me/profiles/p1/to-team", &vera), Some(json!({ "folder": "Ads" }))).await;
        assert_eq!((code, got["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_proxy")));
        assert_eq!(st.store.profile_meta("p1").unwrap().unwrap().1, personal_space(&vera_id));
        assert_eq!(answer(&st, ask("POST", "/me/proxies/x-own/to-team", &vera), None).await.0, StatusCode::FORBIDDEN);
        // Unbound, it goes.
        let mut unbound = based_on(profile_json("Mine", "", b"s"), 1);
        unbound["card"] = json!({ "proxy_id": null });
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/p1", &vera), Some(unbound)).await.0, StatusCode::OK);
        let (code, _) = answer(&st, ask("POST", "/me/profiles/p1/to-team", &vera), Some(json!({ "folder": "Ads" }))).await;
        assert_eq!(code, StatusCode::OK);

        // With its proxy moved in first, by one who may, the binding goes along.
        let (_, lena) = member(&st, "Lena", vec![team(Action::See), team(Action::Start), team(Action::Create)]).await;
        assert_eq!(answer(&st, ask("PUT", "/me/proxies/x-l", &lena), Some(proxy_json("Lena's"))).await.0, StatusCode::CREATED);
        let mut hers = profile_json("Hers", "", b"s");
        hers["card"] = json!({ "proxy_id": "x-l" });
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/p-l", &lena), Some(hers)).await.0, StatusCode::CREATED);
        assert_eq!(answer(&st, ask("POST", "/me/proxies/x-l/to-team", &lena), None).await.0, StatusCode::OK);
        assert_eq!(answer(&st, ask("POST", "/me/profiles/p-l/to-team", &lena), None).await.0, StatusCode::OK);
        let (_, row) = answer(&st, ask("GET", "/profiles/p-l", &lena), None).await;
        assert_eq!(row["card"]["proxy_id"], "x-l");
    }

    #[tokio::test]
    async fn a_package_has_a_limit_of_its_own_past_every_other_routes() {
        let st = state();
        let (_, vera) = member(&st, "Vera", vec![]).await;
        assert_eq!((PACKAGE_MAX, MAX_BODY, BLOB_MAX), (64 << 20, 16 << 20, 256 << 20));
        assert_eq!(answer(&st, ask("PUT", "/me/items/extension/e1", &vera), Some(json!({ "data": "" }))).await.0, StatusCode::CREATED);
        let pkg = "/me/items/extension/e1/package";
        // Declared past it: refused before a byte is read.
        let b = ask("PUT", pkg, &vera).header(CONTENT_LENGTH, (PACKAGE_MAX + 1).to_string());
        let (code, _, raw) = bytes_answer(&st, b, b"tiny".to_vec()).await;
        assert_eq!((code, code_of(&raw)), (StatusCode::PAYLOAD_TOO_LARGE, "too_large".into()));
        // Past what every other body may be, and taken here.
        let (code, _, raw) = bytes_answer(&st, ask("PUT", pkg, &vera), vec![1u8; MAX_BODY + 1]).await;
        assert_eq!((code, json_of(&raw)["size"].as_u64()), (StatusCode::OK, Some(MAX_BODY as u64 + 1)));
        // The extension's own JSON keeps the common limit.
        let req = ask("PUT", "/me/items/extension/e2", &vera)
            .header("content-type", "application/json")
            .body(Body::from("x".repeat(MAX_BODY + 1)))
            .unwrap();
        assert_eq!(call(&st, req).await, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // ---- large bodies and the live channel under load ----

    /// The frames of a response body as they come, without joining them.
    async fn frames_of(mut body: Body) -> Vec<Bytes> {
        let mut out = Vec::new();
        while let Some(f) = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await {
            if let Ok(data) = f.unwrap().into_data() {
                out.push(data);
            }
        }
        out
    }

    #[tokio::test]
    async fn a_session_download_comes_a_chunk_at_a_time() {
        let st = state();
        st.store.put_profile("p1", "Shop", "", b"v1", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let mac = |b: Builder| on(b, "mac", "Mac");
        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        let chunk = 512 * 1024;
        let jar: Vec<u8> = (0..3 * chunk + 100).map(|i| (i % 249) as u8).collect();
        let put = mac(ask("PUT", "/profiles/p1/session?base_session_rev=0", &vera));
        assert_eq!(bytes_answer(&st, put, jar.clone()).await.0, StatusCode::OK);

        let req = mac(ask("GET", "/profiles/p1/session", &vera)).body(Body::empty()).unwrap();
        let res = router(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let declared = res.body().size_hint().exact();
        let frames = frames_of(res.into_body()).await;
        assert!(frames.len() >= 4 && frames.iter().all(|f| f.len() <= chunk), "{:?}", frames.iter().map(Bytes::len).collect::<Vec<_>>());
        let all = frames.concat();
        assert_eq!(declared, Some(all.len() as u64), "its length is said up front");
        let (front, rest) = split_session(&all);
        assert_eq!(front["size"].as_u64(), Some(jar.len() as u64));
        assert!(rest == jar, "the bytes as sent");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn uploads_that_stall_keep_nobody_else_waiting() {
        let st = state();
        st.store.put_profile("p1", "Shop", "", b"v1", "setup").unwrap();
        let (_, vera) = member(&st, "Vera", everything()).await;
        let mac = |b: Builder| on(b, "mac", "Mac");
        assert_eq!(answer(&st, mac(ask("POST", "/profiles/p1/lease", &vera)), None).await.0, StatusCode::OK);
        let put = mac(ask("PUT", "/profiles/p1/session?base_session_rev=0", &vera));
        assert_eq!(bytes_answer(&st, put, b"jar".to_vec()).await.0, StatusCode::OK);

        // A member with no rights at all, six profiles of their own, six uploads that never finish.
        let (_, mal) = member(&st, "Mallory", vec![]).await;
        for i in 0..6 {
            let own = format!("/me/profiles/own{i}");
            assert_eq!(answer(&st, ask("PUT", &own, &mal), Some(profile_json("Own", "", b"x"))).await.0, StatusCode::CREATED);
            assert_eq!(answer(&st, on(ask("POST", &format!("{own}/lease"), &mal), "m", "M"), None).await.0, StatusCode::OK);
            let req = on(ask("PUT", &format!("{own}/session?base_session_rev=0"), &mal), "m", "M")
                .body(Body::new(Stalled))
                .unwrap();
            tokio::spawn(router(st.clone()).oneshot(req));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Starting a profile and closing one go through meanwhile.
        let get = bytes_answer(&st, mac(ask("GET", "/profiles/p1/session", &vera)), vec![]);
        let got = tokio::time::timeout(std::time::Duration::from_secs(3), get).await;
        assert_eq!(got.expect("a download waited behind stalled uploads").0, StatusCode::OK);
        let put = bytes_answer(&st, mac(ask("PUT", "/profiles/p1/session?base_session_rev=1", &vera)), b"jar2".to_vec());
        let got = tokio::time::timeout(std::time::Duration::from_secs(3), put).await;
        assert_eq!(got.expect("an upload waited behind stalled uploads").0, StatusCode::OK);
    }

    /// A body that sends its bytes only after a while.
    struct Late {
        wait: std::pin::Pin<Box<tokio::time::Sleep>>,
        data: Option<Bytes>,
    }
    impl http_body::Body for Late {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            use std::future::Future;
            if self.wait.as_mut().poll(cx).is_pending() {
                return std::task::Poll::Pending;
            }
            std::task::Poll::Ready(self.data.take().map(|d| Ok(http_body::Frame::data(d))))
        }
    }

    #[tokio::test]
    async fn an_upload_longer_than_the_lease_has_left_still_lands() {
        let st = state();
        st.store.put_profile("p1", "Shop", "", b"v1", "setup").unwrap();
        let (vera_id, vera) = member(&st, "Vera", everything()).await;
        // Opened a while ago, its last renewal missed: two seconds left.
        st.store.lease_acquire("p1", &vera_id, "mac", "Mac", 2).unwrap().unwrap();
        let late = Late { wait: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(3))), data: Some(Bytes::from_static(b"jar")) };
        let req = on(ask("PUT", "/profiles/p1/session?base_session_rev=0", &vera), "mac", "Mac").body(Body::new(late)).unwrap();
        let res = router(st.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let raw = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&raw));
        // Still hers, with a lease's worth of time to give it back in.
        let lease = st.store.lease_live("p1").unwrap().expect("still open on her Mac");
        assert!(lease.expires_at >= auth::unix_now() + 60, "{lease:?}");
    }

    /// Upgrade a plain socket to the live channel, as a launcher would.
    async fn live_socket(addr: std::net::SocketAddr, token: &str) -> tokio::net::TcpStream {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "GET /live HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
             Authorization: Bearer {token}\r\n\r\n"
        );
        s.write_all(head.as_bytes()).await.unwrap();
        let mut got = Vec::new();
        while !got.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert_eq!(s.read(&mut b).await.unwrap(), 1, "closed during the upgrade");
            got.push(b[0]);
        }
        assert!(got.starts_with(b"HTTP/1.1 101"), "{}", String::from_utf8_lossy(&got));
        s
    }

    /// Whether the server closes the socket within a few seconds.
    async fn closed_soon(s: &mut tokio::net::TcpStream) -> bool {
        use tokio::io::AsyncReadExt;
        let drain = async {
            let mut b = [0u8; 1024];
            loop {
                match s.read(&mut b).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), drain).await.is_ok()
    }

    #[tokio::test]
    async fn a_live_socket_takes_nothing_large_and_a_member_keeps_only_so_many() {
        use tokio::io::AsyncWriteExt;
        let st = state();
        let (_, vera) = member(&st, "Vera", vec![]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // A frame that says it is a megabyte: refused on its head, not waited for.
        let mut s = live_socket(addr, &vera).await;
        let mut head = vec![0x81, 0x80 | 127];
        head.extend_from_slice(&(1u64 << 20).to_be_bytes());
        head.extend_from_slice(&[1, 2, 3, 4]);
        let _ = s.write_all(&head).await;
        let _ = s.write_all(&[0u8; 4096]).await;
        assert!(closed_soon(&mut s).await, "the server waited for the rest of a large frame");

        // One more socket than a member may keep: the oldest goes.
        let mut first = live_socket(addr, &vera).await;
        let mut rest = Vec::new();
        for _ in 0..crate::live::MAX_SOCKETS {
            rest.push(live_socket(addr, &vera).await);
        }
        assert!(closed_soon(&mut first).await, "the oldest socket was kept");
    }

    #[tokio::test]
    async fn a_lease_that_runs_out_is_told_like_one_given_back() {
        let st = state();
        st.store.put_profile("p1", "Shop", "Ads", b"x", "setup").unwrap();
        st.store.put_profile("p2", "Busy", "Ads", b"x", "setup").unwrap();
        let (sam_id, sam) = member(&st, "Sam", vec![allow(folder("Ads"), Action::See)]).await;
        let (vera_id, vera) = member(&st, "Vera", vec![]).await;
        let (nobody_id, _) = member(&st, "Nobody", vec![]).await;
        assert_eq!(answer(&st, ask("PUT", "/me/profiles/own", &vera), Some(profile_json("Own", "", b"x"))).await.0, StatusCode::CREATED);
        let mut sam_live = st.hub.subscribe(&sam_id).await;
        let mut vera_live = st.hub.subscribe(&vera_id).await;
        let mut nobody_live = st.hub.subscribe(&nobody_id).await;
        // A launcher that died with them open: nobody renews, nobody gives back.
        st.store.lease_acquire("p1", &vera_id, "mac", "Mac", 0).unwrap().unwrap();
        st.store.lease_acquire("own", &vera_id, "mac", "Mac", 0).unwrap().unwrap();
        st.store.lease_acquire("p2", &vera_id, "pc", "PC", 90).unwrap().unwrap();
        assert!(answer(&st, ask("GET", "/profiles?ids=p1", &sam), None).await.1[0]["open_on"].is_null());

        assert_eq!(reap_leases(&st).await.unwrap(), 2);
        assert_eq!(heard(&mut sam_live), vec![Event::TeamChanged { what: "lease", id: "p1".into() }]);
        assert_eq!(heard(&mut vera_live), vec![Event::PersonalChanged { what: "lease", id: "own".into() }]);
        assert!(heard(&mut nobody_live).is_empty());
        // Told once; the live one stays.
        assert_eq!(reap_leases(&st).await.unwrap(), 0);
        assert_eq!(st.store.lease_live("p2").unwrap().unwrap().device_name, "PC");
    }

    // ---- what a permission carries ----

    /// Something of every kind a rule can name: folders with and without a
    /// row, an empty one, one holding only the trash, profiles in no folder,
    /// proxies, deleted ones, and someone's own.
    fn varied_team(st: &AppState) {
        let s = &st.store;
        let by = Writer { member_id: ADMIN_ID, device_id: None };
        let any = |_: &Standing, _: Option<&TeamProfile>| Ok(());
        let any_proxy = |_: &Standing, _: Option<&TeamProxy>| Ok(());
        for (id, folder) in [("p1", "Ads"), ("p2", "Ads"), ("p3", "Shops"), ("p5", ""), ("t1", "Bin"), ("t2", "Ads")] {
            s.put_profile_checked(TEAM, id, id, folder, b"x", by, None, any).unwrap().unwrap();
        }
        // Written before folders had rows: the name is only on the profile.
        s.put_profile("p4", "p4", "Legacy", b"x", ADMIN_ID).unwrap();
        s.create_folder(TEAM, "Empty", "").unwrap();
        for id in ["x1", "x2", "tx1"] {
            s.put_proxy_checked(TEAM, id, id, "socks5", "", b"x", by, None, any_proxy).unwrap().unwrap();
        }
        for id in ["t1", "t2"] {
            s.delete_profile_checked(TEAM, id, by, None, |_, _| Ok(())).unwrap().unwrap();
        }
        s.delete_proxy_checked(TEAM, "tx1", by, None, |_, _| Ok(())).unwrap().unwrap();
        let theirs = personal_space("someone");
        s.put_profile_checked(&theirs, "own1", "own1", "Ads", b"x", by, None, any).unwrap().unwrap();
        s.put_proxy_checked(&theirs, "ownx", "ownx", "socks5", "", b"x", by, None, any_proxy).unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_permission_decides_as_the_whole_table_and_names_nothing_hidden() {
        use rand::{rngs::StdRng, Rng, SeedableRng};
        let st = state();
        varied_team(&st);
        let mut scopes = vec![Scope::Team];
        for f in ["", "Ads", "Shops", "Empty", "Legacy", "Bin", "Ghost"] {
            scopes.push(folder(f));
        }
        for id in ["p1", "p2", "p3", "p4", "p5", "t1", "t2", "x1", "x2", "tx1", "own1", "ownx", "nope"] {
            scopes.push(Scope::Profile { id: id.into() });
        }
        let rule = |scope: &str, action, allow| {
            let scope = scopes.iter().find(|s| matches!(s, Scope::Folder { name } | Scope::Profile { id: name } if name == scope)).unwrap().clone();
            Grant { scope, action, allow }
        };
        // A few tables by hand, then many at random.
        let mut tables = vec![
            vec![allow(Scope::Team, Action::See), rule("Ads", Action::See, false), rule("p1", Action::See, true)],
            vec![rule("p1", Action::See, true), rule("Ads", Action::Delete, false), rule("Shops", Action::Start, true)],
            vec![rule("t1", Action::See, true), rule("Bin", Action::Delete, false), rule("Ghost", Action::See, true)],
            vec![allow(Scope::Team, Action::See), rule("own1", Action::See, true), rule("nope", Action::Start, false)],
            vec![rule("Legacy", Action::See, true), rule("x1", Action::See, true), rule("tx1", Action::Delete, true)],
        ];
        let mut rng = StdRng::seed_from_u64(0x5eed);
        for _ in 0..150 {
            // Half see the team, so the narrower rules have something to hide.
            let mut t: Vec<Grant> = Vec::new();
            if rng.gen_bool(0.5) {
                t.push(allow(Scope::Team, Action::See));
            }
            for _ in 0..rng.gen_range(0..=10) {
                t.push(Grant {
                    scope: scopes[rng.gen_range(0..scopes.len())].clone(),
                    action: Action::ALL[rng.gen_range(0..Action::ALL.len())],
                    allow: rng.gen_bool(0.6),
                });
            }
            tables.push(t);
        }

        let (mut left_out, mut named_kept, mut checked) = (0, 0, 0);
        for (n, grants) in tables.into_iter().enumerate() {
            let (id, token) = member(&st, &format!("m{n}"), grants).await;
            let Ok(Json(issued)) = permission_get(State(st.clone()), hdr(&token)).await else {
                panic!("a live member gets a permission");
            };
            let carried = verify_permission(&st.key.public_b64(), &issued.permission).unwrap().grants;
            let full = st.store.grants_of(&id).unwrap();
            let Json(profiles) = profiles_list(State(st.clone()), hdr(&token), whole_list()).await.unwrap();
            let Json(proxies) = proxies_list(State(st.clone()), hdr(&token)).await.unwrap();
            let Json(trash) = trash_list(State(st.clone()), hdr(&token)).await.unwrap();
            let Json(folders) = folders_list(State(st.clone()), hdr(&token)).await.unwrap();

            // Whatever they can see, the launcher decides as the server would.
            let mut targets = vec![Target::team(), Target::folder("")];
            targets.extend(profiles.iter().map(|r| Target::profile(&r.profile.id, &r.profile.folder)));
            targets.extend(proxies.iter().map(|x| proxy_target(&x.id)));
            targets.extend(trash.iter().map(|t| trash_target(&t.item)));
            targets.extend(folders.iter().map(|f| Target::folder(&f.name)));
            // And a folder they may See or make profiles in by name, made yet or not.
            let names = |s: &Scope| match s {
                Scope::Folder { name } if view::names_folder(&full, name) => Some(name.clone()),
                _ => None,
            };
            let named: Vec<String> = scopes.iter().filter_map(names).collect();
            targets.extend(named.iter().map(|n| Target::folder(n)));
            for t in &targets {
                for a in Action::ALL {
                    assert_eq!(decide(&carried, a, t), decide(&full, a, t), "m{n} {a:?} {t:?}\n{full:?}");
                    checked += 1;
                }
            }
            // And nothing in it names what they cannot.
            let sees_folder = |name: &str| {
                name.is_empty()
                    || folders.iter().any(|f| f.name == name)
                    || trash.iter().any(|t| t.item.folder == name)
                    || view::names_folder(&full, name)
            };
            let sees_id = |id: &str| {
                profiles.iter().any(|r| r.profile.id == id)
                    || proxies.iter().any(|x| x.id == id)
                    || trash.iter().any(|t| t.item.id == id)
            };
            for g in &carried {
                assert!(full.contains(g), "m{n}: {g:?} is not theirs");
                match &g.scope {
                    Scope::Team => {}
                    Scope::Folder { name } => assert!(sees_folder(name), "m{n} learns of folder {name:?}"),
                    Scope::Profile { id } => assert!(sees_id(id), "m{n} learns of {id:?}"),
                }
                named_kept += usize::from(g.scope != Scope::Team);
            }
            left_out += full.len() - carried.len();
        }
        // The fixture really did both.
        assert!(left_out > 100 && named_kept > 100, "{left_out} left out, {named_kept} kept");
        assert!(checked > 10_000, "{checked}");
    }

    #[tokio::test]
    async fn a_permission_moves_when_anything_comes_into_view_or_goes() {
        let st = state();
        let a = admin(&st);
        let p9 = || Scope::Profile { id: "p9".into() };
        let (vera, _) = member(&st, "Vera", vec![allow(folder("New"), Action::See), allow(folder("New"), Action::Start)]).await;
        let hidden = Grant { scope: folder("New"), action: Action::See, allow: false };
        let (hid, hid_token) = member(&st, "Hid", vec![allow(Scope::Team, Action::See), hidden]).await;
        let (sam, _) = member(&st, "Sam", vec![allow(Scope::Team, Action::See), allow(Scope::Team, Action::Start)]).await;
        let x9 = Scope::Profile { id: "x9".into() };
        let (pia, _) =
            member(&st, "Pia", vec![allow(p9(), Action::See), allow(p9(), Action::Start), allow(x9, Action::See)]).await;
        let edit_refused = Grant { scope: folder("Shops"), action: Action::Edit, allow: false };
        let (kit, kit_token) = member(&st, "Kit", vec![allow(p9(), Action::See), edit_refused.clone()]).await;

        struct Watcher {
            name: &'static str,
            id: String,
            rx: broadcast::Receiver<Event>,
            epoch: u64,
        }
        let stored = |id: &str| st.store.members().unwrap().into_iter().find(|m| m.id == id).unwrap().epoch;
        let mut watchers = Vec::new();
        for (name, id) in [("vera", &vera), ("hid", &hid), ("sam", &sam), ("pia", &pia), ("kit", &kit)] {
            let rx = st.hub.subscribe(id).await;
            watchers.push(Watcher { name, id: id.clone(), rx, epoch: stored(id) });
        }
        // Who was told since last asked; told the epoch they now have, and nobody else moved.
        let told = |ws: &mut Vec<Watcher>| -> Vec<&'static str> {
            let mut out = Vec::new();
            for w in ws.iter_mut() {
                let now = stored(&w.id);
                let heard: Vec<u64> = heard(&mut w.rx)
                    .into_iter()
                    .filter_map(|e| match e {
                        Event::GrantsChanged { epoch } => Some(epoch),
                        _ => None,
                    })
                    .collect();
                if heard.is_empty() {
                    assert_eq!(now, w.epoch, "{} moved untold", w.name);
                } else {
                    assert_eq!(heard, vec![now], "{}", w.name);
                    out.push(w.name);
                }
                w.epoch = now;
            }
            out
        };
        let step = |method: &str, uri: &str, body: Option<Value>| {
            let (st, a) = (st.clone(), a.clone());
            let (method, uri) = (method.to_owned(), uri.to_owned());
            async move {
                let (code, _) = answer(&st, ask(&method, &uri, &a), body).await;
                assert!(code.is_success(), "{method} {uri}: {code}");
            }
        };

        // A folder Vera's rules already name comes to be, and Sam sees every
        // folder; Hid's rules name it but hide it.
        step("POST", "/admin/folders", Some(json!({ "name": "New" }))).await;
        assert_eq!(told(&mut watchers), ["vera", "sam"]);
        let (_, p) = answer(&st, ask("GET", "/permission", &hid_token), None).await;
        let p = verify_permission(&st.key.public_b64(), p["permission"].as_str().unwrap()).unwrap();
        assert_eq!(p.grants, vec![allow(Scope::Team, Action::See)], "Hid's refusal would name New");

        // p9 is made in Shops: whoever sees it, by any rule.
        step("PUT", "/profiles/p9", Some(profile_json("Nine", "Shops", b"x"))).await;
        assert_eq!(told(&mut watchers), ["hid", "sam", "pia", "kit"]);
        let (_, p) = answer(&st, ask("GET", "/permission", &kit_token), None).await;
        let p = verify_permission(&st.key.public_b64(), p["permission"].as_str().unwrap()).unwrap();
        assert!(p.grants.contains(&edit_refused));

        // Moved into New: Vera sees it now, Hid no longer; Shops is out of
        // Pia's and Kit's sight, and New comes into Pia's. Sam sees all of it still.
        step("PUT", "/profiles/p9", Some(based_on(profile_json("Nine", "New", b"x"), 1))).await;
        assert_eq!(told(&mut watchers), ["vera", "hid", "pia", "kit"]);

        // Into the trash and back: it stays in sight the whole time, so nobody.
        step("DELETE", "/profiles/p9", None).await;
        assert!(told(&mut watchers).is_empty());
        step("POST", "/trash/p9/restore", None).await;
        assert!(told(&mut watchers).is_empty());
        step("DELETE", "/profiles/p9", None).await;
        assert!(told(&mut watchers).is_empty());
        // Purged, it is gone for whoever saw it.
        step("DELETE", "/trash/p9", None).await;
        assert_eq!(told(&mut watchers), ["vera", "sam", "pia", "kit"]);

        // A proxy, into the team and out of it.
        step("PUT", "/proxies/x9", Some(proxy_json("DC"))).await;
        assert_eq!(told(&mut watchers), ["hid", "sam", "pia"]);
        step("POST", "/proxies/x9/to-me", None).await;
        assert_eq!(told(&mut watchers), ["hid", "sam", "pia"]);

        // Renamed and deleted: Vera's permission names it and Sam sees it;
        // Hid's rule moves with it, but it was hidden from her throughout.
        step("PATCH", "/admin/folders/New", Some(json!({ "name": "Newer" }))).await;
        assert_eq!(told(&mut watchers), ["vera", "sam"]);
        assert!(st.store.grants_of(&hid).unwrap().contains(&Grant { scope: folder("Newer"), action: Action::See, allow: false }));
        step("DELETE", "/admin/folders/Newer", None).await;
        assert_eq!(told(&mut watchers), ["vera", "sam"]);
    }

    // ---- the Team page: last seen, whole tables, re-issued tokens ----

    /// One member's row in `GET /admin/members`.
    fn row_of(rows: &Value, id: &str) -> Value {
        rows.as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap_or_else(|| panic!("no {id} in {rows}"))
    }

    #[tokio::test]
    async fn a_member_reads_as_invited_until_their_invite_is_used() {
        let st = state();
        let (vera_id, vera) = member(&st, "Vera", vec![allow(Scope::Team, Action::See)]).await;
        let a = admin(&st);
        let seen = |rows: &Value| row_of(rows, &vera_id)["last_seen_at"].clone();

        let (code, rows) = answer(&st, ask("GET", "/admin/members", &a), None).await;
        assert_eq!(code, StatusCode::OK);
        assert!(seen(&rows).is_null(), "{rows}");
        // The admin's own requests and a stranger's are nobody's.
        assert_eq!(answer(&st, ask("GET", "/me", &a), None).await.0, StatusCode::OK);
        assert_eq!(answer(&st, ask("GET", "/me", "stm_stranger"), None).await.0, StatusCode::UNAUTHORIZED);
        assert!(seen(&answer(&st, ask("GET", "/admin/members", &a), None).await.1).is_null());

        // Joining is her first request.
        let before = auth::unix_now();
        assert_eq!(answer(&st, ask("GET", "/me", &vera), None).await.0, StatusCode::OK);
        let at = seen(&answer(&st, ask("GET", "/admin/members", &a), None).await.1).as_u64().expect("seen");
        assert!((before..=auth::unix_now()).contains(&at), "{at}");
        // Switching her off answers her row as it now stands.
        let (code, row) = answer(&st, ask("PATCH", &format!("/admin/members/{vera_id}"), &a), Some(json!({ "active": false }))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!((row["id"].as_str(), row["active"].as_bool(), row["last_seen_at"].as_u64()), (Some(vera_id.as_str()), Some(false), Some(at)));
        assert!(row["epoch"].as_u64().unwrap() > 1 && row["name"] == "Vera", "{row}");
    }

    #[tokio::test]
    async fn the_admin_reads_a_members_whole_table_and_the_list_carries_each() {
        let st = state();
        let table = vec![
            allow(folder("Ads"), Action::Start),
            allow(Scope::Team, Action::See),
            // About a profile that is not there: no permission carries it, the table does.
            Grant { scope: Scope::Profile { id: "p-gone".into() }, action: Action::See, allow: false },
        ];
        let (vera_id, vera) = member(&st, "Vera", table.clone()).await;
        let (sam_id, _) = member(&st, "Sam", vec![]).await;
        let a = admin(&st);
        let mut sorted = table;
        view::sort_rules(&mut sorted);
        let want = serde_json::to_value(&sorted).unwrap();
        let uri = format!("/admin/members/{vera_id}/grants");

        let (code, got) = answer(&st, ask("GET", &uri, &a), None).await;
        assert_eq!((code, &got), (StatusCode::OK, &want));
        assert_eq!(got[0], json!({ "scope": { "kind": "team" }, "action": "see", "allow": true }));
        let (_, rows) = answer(&st, ask("GET", "/admin/members", &a), None).await;
        assert_eq!((&row_of(&rows, &vera_id)["grants"], &row_of(&rows, &sam_id)["grants"]), (&want, &json!([])));

        assert_eq!(answer(&st, ask("GET", &uri, &vera), None).await.0, StatusCode::UNAUTHORIZED);
        for nobody in ["nobody", ADMIN_ID] {
            let (code, e) = answer(&st, ask("GET", &format!("/admin/members/{nobody}/grants"), &a), None).await;
            assert_eq!((code, e["error"].as_str()), (StatusCode::NOT_FOUND, Some("no such thing")), "{nobody}");
        }

        // A rewrite answers the table as kept: one rule once, a refusal over an allow.
        let see = |allow: bool| json!({ "scope": { "kind": "team" }, "action": "see", "allow": allow });
        let start = json!({ "scope": { "kind": "team" }, "action": "start", "allow": true });
        let body = json!({ "grants": [start, see(true), see(false), start] });
        let (code, ack) = answer(&st, ask("PUT", &uri, &a), Some(body)).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!((ack["id"].as_str(), &ack["grants"]), (Some(vera_id.as_str()), &json!([see(false), start])));
        assert_eq!(answer(&st, ask("GET", &uri, &a), None).await.1, ack["grants"]);
        let epoch = st.store.member(&vera_id).unwrap().unwrap().epoch;
        assert_eq!(ack["epoch"].as_u64(), Some(epoch));
    }

    #[tokio::test]
    async fn a_reissued_token_stops_at_once_and_is_told_apart_from_a_removal() {
        let st = state();
        let (vera_id, old) = member(&st, "Vera", vec![allow(Scope::Team, Action::See)]).await;
        let a = admin(&st);
        assert_eq!(answer(&st, ask("GET", "/me", &old), None).await.0, StatusCode::OK);
        let mut live = st.hub.subscribe(&vera_id).await;
        let uri = format!("/admin/members/{vera_id}/token");

        assert_eq!(answer(&st, ask("POST", &uri, &old), None).await.0, StatusCode::UNAUTHORIZED);
        let (code, made) = answer(&st, ask("POST", &uri, &a), None).await;
        assert_eq!(code, StatusCode::OK);
        let new = made["token"].as_str().unwrap().to_owned();
        assert!(new.starts_with("stm_") && new != old);
        assert_eq!((made["id"].as_str(), made["name"].as_str(), made["active"].as_bool()), (Some(vera_id.as_str()), Some("Vera"), Some(true)));
        assert!(made["last_seen_at"].is_null(), "invited again until the new one is used: {made}");

        // Her launcher hears it as a kind of its own, not as being removed.
        assert_eq!(heard(&mut live), vec![Event::TokenReplaced]);

        // The old token is refused at once, and says why.
        for path in ["/me", "/permission", "/profiles"] {
            let (code, e) = answer(&st, ask("GET", path, &old), None).await;
            assert_eq!((code, e["code"].as_str()), (StatusCode::UNAUTHORIZED, Some(code::TOKEN_REPLACED)), "{path}");
            assert!(e["error"].as_str().unwrap().contains("does not know this token"), "{e}");
        }
        // One never issued is still just unknown.
        assert_eq!(answer(&st, ask("GET", "/me", "stm_never"), None).await.1["code"], code::UNAUTHORIZED);

        // The new one is the same member, with the same rules.
        let (code, me) = answer(&st, ask("GET", "/me", &new), None).await;
        assert_eq!((code, me["member"]["id"].as_str()), (StatusCode::OK, Some(vera_id.as_str())));
        assert_eq!(st.store.grants_of(&vera_id).unwrap(), vec![allow(Scope::Team, Action::See)]);
        assert!(answer(&st, ask("GET", "/admin/members", &a), None).await.1[0]["last_seen_at"].is_u64());

        for nobody in ["nobody", ADMIN_ID] {
            assert_eq!(answer(&st, ask("POST", &format!("/admin/members/{nobody}/token"), &a), None).await.0, StatusCode::NOT_FOUND);
        }

        // Removed later: both tokens read as a removal.
        assert_eq!(answer(&st, ask("DELETE", &format!("/admin/members/{vera_id}"), &a), None).await.0, StatusCode::NO_CONTENT);
        for t in [&old, &new] {
            let (code, e) = answer(&st, ask("GET", "/me", t), None).await;
            assert_eq!((code, e["code"].as_str()), (StatusCode::UNAUTHORIZED, Some(code::UNAUTHORIZED)));
        }
    }

    #[tokio::test]
    async fn a_reissue_closes_the_members_live_sockets_after_saying_why() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let st = state();
        let (vera_id, old) = member(&st, "Vera", vec![]).await;
        let (_, sam) = member(&st, "Sam", vec![]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut hers = live_socket(addr, &old).await;
        let mut his = live_socket(addr, &sam).await;

        let (code, _) = answer(&st, ask("POST", &format!("/admin/members/{vera_id}/token"), &admin(&st)), None).await;
        assert_eq!(code, StatusCode::OK);

        // Said, then closed.
        let mut got = Vec::new();
        let read = async {
            let mut b = [0u8; 256];
            while let Ok(n @ 1..) = hers.read(&mut b).await {
                got.extend_from_slice(&b[..n]);
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), read).await.expect("her socket stayed open");
        let text = String::from_utf8_lossy(&got);
        assert!(text.contains(r#"{"kind":"token-replaced"}"#), "{text}");
        // Sam's stays.
        let mut b = [0u8; 16];
        assert!(tokio::time::timeout(std::time::Duration::from_millis(200), his.read(&mut b)).await.is_err());

        // And the old token opens no new socket.
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "GET /live HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
             Authorization: Bearer {old}\r\nConnection: close\r\n\r\n"
        );
        s.write_all(head.as_bytes()).await.unwrap();
        let mut reply = String::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_string(&mut reply)).await;
        assert!(reply.starts_with("HTTP/1.1 401") && reply.contains("token_replaced"), "{reply}");
    }
}
