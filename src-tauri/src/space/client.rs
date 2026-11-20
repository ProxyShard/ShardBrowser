//! The one way to a team server. Every request leaves through
//! [`TeamClient::exchange`], and every byte, the live socket's frames too,
//! rides a connection made by [`link`]: plain http only to ask for the switch,
//! then TLS held to the team's key ([`team_wire`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, UPGRADE};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub use hyper::Method;

/// How long an ordinary call may take, answer included.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the switch to TLS may take once connected, handshake included.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Under the server's 10 s for an idle connection, so a kept one is not used as it closes.
const POOL_IDLE: Duration = Duration::from_secs(8);
/// How long the live socket may take to open.
const LIVE_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The server sends small JSON events and nothing else.
const LIVE_MAX_IN: usize = 64 * 1024;
/// A refusal is a line of JSON; a page longer than this is not one.
const REFUSAL_MAX: usize = 64 * 1024;

/// A team server, as one member on this device reaches it.
pub struct TeamClient {
    /// None while joining, when the team is not known yet.
    team_id: Option<String>,
    base: String,
    token: String,
    device_id: String,
    device_name: String,
    /// A 401 or switched_off is handed to the live channel to confirm.
    report_lost: bool,
    /// Sync's own client: a key learned is kept for its link.
    for_sync: bool,
    link: Link,
    /// Set before the token goes out: at once for a key held, by a vouch for one learned.
    trust: tokio::sync::OnceCell<()>,
}

impl TeamClient {
    /// A client for a server and token not saved yet: joining. The server's
    /// key is learned, and proven against the token before the token is sent.
    pub fn new(base: &str, token: &str, device_id: &str) -> Result<Self, TeamError> {
        Ok(TeamClient::reaching(base, token, device_id, None))
    }

    fn reaching(base: &str, token: &str, device_id: &str, key: Option<team_wire::Key>) -> Self {
        let base = base.trim_end_matches('/').to_string();
        let trust = tokio::sync::OnceCell::new_with(key.map(|_| ()));
        TeamClient {
            team_id: None,
            link: link(&base, key),
            base,
            token: token.to_string(),
            device_id: device_id.to_string(),
            device_name: crate::team::device_name(),
            report_lost: false,
            for_sync: false,
            trust,
        }
    }

    /// The client for a team this launcher has joined, held to the key kept for it.
    pub fn for_team(team_id: &str) -> anyhow::Result<Self> {
        let reach = crate::team::reach(team_id)?;
        let mut c = TeamClient::reaching(&reach.server, &reach.token, &reach.device_id, team_wire::key_from_text(&reach.key));
        c.team_id = Some(team_id.to_string());
        // A reaper leaves a lost token to the launcher: it writes nothing of the team's.
        c.report_lost = !super::reap::in_reaper();
        Ok(c)
    }

    /// The client for the server holding the person's own space for sync. A
    /// lost token is told as sync's error, never as a team's withdrawal.
    pub fn for_sync() -> anyhow::Result<Self> {
        let reach = crate::team::sync_reach()?;
        let place = crate::team::sync_place().ok_or_else(|| anyhow::anyhow!(super::sync::NOT_CONNECTED))?;
        let mut c = TeamClient::reaching(&reach.server, &reach.token, &reach.device_id, team_wire::key_from_text(&reach.key));
        c.team_id = Some(place.team_id);
        c.for_sync = true;
        Ok(c)
    }

    /// A client held to `key` from the start, for a test that speaks for another device.
    #[cfg(test)]
    pub fn held_to(base: &str, token: &str, device_id: &str, key: &str) -> Self {
        TeamClient::reaching(base, token, device_id, Some(team_wire::key_from_text(key).expect("a team key")))
    }

    /// Lost access is the caller's to confirm: the live channel's own client.
    pub fn quiet(mut self) -> Self {
        self.report_lost = false;
        self
    }

    /// The team this client was made for; None while joining.
    pub fn team(&self) -> Option<&str> {
        self.team_id.as_deref()
    }

    /// The key the server proved itself with, as a launcher keeps it; None before the first answer.
    pub fn server_key(&self) -> Option<String> {
        self.trust.initialized().then(|| self.link.pin.key().map(|k| team_wire::key_text(&k))).flatten()
    }

    /// Still the address and token saved for its team: not left, nor joined again.
    pub fn is_current(&self) -> bool {
        let Some(team) = &self.team_id else { return false };
        crate::team::reach(team).is_ok_and(|r| self.made_with(&r.server, &r.token))
    }

    /// Whether this client was made with that address and token.
    pub fn made_with(&self, server: &str, token: &str) -> bool {
        self.token == token && self.base == server.trim_end_matches('/')
    }

    /// Whether the server at this address is still this client's team, as
    /// `/health` names it: one that came up empty there is another team.
    pub async fn same_team(&self) -> bool {
        #[derive(Deserialize)]
        struct Health {
            #[serde(default)]
            team: String,
        }
        match (&self.team_id, self.get::<Health>("/health").await) {
            (Some(ours), Ok(h)) => h.team == *ours,
            _ => false,
        }
    }

    /// Every byte the launcher sends a team server goes through here.
    async fn exchange(&self, method: Method, path: &str, headers: HeaderMap, body: Payload) -> Result<Response, TeamError> {
        self.trusted().await?;
        let mut req = hyper::Request::builder().method(method).uri(format!("{}{}", self.base, path));
        let h = req.headers_mut().expect("a request being built");
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            h.insert(AUTHORIZATION, v);
        }
        if let Ok(v) = HeaderValue::from_str(&self.device_id) {
            h.insert("x-device-id", v);
        }
        // Bytes, not to_str: a person names their machine in their own language.
        if let Ok(name) = HeaderValue::from_bytes(self.device_name.as_bytes()) {
            h.insert("x-device-name", name);
        }
        h.extend(headers);
        let body = match body {
            Payload::None => empty(),
            Payload::Json(body) => {
                h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                full(body)
            }
            Payload::File(body, len) => {
                h.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
                h.insert(CONTENT_LENGTH, HeaderValue::from(len));
                body
            }
        };
        let req = req.body(body).map_err(|_| TeamError::Garbled(format!("{} is not a server address", self.base)))?;
        self.send(req).await
    }

    /// A request onto the link as it is, nothing added.
    async fn send(&self, req: hyper::Request<Body>) -> Result<Response, TeamError> {
        self.link.http.request(req).await.map_err(|e| self.failed(&e, e.is_connect()))
    }

    /// Why the link failed: the wire's own refusals as such, the rest in plain words.
    fn failed(&self, e: &(dyn std::error::Error + 'static), connecting: bool) -> TeamError {
        match wire_fault(e) {
            Some(w) => TeamError::Garbled(w.words(&self.base)),
            None => TeamError::Offline(plain(e, connecting)),
        }
    }

    /// Nothing carrying the token goes out before the server is known: held
    /// to a key kept, or one learned that it proved first. A failed vouch is asked again.
    async fn trusted(&self) -> Result<(), TeamError> {
        self.trust.get_or_try_init(|| self.vouch()).await.map(|_| ())
    }

    /// Ask the server to prove it issued this token, bound to the key it showed.
    async fn vouch(&self) -> Result<(), TeamError> {
        let challenge = team_wire::challenge();
        let ask = serde_json::to_vec(&team_wire::Vouch::new(&self.token, &challenge)).unwrap_or_default();
        let req = hyper::Request::builder()
            .method(Method::POST)
            .uri(format!("{}{}", self.base, team_wire::VOUCH))
            .header(CONTENT_TYPE, "application/json")
            .body(full(ask))
            .map_err(|_| TeamError::Garbled(format!("{} is not a server address", self.base)))?;
        let answer = async {
            let resp = self.send(req).await?;
            let status = resp.status().as_u16();
            Ok::<_, TeamError>((status, self.body(resp, REFUSAL_MAX).await?))
        };
        let (status, bytes) = tokio::time::timeout(CALL_TIMEOUT, answer).await.map_err(|_| no_answer())??;
        let unproven = || TeamError::Garbled(format!("{} could not prove it is the server this token is from", self.base));
        if status == 404 {
            let r = Refusal::read(status, &bytes);
            return Err(if r.code == Code::UnknownToken { TeamError::Refused(Box::new(r)) } else { unproven() });
        }
        let key = self.link.pin.key().ok_or_else(unproven)?;
        let vouched: team_wire::Vouched = serde_json::from_slice(&bytes).map_err(|_| unproven())?;
        if status != 200 || !vouched.holds(&self.token, &challenge, &key) {
            return Err(unproven());
        }
        remember(&self.base, key, self.link.clone());
        if self.for_sync {
            crate::team::sync_key_learned(&self.base, &self.token, &team_wire::key_text(&key));
        }
        Ok(())
    }

    /// An answer's body, read whole; past `max` bytes it is not the server's.
    async fn body(&self, resp: Response, max: usize) -> Result<Vec<u8>, TeamError> {
        let limited = http_body_util::Limited::new(resp.into_body(), max);
        match limited.collect().await {
            Ok(b) => Ok(b.to_bytes().to_vec()),
            Err(e) => match e.downcast_ref::<hyper::Error>() {
                Some(h) => Err(TeamError::Offline(plain(h, false))),
                None => Err(TeamError::Garbled(format!("{} sent more than {} KB", self.base, max >> 10))),
            },
        }
    }

    /// One request. A 2xx answer comes back as status and body; anything
    /// else as the server's refusal.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, Vec<u8>), TeamError> {
        let body = body.map_or(Payload::None, |b| Payload::Json(b.to_string().into_bytes()));
        let answer = async {
            let resp = self.exchange(method, path, HeaderMap::new(), body).await?;
            let status = resp.status().as_u16();
            Ok::<_, TeamError>((status, self.body(resp, usize::MAX).await?))
        };
        let (status, bytes) = tokio::time::timeout(CALL_TIMEOUT, answer).await.map_err(|_| no_answer())??;
        if (200..300).contains(&status) {
            return Ok((status, bytes));
        }
        let err = TeamError::Refused(Box::new(Refusal::read(status, &bytes)));
        self.report(&err);
        Err(err)
    }

    /// Any request, any answer, nothing reported: for a test that probes the server.
    #[cfg(test)]
    pub async fn raw(&self, method: Method, path: &str, body: Option<Vec<u8>>) -> Result<(u16, Vec<u8>), TeamError> {
        let body = match body {
            Some(b) => {
                let len = b.len() as u64;
                Payload::File(full(b), len)
            }
            None => Payload::None,
        };
        let resp = self.exchange(method, path, HeaderMap::new(), body).await?;
        let status = resp.status().as_u16();
        Ok((status, self.body(resp, usize::MAX).await?))
    }

    /// A connection of its own inside the TLS, for a test that writes http by hand.
    #[cfg(test)]
    pub async fn wire_stream(&self) -> Result<impl AsyncRead + AsyncWrite + Unpin, TeamError> {
        use tower_service::Service;
        self.trusted().await?;
        let uri: hyper::Uri = self.base.parse().map_err(|_| TeamError::Garbled("not an address".into()))?;
        let mut wire = Wire { pin: self.link.pin.clone() };
        let secure = wire.call(uri).await.map_err(|e| self.failed(&e, true))?;
        Ok(secure.0.into_inner())
    }

    /// A GET whose answer is JSON.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, TeamError> {
        self.json(Method::GET, path, None).await
    }

    /// One request whose answer is JSON.
    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, TeamError> {
        let (_, body) = self.call(method, path, body).await?;
        serde_json::from_slice(&body)
            .map_err(|_| TeamError::Garbled(format!("{} did not answer as a team server", self.base)))
    }

    /// The signed permission, as the server issues it now.
    pub async fn permission(&self) -> Result<Issued, TeamError> {
        self.get("/permission").await
    }

    /// A refusal read out of an answer that is not 2xx.
    async fn refused(&self, resp: Response) -> TeamError {
        let status = resp.status().as_u16();
        let bytes = self.body(resp, REFUSAL_MAX).await.unwrap_or_default();
        let err = TeamError::Refused(Box::new(Refusal::read(status, &bytes)));
        self.report(&err);
        err
    }

    /// A session download: the JSON in front is answered, the bytes after it
    /// go to `to` a piece at a time, so nothing holds the session whole.
    pub async fn download(&self, path: &str, to: &std::path::Path) -> Result<SessionFront, TeamError> {
        let ask = self.exchange(Method::GET, path, HeaderMap::new(), Payload::None);
        let mut resp = tokio::time::timeout(CALL_TIMEOUT, ask)
            .await
            .map_err(|_| TeamError::Offline("the session download did not start".into()))??;
        if !resp.status().is_success() {
            return Err(self.refused(resp).await);
        }
        let garbled = || TeamError::Garbled(format!("{} did not send a session", self.base));
        let local = |e: std::io::Error| TeamError::Garbled(format!("the session could not be written here: {e}"));
        let mut file = tokio::fs::File::create(to).await.map_err(local)?;
        let mut head: Vec<u8> = Vec::new();
        let mut front: Option<SessionFront> = None;
        let mut got = 0u64;
        loop {
            let piece = tokio::time::timeout(BODY_IDLE, next_piece(resp.body_mut()))
                .await
                .map_err(|_| TeamError::Offline("the session download stalled".into()))?
                .map_err(|e| TeamError::Offline(plain(&e, false)))?;
            let Some(piece) = piece else { break };
            let mut rest: &[u8] = &piece;
            if front.is_none() {
                head.extend_from_slice(rest);
                rest = &[];
                if head.len() < 4 {
                    continue;
                }
                let n = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
                if n > FRONT_MAX {
                    return Err(garbled());
                }
                if head.len() < 4 + n {
                    continue;
                }
                front = Some(serde_json::from_slice(&head[4..4 + n]).map_err(|_| garbled())?);
                let tail = head.split_off(4 + n);
                got += tail.len() as u64;
                file.write_all(&tail).await.map_err(local)?;
            }
            got += rest.len() as u64;
            file.write_all(rest).await.map_err(local)?;
        }
        let front = front.ok_or_else(garbled)?;
        if got != front.size {
            return Err(TeamError::Offline(format!("the session broke off at {got} of {} bytes", front.size)));
        }
        file.sync_all().await.map_err(local)?;
        Ok(front)
    }

    /// A blob as it is, into `to`; refused past `max` bytes. Its size.
    pub async fn fetch(&self, path: &str, to: &std::path::Path, max: u64) -> Result<u64, TeamError> {
        let ask = self.exchange(Method::GET, path, HeaderMap::new(), Payload::None);
        let mut resp = tokio::time::timeout(CALL_TIMEOUT, ask)
            .await
            .map_err(|_| TeamError::Offline("the download did not start".into()))??;
        if !resp.status().is_success() {
            return Err(self.refused(resp).await);
        }
        let local = |e: std::io::Error| TeamError::Garbled(format!("the download could not be written here: {e}"));
        let mut file = tokio::fs::File::create(to).await.map_err(local)?;
        let mut got = 0u64;
        loop {
            let piece = tokio::time::timeout(BODY_IDLE, next_piece(resp.body_mut()))
                .await
                .map_err(|_| TeamError::Offline("the download stalled".into()))?
                .map_err(|e| TeamError::Offline(plain(&e, false)))?;
            let Some(piece) = piece else { break };
            got += piece.len() as u64;
            if got > max {
                return Err(TeamError::Garbled(format!("{} sent more than {} MB", self.base, max >> 20)));
            }
            file.write_all(&piece).await.map_err(local)?;
        }
        file.sync_all().await.map_err(local)?;
        Ok(got)
    }

    /// A session upload from the file at `from`, sent as it is read.
    pub async fn upload(&self, path: &str, from: &std::path::Path) -> Result<SessionInfo, TeamError> {
        self.send_file(path, from).await
    }

    /// A PUT of the file at `from`, sent as it is read; the answer is JSON.
    pub async fn send_file<T: DeserializeOwned>(&self, path: &str, from: &std::path::Path) -> Result<T, TeamError> {
        let local = |e: std::io::Error| TeamError::Garbled(format!("the file could not be read here: {e}"));
        let file = tokio::fs::File::open(from).await.map_err(local)?;
        let len = file.metadata().await.map_err(local)?.len();
        // Room for a slow link: a minute, and a minute more for every 8 MB.
        let allow = Duration::from_secs(60 + len / (128 * 1024));
        let send = self.exchange(Method::PUT, path, HeaderMap::new(), Payload::File(file_body(file, len), len));
        let resp = tokio::time::timeout(allow, send)
            .await
            .map_err(|_| TeamError::Offline("the upload took too long".into()))??;
        if !resp.status().is_success() {
            return Err(self.refused(resp).await);
        }
        let bytes = self.body(resp, usize::MAX).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| TeamError::Garbled(format!("{} did not answer as a team server", self.base)))
    }

    /// The live socket. The upgrade is an ordinary request through
    /// [`Self::exchange`], so the socket rides the same link as every call.
    pub async fn live(&self) -> Result<LiveSocket, TeamError> {
        let key = generate_key();
        let mut h = HeaderMap::new();
        h.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        h.insert(UPGRADE, HeaderValue::from_static("websocket"));
        h.insert("sec-websocket-version", HeaderValue::from_static("13"));
        if let Ok(v) = HeaderValue::from_str(&key) {
            h.insert("sec-websocket-key", v);
        }
        let open = async {
            let resp = self.exchange(Method::GET, "/live", h, Payload::None).await?;
            let status = resp.status().as_u16();
            if status != 101 {
                let bytes = self.body(resp, REFUSAL_MAX).await.unwrap_or_default();
                let err = if (200..300).contains(&status) {
                    TeamError::Garbled(format!("{} did not open the live channel", self.base))
                } else {
                    TeamError::Refused(Box::new(Refusal::read(status, &bytes)))
                };
                self.report(&err);
                return Err(err);
            }
            let accept = resp.headers().get("sec-websocket-accept").map(|v| v.as_bytes().to_vec());
            if accept.as_deref() != Some(derive_accept_key(key.as_bytes()).as_bytes()) {
                return Err(TeamError::Garbled(format!("{} answered the live channel wrongly", self.base)));
            }
            let raw = hyper::upgrade::on(resp).await.map_err(|e| TeamError::Offline(plain(&e, false)))?;
            let cfg = WebSocketConfig {
                max_message_size: Some(LIVE_MAX_IN),
                max_frame_size: Some(LIVE_MAX_IN),
                ..Default::default()
            };
            let ws = WebSocketStream::from_raw_socket(TokioIo::new(raw), Role::Client, Some(cfg)).await;
            Ok(LiveSocket { ws })
        };
        tokio::time::timeout(LIVE_OPEN_TIMEOUT, open)
            .await
            .map_err(|_| TeamError::Offline("the live channel took too long to open".into()))?
    }

    fn report(&self, e: &TeamError) {
        if let (true, true, Some(team)) = (self.report_lost, e.access_lost(), &self.team_id) {
            super::live::access_lost(team);
        }
    }
}

fn no_answer() -> TeamError {
    TeamError::Offline("no answer in time".into())
}

/// What a request carries.
enum Payload {
    None,
    Json(Vec<u8>),
    /// A body read as it goes out, and its length.
    File(Body, u64),
}

type Body = UnsyncBoxBody<Bytes, std::io::Error>;
type Response = hyper::Response<Incoming>;

fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}

fn full(bytes: Vec<u8>) -> Body {
    Full::new(Bytes::from(bytes)).map_err(|never| match never {}).boxed_unsync()
}

/// The next piece of an answer's body; None at its end.
async fn next_piece(body: &mut Incoming) -> Result<Option<Bytes>, hyper::Error> {
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            return Ok(Some(data));
        }
    }
    Ok(None)
}

/// A file as a request body, read a piece at a time as it goes out.
fn file_body(file: tokio::fs::File, len: u64) -> Body {
    FileBody { file, left: len, buf: vec![0; 256 * 1024].into_boxed_slice() }.boxed_unsync()
}

struct FileBody {
    file: tokio::fs::File,
    left: u64,
    buf: Box<[u8]>,
}

impl http_body::Body for FileBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;
        let this = self.get_mut();
        if this.left == 0 {
            return Poll::Ready(None);
        }
        let want = this.buf.len().min(usize::try_from(this.left).unwrap_or(usize::MAX));
        let mut rb = tokio::io::ReadBuf::new(&mut this.buf[..want]);
        if let Err(e) = std::task::ready!(tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut this.file), cx, &mut rb)) {
            return Poll::Ready(Some(Err(e)));
        }
        let n = rb.filled().len();
        if n == 0 {
            return Poll::Ready(Some(Err(std::io::ErrorKind::UnexpectedEof.into())));
        }
        this.left -= n as u64;
        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::copy_from_slice(rb.filled())))))
    }

    fn is_end_stream(&self) -> bool {
        self.left == 0
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.left)
    }
}

/// The JSON in front of a session's bytes. No `Debug`: it carries the proxy's login.
#[derive(Deserialize)]
pub struct SessionFront {
    /// 0 before the first upload.
    pub session_rev: u64,
    #[serde(default)]
    pub size: u64,
    /// The proxy the profile is bound to; set while `proxy` is null means it is not there.
    #[serde(default)]
    pub proxy_id: Option<String>,
    #[serde(default)]
    pub proxy: Option<serde_json::Value>,
}

/// What an upload answers.
#[derive(Debug, Deserialize)]
pub struct SessionInfo {
    pub session_rev: u64,
}

/// The JSON in front of a session is small; anything longer is not a session.
const FRONT_MAX: usize = 64 * 1024;
/// How long a session download may stall between pieces.
const BODY_IDLE: Duration = Duration::from_secs(60);

/// The connections to one server, every one held to one key.
#[derive(Clone)]
struct Link {
    http: Client<Wire, Body>,
    pin: Arc<team_wire::Pin>,
}

/// Links by address and key, kept so a call need not open and switch a connection again.
fn links() -> &'static Mutex<HashMap<(String, team_wire::Key), Link>> {
    static LINKS: OnceLock<Mutex<HashMap<(String, team_wire::Key), Link>>> = OnceLock::new();
    LINKS.get_or_init(Default::default)
}

/// Where every team connection comes from. Redirects are not followed: nothing
/// meant for the team goes to another address. No key yet: a link of its own.
fn link(base: &str, key: Option<team_wire::Key>) -> Link {
    if let Some(k) = key {
        if let Some(l) = links().lock().unwrap_or_else(|e| e.into_inner()).get(&(base.to_owned(), k)) {
            return l.clone();
        }
    }
    let pin = key.map_or_else(team_wire::Pin::learn, team_wire::Pin::to);
    let http = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(POOL_IDLE)
        .pool_timer(TokioTimer::new())
        .build(Wire { pin: pin.clone() });
    let l = Link { http, pin };
    if let Some(k) = key {
        remember(base, k, l.clone());
    }
    l
}

/// A link whose key is known now, kept for the clients after this one.
fn remember(base: &str, key: team_wire::Key, l: Link) {
    links().lock().unwrap_or_else(|e| e.into_inner()).entry((base.to_owned(), key)).or_insert(l);
}

/// Opens a connection the way [`team_wire`] says: connect, ask to switch, TLS.
#[derive(Clone)]
struct Wire {
    pin: Arc<team_wire::Pin>,
}

impl tower_service::Service<hyper::Uri> for Wire {
    type Response = Secure;
    type Error = std::io::Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Secure>> + Send>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: hyper::Uri) -> Self::Future {
        Box::pin(open(uri, self.pin.clone()))
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn open(uri: hyper::Uri, pin: Arc<team_wire::Pin>) -> std::io::Result<Secure> {
    use std::io::{Error, ErrorKind};
    let https = uri.scheme_str() == Some("https");
    let authority = uri.authority().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no host"))?.to_string();
    let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_owned();
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| Error::from(ErrorKind::TimedOut))??;
    let _ = tcp.set_nodelay(true);
    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("team.invalid").expect("a name"));
    let switched = async {
        // https is somebody's proxy in front of the server: through its TLS first.
        let mut io: Box<dyn Io> = if https {
            let outer = tokio_rustls::TlsConnector::from(outer_tls()).connect(name.clone(), tcp).await?;
            Box::new(outer)
        } else {
            Box::new(tcp)
        };
        switch(&mut io, &authority).await?;
        let tls = tokio_rustls::TlsConnector::from(team_wire::client_config(pin.clone()));
        tls.connect(name, io).await.map_err(|e| if pin.saw_another() { Error::other(WireFault::OtherKey) } else { e })
    };
    let tls = tokio::time::timeout(SWITCH_TIMEOUT, switched).await.map_err(|_| Error::from(ErrorKind::TimedOut))??;
    Ok(Secure(TokioIo::new(tls)))
}

/// Ask for the switch and read the answer's head; nothing after it is ours to read.
async fn switch(io: &mut Box<dyn Io>, authority: &str) -> std::io::Result<()> {
    let ask = format!(
        "GET {} HTTP/1.1\r\nHost: {authority}\r\nConnection: Upgrade\r\nUpgrade: {}\r\n\r\n",
        team_wire::PATH,
        team_wire::UPGRADE
    );
    io.write_all(ask.as_bytes()).await?;
    let mut head = Vec::with_capacity(256);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(std::io::Error::other(WireFault::NoSwitch));
        }
        let mut b = [0u8; 1];
        if io.read(&mut b).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.push(b[0]);
    }
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut res = httparse::Response::new(&mut headers);
    let _ = res.parse(&head);
    let upgrade = res.headers.iter().find(|h| h.name.eq_ignore_ascii_case("upgrade")).map(|h| h.value);
    match res.code {
        Some(101) if upgrade.is_some_and(|v| v.eq_ignore_ascii_case(team_wire::UPGRADE.as_bytes())) => Ok(()),
        Some(426) => Err(std::io::Error::other(WireFault::Newer)),
        _ => Err(std::io::Error::other(WireFault::NoSwitch)),
    }
}

/// The TLS of a proxy in front of the server, checked against the public roots.
fn outer_tls() -> Arc<rustls::ClientConfig> {
    static OUTER: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    OUTER
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring speaks TLS")
                .with_root_certificates(roots)
                .with_no_client_auth();
            cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(cfg)
        })
        .clone()
}

/// A connection after the switch, as the pool holds it.
struct Secure(TokioIo<tokio_rustls::client::TlsStream<Box<dyn Io>>>);

impl Connection for Secure {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl hyper::rt::Read for Secure {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for Secure {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// What the wire itself refused, as opposed to a link that broke.
#[derive(Debug)]
enum WireFault {
    /// Answered the switch with something else: not a team server, or one older than this launcher.
    NoSwitch,
    /// Asks for a newer switch than this launcher speaks.
    Newer,
    /// Showed another key than the team's.
    OtherKey,
}

impl WireFault {
    fn words(&self, base: &str) -> String {
        match self {
            WireFault::NoSwitch => format!("{base} does not answer as an encrypted team server: it is not one, or it is older than this launcher"),
            WireFault::Newer => format!("{base} is a newer team server than this launcher: update ShardBrowser"),
            WireFault::OtherKey => format!("{base} is not this team's server: it shows another key. If the server was set up anew, paste a new invite"),
        }
    }
}

impl std::fmt::Display for WireFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words("the server"))
    }
}

impl std::error::Error for WireFault {}

/// The wire's refusal somewhere in an error's causes; an io error holds it inside, not as a source.
fn wire_fault<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a WireFault> {
    let mut src = Some(e);
    while let Some(inner) = src {
        if let Some(w) = inner.downcast_ref::<WireFault>() {
            return Some(w);
        }
        let held = inner.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref());
        if let Some(w) = held.and_then(|h| h.downcast_ref::<WireFault>()) {
            return Some(w);
        }
        src = inner.source();
    }
    None
}

/// What went wrong on the link, in words a person reads; the causes go to the log.
fn plain(e: &(dyn std::error::Error + 'static), connecting: bool) -> String {
    use std::io::ErrorKind;
    let mut chain = e.to_string();
    let mut io = None;
    let mut src = Some(e);
    while let Some(inner) = src {
        io = io.or_else(|| inner.downcast_ref::<std::io::Error>().map(|e| e.kind()));
        src = inner.source();
        if let Some(s) = src {
            chain = format!("{chain}: {s}");
        }
    }
    eprintln!("[launcher] team server link: {chain}");
    let words = if io == Some(ErrorKind::TimedOut) {
        "no answer in time"
    } else if connecting && io == Some(ErrorKind::ConnectionRefused) {
        "nothing is listening at its address"
    } else if connecting {
        "this computer could not connect to it"
    } else {
        "the connection broke off"
    };
    words.to_string()
}

/// What `GET /permission` answers.
#[derive(Deserialize)]
pub struct Issued {
    /// The signed document; `perm::accept` checks it before anything reads it.
    pub permission: String,
}

/// Why a call to a team server did not do what it asked.
#[derive(Debug)]
pub enum TeamError {
    /// No answer: the server is down, or the link to it.
    Offline(String),
    /// The server answered no.
    Refused(Box<Refusal>),
    /// An answer that is not a team server's.
    Garbled(String),
}

/// The server's stable codes; the launcher acts on these, not on the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    Unauthorized,
    /// The admin re-issued the token: still a member, under a new invite.
    TokenReplaced,
    SwitchedOff,
    Forbidden,
    NotFound,
    BadRequest,
    Stale,
    Leased,
    NoLease,
    Conflict,
    BadProxy,
    InTrash,
    TooLarge,
    /// The server never issued the token, or the member is gone; said before the server is trusted.
    UnknownToken,
    Other(String),
}

impl Code {
    fn parse(s: &str) -> Code {
        match s {
            "unauthorized" => Code::Unauthorized,
            "token_replaced" => Code::TokenReplaced,
            "switched_off" => Code::SwitchedOff,
            "forbidden" => Code::Forbidden,
            "not_found" => Code::NotFound,
            "bad_request" => Code::BadRequest,
            "stale" => Code::Stale,
            "leased" => Code::Leased,
            "no_lease" => Code::NoLease,
            "conflict" => Code::Conflict,
            "bad_proxy" => Code::BadProxy,
            "in_trash" => Code::InTrash,
            "too_large" => Code::TooLarge,
            team_wire::UNKNOWN_TOKEN => Code::UnknownToken,
            other => Code::Other(other.to_string()),
        }
    }

    /// As the server spells it.
    pub fn as_str(&self) -> &str {
        match self {
            Code::Unauthorized => "unauthorized",
            Code::TokenReplaced => "token_replaced",
            Code::SwitchedOff => "switched_off",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not_found",
            Code::BadRequest => "bad_request",
            Code::Stale => "stale",
            Code::Leased => "leased",
            Code::NoLease => "no_lease",
            Code::Conflict => "conflict",
            Code::BadProxy => "bad_proxy",
            Code::InTrash => "in_trash",
            Code::TooLarge => "too_large",
            Code::UnknownToken => team_wire::UNKNOWN_TOKEN,
            Code::Other(s) if s.is_empty() => "refused",
            Code::Other(s) => s,
        }
    }
}

/// A refusal as the server sent it: `{error, code}` and what is in the way.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub status: u16,
    pub code: Code,
    pub message: String,
    pub current_revision: Option<u64>,
    pub current_session_rev: Option<u64>,
    pub current_package_rev: Option<u64>,
    /// Who has the profile open, for `leased`.
    pub lease: Option<LeaseHolder>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseHolder {
    #[serde(default)]
    pub device_name: String,
}

#[derive(Default, Deserialize)]
struct WireRefusal {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: String,
    current_revision: Option<u64>,
    current_session_rev: Option<u64>,
    current_package_rev: Option<u64>,
    lease: Option<LeaseHolder>,
}

impl Refusal {
    /// A body that is not the server's JSON (a proxy's page) keeps only the status.
    fn read(status: u16, body: &[u8]) -> Refusal {
        let w: WireRefusal = serde_json::from_slice(body).unwrap_or_default();
        Refusal {
            status,
            code: Code::parse(&w.code),
            message: w.error,
            current_revision: w.current_revision,
            current_session_rev: w.current_session_rev,
            current_package_rev: w.current_package_rev,
            lease: w.lease,
        }
    }
}

impl TeamError {
    pub fn code(&self) -> Option<&Code> {
        match self {
            TeamError::Refused(r) => Some(&r.code),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            TeamError::Refused(r) => Some(r.status),
            _ => None,
        }
    }

    /// The token stopped working: 401, 403 switched_off, or 401 token_replaced.
    /// Acted on only once a second `GET /permission` says the same.
    pub fn access_lost(&self) -> bool {
        match self {
            TeamError::Refused(r) => {
                (r.status == 401 && r.code == Code::Unauthorized)
                    || (r.status == 403 && r.code == Code::SwitchedOff)
                    || self.token_replaced()
            }
            _ => false,
        }
    }

    /// The token was re-issued: not a removal, the member needs the new invite.
    pub fn token_replaced(&self) -> bool {
        matches!(self, TeamError::Refused(r) if r.status == 401 && r.code == Code::TokenReplaced)
    }
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamError::Offline(why) => write!(f, "the team server did not answer: {why}"),
            TeamError::Garbled(why) => f.write_str(why),
            TeamError::Refused(r) => {
                if let Some(l) = r.lease.as_ref().filter(|l| !l.device_name.is_empty()) {
                    return write!(f, "open on {}", l.device_name);
                }
                if r.message.is_empty() {
                    write!(f, "the team server answered {}", r.status)?;
                } else {
                    f.write_str(&r.message)?;
                }
                let now = r.current_revision.or(r.current_session_rev).or(r.current_package_rev);
                match now {
                    Some(n) => write!(f, " (now at revision {n})"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for TeamError {}

/// What the server says down the live socket.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LiveEvent {
    /// The member's rules changed; `epoch` is the edition to hold now.
    GrantsChanged { epoch: u64 },
    /// The member was switched off or removed.
    Revoked,
    /// The admin re-issued the token: this one stops working now.
    TokenReplaced,
    /// News about the team's own data; `what` is "team" for its name.
    TeamChanged { what: String },
    /// News about the member's own space, which sync follows.
    PersonalChanged { what: String },
    #[serde(other)]
    Other,
}

/// What one read of the live socket brought.
pub enum Heard {
    Event(LiveEvent),
    /// Something that only says the link is up: a pong, an unknown frame.
    Alive,
    Closed,
}

/// The open live channel to one team.
pub struct LiveSocket {
    ws: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
}

impl LiveSocket {
    /// The next thing the server says. Safe to drop mid-wait.
    pub async fn hear(&mut self) -> Heard {
        match self.ws.next().await {
            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => Heard::Closed,
            Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
                Ok(e) => Heard::Event(e),
                Err(_) => Heard::Alive,
            },
            Some(Ok(_)) => Heard::Alive,
        }
    }

    /// False once the socket cannot be written to.
    pub async fn ping(&mut self) -> bool {
        self.ws.send(Message::Ping(Vec::new())).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link that fails reads in plain words, without the transport's own.
    #[tokio::test]
    async fn a_dead_link_reads_in_plain_words() {
        let _root = crate::store::test_root("plain-words");
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let c = TeamClient::new(&at, "t", "d").unwrap();
        let e = c.call(Method::GET, "/health", None).await.unwrap_err();
        assert_eq!(e.to_string(), "the team server did not answer: nothing is listening at its address");
    }

    #[test]
    fn a_refusal_reads_the_servers_code_and_what_is_in_the_way() {
        let r = Refusal::read(409, br#"{"error":"changed since you read it","code":"stale","current_revision":7}"#);
        assert_eq!((r.status, &r.code, r.current_revision), (409, &Code::Stale, Some(7)));
        let e = TeamError::Refused(Box::new(r));
        assert_eq!(e.to_string(), "changed since you read it (now at revision 7)");
        assert!(!e.access_lost());

        let r = Refusal::read(
            409,
            br#"{"error":"open on another device","code":"leased","lease":{"member_id":"m","device_id":"d","device_name":"Vera's Mac","acquired_at":1,"expires_at":2}}"#,
        );
        assert_eq!(TeamError::Refused(Box::new(r)).to_string(), "open on Vera's Mac");

        // A proxy's page in place of the server's JSON keeps only the status.
        let r = Refusal::read(502, b"<html>Bad Gateway</html>");
        assert_eq!(r.code, Code::Other(String::new()));
        assert_eq!(TeamError::Refused(Box::new(r)).to_string(), "the team server answered 502");
    }

    #[test]
    fn only_a_dead_token_or_a_switch_off_reads_as_lost_access() {
        let lost = |status, code: &str| {
            let body = format!(r#"{{"error":"x","code":"{code}"}}"#);
            TeamError::Refused(Box::new(Refusal::read(status, body.as_bytes()))).access_lost()
        };
        assert!(lost(401, "unauthorized"));
        assert!(lost(403, "switched_off"));
        assert!(!lost(403, "forbidden"));
        assert!(!lost(404, "not_found"));
        assert!(!lost(401, "something_else"));
        // Re-issued: lost too, and told apart from a removal.
        assert!(lost(401, "token_replaced"));
        let replaced = |status, code: &str| {
            let body = format!(r#"{{"error":"x","code":"{code}"}}"#);
            TeamError::Refused(Box::new(Refusal::read(status, body.as_bytes()))).token_replaced()
        };
        assert!(replaced(401, "token_replaced"));
        assert!(!replaced(401, "unauthorized"));
        assert!(!replaced(403, "token_replaced"));
        assert!(!TeamError::Offline("down".into()).access_lost());
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_to_another_address() {
        let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let there = elsewhere.local_addr().unwrap();
        let (tls, key) = some_key();
        let moved = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{there}/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let at = fake_server(tls, move |_| moved.clone()).await;
        let c = TeamClient::reaching(&at, "stm_x", "device", Some(key));
        let body = serde_json::json!({ "name": "x" });
        for (method, body) in [(Method::PUT, Some(&body)), (Method::GET, None)] {
            let err = c.call(method, "/profiles/p1", body).await.unwrap_err();
            assert_eq!(err.status(), Some(307));
        }
        let went = tokio::time::timeout(Duration::from_millis(300), elsewhere.accept()).await;
        assert!(went.is_err(), "a request went on to another address");
    }

    // ---- the wire ----

    use shardbrowser_team_server::{Action, Grant, Scope, TestServer};
    use tokio::net::{TcpListener, TcpStream};

    fn some_key() -> (Arc<rustls::ServerConfig>, team_wire::Key) {
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        team_wire::server_config(pkcs8.as_ref()).unwrap()
    }

    fn every_right() -> Vec<Grant> {
        [Action::See, Action::Start, Action::Edit, Action::Create, Action::Delete, Action::Cookies, Action::TakeOut]
            .map(|action| Grant { scope: Scope::Team, action, allow: true })
            .to_vec()
    }

    fn holds(seen: &[u8], what: &[u8]) -> bool {
        seen.windows(what.len()).any(|w| w == what)
    }

    /// Read an http head by hand, a byte at a time.
    async fn head_of(io: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            if io.read(&mut b).await? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            head.push(b[0]);
        }
        Ok(head)
    }

    /// The front of a team server by hand: the switch, then TLS with `tls`.
    async fn accept_wire(mut tcp: TcpStream, tls: Arc<rustls::ServerConfig>) -> std::io::Result<tokio_rustls::server::TlsStream<TcpStream>> {
        head_of(&mut tcp).await?;
        let ok = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: {}\r\n\r\n", team_wire::UPGRADE);
        tcp.write_all(ok.as_bytes()).await?;
        tokio_rustls::TlsAcceptor::from(tls).accept(tcp).await
    }

    /// A server that switches and shakes hands with `tls`, then answers each request's head with `answer`.
    async fn fake_server(tls: Arc<rustls::ServerConfig>, answer: impl Fn(&[u8]) -> String + Send + Sync + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let answer = Arc::new(answer);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = l.accept().await {
                let (tls, answer) = (tls.clone(), answer.clone());
                tokio::spawn(async move {
                    let Ok(mut io) = accept_wire(tcp, tls).await else { return };
                    if let Ok(head) = head_of(&mut io).await {
                        let _ = io.write_all(answer(&head).as_bytes()).await;
                        let _ = io.shutdown().await;
                    }
                });
            }
        });
        at
    }

    /// Carry bytes one way, keeping a copy of each.
    async fn carry(mut from: impl AsyncRead + Unpin, mut to: impl AsyncWrite + Unpin, seen: Arc<Mutex<Vec<u8>>>) {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = from.read(&mut buf).await {
            if n == 0 || to.write_all(&buf[..n]).await.is_err() {
                break;
            }
            seen.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        let _ = to.shutdown().await;
    }

    /// A relay to the server at `to` that keeps every byte it carries, both ways.
    async fn sniffer(to: &str) -> (String, Arc<Mutex<Vec<u8>>>) {
        let to: std::net::SocketAddr = to.trim_start_matches("http://").parse().unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let keep = seen.clone();
        tokio::spawn(async move {
            while let Ok((c, _)) = l.accept().await {
                let Ok(s) = TcpStream::connect(to).await else { continue };
                let ((cr, cw), (sr, sw)) = (c.into_split(), s.into_split());
                tokio::spawn(carry(cr, sw, keep.clone()));
                tokio::spawn(carry(sr, cw, keep.clone()));
            }
        });
        (at, seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_of_the_token_or_what_it_carries_is_seen_between() {
        use base64::Engine;
        let root = crate::store::test_root("wire-sniff");
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let vera = srv.add_member("Vera", &every_right()).unwrap();
        let (at, seen) = sniffer(&srv.url).await;
        let admin = TeamClient::new(&at, &srv.admin_token, "admin-desk").unwrap();
        let member = TeamClient::new(&at, &vera.token, "vera-laptop").unwrap();
        let mut live = member.live().await.unwrap();

        let data = base64::engine::general_purpose::STANDARD.encode(b"profile-data-nobody-reads");
        let body = serde_json::json!({ "name": "Secret Shop Name", "folder": "", "data": data });
        admin.call(Method::PUT, "/profiles/p1", Some(&body)).await.unwrap();
        let heard = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Heard::Event(LiveEvent::TeamChanged { what }) = live.hear().await {
                    break what;
                }
            }
        });
        assert_eq!(heard.await.unwrap(), "profile", "the live channel works inside");

        member.call(Method::POST, "/profiles/p1/lease", None).await.unwrap();
        let session = root.path.join("session.bin");
        std::fs::write(&session, b"session-bytes-nobody-reads/".repeat(300_000)).unwrap();
        let rev = member.upload("/profiles/p1/session?base_session_rev=0", &session).await.unwrap().session_rev;
        let back = root.path.join("back.bin");
        let front = member.download("/profiles/p1/session", &back).await.unwrap();
        assert_eq!((front.session_rev, std::fs::read(&back).unwrap()), (rev, std::fs::read(&session).unwrap()));
        let rows: serde_json::Value = member.get("/profiles").await.unwrap();
        assert!(rows.to_string().contains("Secret Shop Name"));

        let seen = seen.lock().unwrap().clone();
        assert!(seen.len() > 2 * 8_000_000, "the session went through the relay both ways: {} bytes", seen.len());
        assert!(holds(&seen, format!("Upgrade: {}", team_wire::UPGRADE).as_bytes()), "the switch is what is said in the clear");
        let name = crate::team::device_name();
        let secrets = [
            vera.token.as_str(),
            srv.admin_token.as_str(),
            &srv.admin_token[..srv.admin_token.rfind('.').unwrap()],
            "Bearer",
            "Secret Shop Name",
            &data,
            "profile-data-nobody-reads",
            "session-bytes-nobody-reads",
            "vera-laptop",
            "admin-desk",
            name.as_str(),
            "/profiles",
            "websocket",
            srv.server.team_id(),
        ];
        for secret in secrets.iter().filter(|s| s.len() > 3) {
            assert!(!holds(&seen, secret.as_bytes()), "seen between the launcher and the server: {secret}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_with_another_key_at_the_address_is_refused_before_the_token_goes() {
        let root = crate::store::test_root("wire-other-key");
        let srv = TestServer::start(root.path.join("ours")).await.unwrap();
        let other = TestServer::start(root.path.join("theirs")).await.unwrap();
        let c = TeamClient::held_to(&other.url, &srv.admin_token, "d", &srv.server.public_key());
        let e = c.call(Method::GET, "/me", None).await.unwrap_err();
        assert!(matches!(e, TeamError::Garbled(_)), "{e:?}");
        let words = format!("{} is not this team's server: it shows another key. If the server was set up anew, paste a new invite", other.url);
        assert_eq!(e.to_string(), words);

        // A process of someone else's at the address, listening for what comes in.
        let (tls, _) = some_key();
        let got = Arc::new(Mutex::new(Vec::new()));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let keep = got.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = l.accept().await {
                if let Ok(mut io) = accept_wire(tcp, tls.clone()).await {
                    let mut buf = Vec::new();
                    let _ = io.read_to_end(&mut buf).await;
                    keep.lock().unwrap().extend(buf);
                }
            }
        });
        let c = TeamClient::held_to(&at, &srv.admin_token, "d", &srv.server.public_key());
        let e = c.call(Method::GET, "/me", None).await.unwrap_err();
        assert!(e.to_string().starts_with(&format!("{at} is not this team's server")), "{e}");
        assert!(got.lock().unwrap().is_empty(), "nothing reached it inside the TLS");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_in_between_at_a_join_cannot_prove_the_token_and_gets_none_of_it() {
        let root = crate::store::test_root("wire-in-between");
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let vera = srv.add_member("Vera", &every_right()).unwrap();
        let real: std::net::SocketAddr = srv.url.trim_start_matches("http://").parse().unwrap();
        // Its own TLS toward the launcher, the real server's behind it: every byte read, passed on.
        let (tls, _) = some_key();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let keep = heard.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = l.accept().await {
                let (tls, keep) = (tls.clone(), keep.clone());
                tokio::spawn(async move {
                    let Ok(inside) = accept_wire(tcp, tls).await else { return };
                    let mut up: Box<dyn Io> = Box::new(TcpStream::connect(real).await.unwrap());
                    switch(&mut up, &real.to_string()).await.unwrap();
                    let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
                    let up = tokio_rustls::TlsConnector::from(team_wire::client_config(team_wire::Pin::learn()))
                        .connect(name, up)
                        .await
                        .unwrap();
                    let ((ir, iw), (ur, uw)) = (tokio::io::split(inside), tokio::io::split(up));
                    tokio::join!(carry(ir, uw, keep), carry(ur, iw, Arc::new(Mutex::new(Vec::new()))));
                });
            }
        });
        let c = TeamClient::new(&at, &vera.token, "vera-laptop").unwrap();
        let e = c.call(Method::GET, "/me", None).await.unwrap_err();
        assert_eq!(e.to_string(), format!("{at} could not prove it is the server this token is from"));
        assert!(c.server_key().is_none());
        // Asked again, still no: the key it showed first is the one it is held to.
        assert!(c.call(Method::GET, "/me", None).await.is_err());

        let heard = String::from_utf8_lossy(&heard.lock().unwrap()).into_owned();
        assert!(heard.contains(team_wire::VOUCH), "the vouch went through it: {heard}");
        assert!(!heard.contains(&vera.token) && !heard.contains("Bearer") && !heard.contains("/me"), "{heard}");
        assert!(!heard.contains("vera-laptop"), "{heard}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_old_server_or_another_program_is_refused_in_words() {
        // A team server from before the wire: every unknown path is its 404.
        let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", old.local_addr().unwrap());
        let app = axum::Router::new()
            .route("/health", axum::routing::get(|| async { r#"{"status":"ok","team":"t"}"# }))
            .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, r#"{"error":"no such thing","code":"not_found"}"#) });
        tokio::spawn(async move { axum::serve(old, app).await.unwrap() });
        let e = TeamClient::new(&at, "stm_x", "d").unwrap().call(Method::GET, "/health", None).await.unwrap_err();
        assert!(matches!(e, TeamError::Garbled(_)), "{e:?}");
        assert_eq!(e.to_string(), format!("{at} does not answer as an encrypted team server: it is not one, or it is older than this launcher"));

        // One that wants a newer switch than this launcher speaks.
        let newer = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", newer.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = newer.accept().await {
                let _ = head_of(&mut tcp).await;
                let _ = tcp.write_all(b"HTTP/1.1 426 Upgrade Required\r\nUpgrade: shardx-tls/2\r\nContent-Length: 0\r\n\r\n").await;
            }
        });
        let e = TeamClient::new(&at, "stm_x", "d").unwrap().call(Method::GET, "/health", None).await.unwrap_err();
        assert_eq!(e.to_string(), format!("{at} is a newer team server than this launcher: update ShardBrowser"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_token_the_server_never_issued_is_refused_at_the_vouch_and_withdraws_nothing() {
        let root = crate::store::test_root("wire-unknown");
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let c = TeamClient::new(&srv.url, &format!("stm_{}", "Q".repeat(43)), "d").unwrap();
        let e = c.call(Method::GET, "/me", None).await.unwrap_err();
        assert_eq!((e.status(), e.code()), (Some(404), Some(&Code::UnknownToken)));
        assert!(!e.access_lost(), "said before the server is trusted: never a withdrawal");
    }

    /// A session of 232 MB up and down, over the wire and over plain http as before, same server.
    /// Run it built as a launcher is shipped — a debug build's TLS is ten times
    /// slower than ring's own assembly, so only the release numbers mean anything:
    /// `cargo test --release --lib space::client::tests::a_big_session_is_about_as_fast_over_the_wire -- --ignored --nocapture`
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_big_session_is_about_as_fast_over_the_wire() {
        use std::time::Instant;
        let root = crate::store::test_root("wire-speed");
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let plain = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let plain_at = format!("http://{}", plain.local_addr().unwrap());
        let router = srv.server.router();
        tokio::spawn(async move { axum::serve(plain, router).await.unwrap() });

        let admin = TeamClient::new(&srv.url, &srv.admin_token, "speed").unwrap();
        admin.call(Method::PUT, "/profiles/p1", Some(&serde_json::json!({ "name": "p1", "folder": "", "data": "" }))).await.unwrap();
        admin.call(Method::POST, "/profiles/p1/lease", None).await.unwrap();
        let session = root.path.join("session.bin");
        let piece: Vec<u8> = (0..1u32 << 20).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        std::fs::write(&session, piece.repeat(232)).unwrap();
        let back = root.path.join("back.bin");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut rev = 0;
        let mut rows = Vec::new();
        for round in 0..2 {
            let t = Instant::now();
            rev = admin.upload(&format!("/profiles/p1/session?base_session_rev={rev}"), &session).await.unwrap().session_rev;
            let up = t.elapsed();
            let t = Instant::now();
            admin.download("/profiles/p1/session", &back).await.unwrap();
            let down = t.elapsed();
            assert_eq!(std::fs::metadata(&back).unwrap().len(), 232 << 20);
            rows.push(("wire", round, up, down));

            let t = Instant::now();
            let file = tokio::fs::File::open(&session).await.unwrap();
            let len = file.metadata().await.unwrap().len();
            let body = reqwest::Body::wrap(FileBody { file, left: len, buf: vec![0; 256 * 1024].into_boxed_slice() });
            let r = http
                .put(format!("{plain_at}/profiles/p1/session?base_session_rev={rev}"))
                .bearer_auth(&srv.admin_token)
                .header("X-Device-Id", "speed")
                .header(CONTENT_LENGTH, len)
                .body(body)
                .send()
                .await
                .unwrap();
            let info: SessionInfo = r.json().await.unwrap();
            rev = info.session_rev;
            let up = t.elapsed();
            let t = Instant::now();
            let mut r = http
                .get(format!("{plain_at}/profiles/p1/session"))
                .bearer_auth(&srv.admin_token)
                .header("X-Device-Id", "speed")
                .send()
                .await
                .unwrap();
            let status = r.status();
            let mut out = tokio::fs::File::create(&back).await.unwrap();
            let mut got = 0u64;
            while let Some(c) = r.chunk().await.unwrap() {
                got += c.len() as u64;
                out.write_all(&c).await.unwrap();
            }
            out.sync_all().await.unwrap();
            let down = t.elapsed();
            assert!(status.is_success() && got > 232 << 20, "the plain download: {status} {got} bytes");
            rows.push(("plain", round, up, down));
        }
        for (how, round, up, down) in &rows {
            println!("{how:>5} round {round}: up {up:?}, down {down:?}, both {:?}", *up + *down);
        }
        let both = |how: &str| rows.iter().filter(|r| r.0 == how && r.1 == 1).map(|r| r.2 + r.3).sum::<Duration>();
        let (wire, plain) = (both("wire"), both("plain"));
        if cfg!(debug_assertions) {
            println!("a debug build: the numbers above are the shape, not the cost; build with --release to judge it");
            return;
        }
        assert!(wire <= plain * 2 + Duration::from_secs(2), "the wire took {wire:?} against {plain:?} in the clear");
    }

    /// Files that talk HTTP or open sockets for something other than a team.
    const OTHER_TRAFFIC: [(&str, &str); 11] = [
        ("process/close.rs", "Stop's Browser.close on a launched browser's own devtools"),
        ("live_e2e.rs", "the live end-to-end harness, run by hand: a test site, a proxy, CDP, the server raw"),
        ("runtime.rs", "engine and launcher downloads"),
        ("psapi.rs", "the ProxyShard billing API"),
        ("mcp_setup.rs", "the MCP server download"),
        ("gpu_caps.rs", "the engine's own devtools on localhost"),
        ("cdp.rs", "CDP to the launched browsers"),
        ("requests.rs", "the automation runner's own requests"),
        ("space/local/proxy.rs", "proxy checks"),
        ("space/local/extensions.rs", "extension downloads"),
        ("space/local/extensions/widevine.rs", "the Widevine CDM from Google's component updater"),
    ];

    const TALKS: [&str; 6] = ["reqwest", "tungstenite", "wreq", "TcpStream::connect", "hyper", "rustls"];

    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Code lines only: a comment may name a client.
    fn code(text: &str) -> impl Iterator<Item = (usize, &str)> {
        text.lines().enumerate().filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with('*'))
        })
    }

    #[test]
    fn no_http_outside_client() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        let mut bad = Vec::new();
        let mut unused: Vec<&str> = OTHER_TRAFFIC.iter().map(|(f, _)| *f).collect();
        for path in &files {
            let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            if rel == "space/client.rs" {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            let allowed = OTHER_TRAFFIC.iter().any(|(f, _)| *f == rel);
            for (n, line) in code(&text) {
                if TALKS.iter().any(|t| line.contains(t)) {
                    unused.retain(|f| *f != rel);
                    if !allowed {
                        bad.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                    }
                }
                // The team token, and sync's, are handed out only to the client.
                let hands_out = line.contains("team::reach(") || line.contains("sync_reach(");
                if (hands_out && rel != "team.rs") || (rel != "team.rs" && line.contains("fn reach(")) {
                    bad.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
        assert!(bad.is_empty(), "talking to a server outside space/client.rs:\n{}", bad.join("\n"));
        assert!(unused.is_empty(), "no longer talks HTTP, take it off the list: {unused:?}");
        // Inside it, one pool and one way to open a connection: the switch to TLS.
        let ours = std::fs::read_to_string(src.join("space/client.rs")).unwrap();
        let ours = ours.split("#[cfg(test)]\nmod tests").next().unwrap();
        let builders = code(ours).filter(|(_, l)| l.contains("Client::builder(")).count();
        assert_eq!(builders, 1, "one place makes team connections: link()");
        let opens = code(ours).filter(|(_, l)| l.contains("TcpStream::connect")).count();
        assert_eq!(opens, 1, "one place opens them: open()");
    }

    #[test]
    fn live_events_are_read_and_unknown_ones_pass() {
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"grants-changed","epoch":4}"#).unwrap();
        assert_eq!(e, LiveEvent::GrantsChanged { epoch: 4 });
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"revoked"}"#).unwrap();
        assert_eq!(e, LiveEvent::Revoked);
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"token-replaced"}"#).unwrap();
        assert_eq!(e, LiveEvent::TokenReplaced);
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"team-changed","what":"profile","id":"p1"}"#).unwrap();
        assert_eq!(e, LiveEvent::TeamChanged { what: "profile".into() });
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"personal-changed","what":"profile","id":"p1"}"#).unwrap();
        assert_eq!(e, LiveEvent::PersonalChanged { what: "profile".into() });
        let e: LiveEvent = serde_json::from_str(r#"{"kind":"something-new","what":"x"}"#).unwrap();
        assert_eq!(e, LiveEvent::Other);
    }
}
