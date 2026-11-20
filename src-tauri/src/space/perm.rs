//! Passes for the rights a door needs. Only this module makes them, so a
//! function that takes a pass cannot be reached without asking here first.
//! The personal space grants every right. A team's rights come from the
//! permission its server signed, checked here before anything believes it.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use team_model::{decide, Action, Grant, Target};

use super::error::{CmdError, Need};

/// The right to start a profile's browser.
pub struct Start;

/// The right to read and write a profile's cookies, CDP included.
pub struct Cookies;

/// The right to change a team profile or proxy.
pub struct Edit;

/// The right to move a team profile or proxy to the team's trash.
pub struct Delete;

/// The right to make a profile in a folder, or a proxy in the team.
pub struct Create;

/// A right a pass can carry, as the team's rules name it.
pub trait Right {
    const ACTION: Action;
}

impl Right for Start {
    const ACTION: Action = Action::Start;
}

impl Right for Cookies {
    const ACTION: Action = Action::Cookies;
}

impl Right for Edit {
    const ACTION: Action = Action::Edit;
}

impl Right for Delete {
    const ACTION: Action = Action::Delete;
}

impl Right for Create {
    const ACTION: Action = Action::Create;
}

/// What a pass is held on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum On {
    Profile { id: String, folder: String },
    Folder(String),
    Team,
    Proxy(String),
}

impl On {
    fn target(&self) -> Target<'_> {
        match self {
            On::Profile { id, folder } => Target::profile(id, folder),
            On::Folder(name) => Target::folder(name),
            On::Team => Target::team(),
            // A proxy is in no folder, as the server judges it.
            On::Proxy(id) => Target { profile_id: Some(id), folder: None },
        }
    }

    fn name(&self) -> &'static str {
        match self {
            On::Profile { .. } => "profile",
            On::Folder(_) => "folder",
            On::Team => "team",
            On::Proxy(_) => "proxy",
        }
    }
}

/// A right `A`, as this module granted it.
pub struct Granted<A> {
    on: On,
    _right: PhantomData<fn() -> A>,
}

impl<A> Granted<A> {
    /// The profile's id; empty for a pass held on something else.
    pub fn profile_id(&self) -> &str {
        match &self.on {
            On::Profile { id, .. } => id,
            _ => "",
        }
    }

    pub fn on(&self) -> &On {
        &self.on
    }
}

fn grant<A>(profile_id: &str) -> Granted<A> {
    Granted {
        on: On::Profile { id: profile_id.to_string(), folder: String::new() },
        _right: PhantomData,
    }
}

/// The team's name for an action, as a refusal names what it needs.
pub fn action_name(a: Action) -> &'static str {
    match a {
        Action::See => "see",
        Action::Start => "start",
        Action::Edit => "edit",
        Action::Create => "create",
        Action::Delete => "delete",
        Action::Cookies => "cookies",
        Action::TakeOut => "take_out",
    }
}

/// A pass to write to a team, by the permission held for it now. One the member
/// may not See reads as missing; the server decides again on the write.
pub fn team_write<A: Right>(team_id: &str, on: On) -> Result<Granted<A>, CmdError> {
    if !usable(team_id) {
        return Err(no_permission());
    }
    if let On::Profile { id, .. } | On::Proxy(id) = &on {
        if !allows(team_id, Action::See, &on.target()) {
            return Err(CmdError::Missing(format!("no {} {id} in this team", on.name())));
        }
    }
    if !allows(team_id, A::ACTION, &on.target()) {
        return Err(CmdError::Forbidden(Need { right: action_name(A::ACTION), on: on.name() }));
    }
    Ok(Granted { on, _right: PhantomData })
}

/// Whether a permission is held for the team and has not run out.
pub fn usable(team_id: &str) -> bool {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id).is_some_and(|p| !p.expired(&SystemClock))
}

/// Nothing is written to a team without a permission to judge it by.
pub fn no_permission() -> CmdError {
    CmdError::Offline("no current permission from the team: it has to be reachable to change anything".into())
}

/// A personal profile: always. Only one in this machine's own store.
pub fn start(profile_id: &str) -> Result<Granted<Start>> {
    personal(profile_id)
}

/// A personal profile: always. Only one in this machine's own store.
pub fn cookies(profile_id: &str) -> Result<Granted<Cookies>> {
    personal(profile_id)
}

fn personal<A>(profile_id: &str) -> Result<Granted<A>> {
    // In a team, a personal profile is not there, and a team one has its own passes.
    if let Some(team) = crate::team::active_team() {
        if super::cache::has_profile(&team, profile_id) {
            return Err(CmdError::NotOffered(super::TEAM_NOT_YET).into());
        }
        return Err(CmdError::Missing(format!("no profile {profile_id} in this team")).into());
    }
    if !super::local::profile::exists(profile_id) {
        return Err(CmdError::Missing(format!("no profile {profile_id} in your own space")).into());
    }
    Ok(grant(profile_id))
}

/// A team profile as the server listed it, under the permission held then.
/// Only [`look`] makes one, so a pass never rests on a folder remembered from before.
#[derive(Debug, Clone)]
pub struct Seen {
    id: String,
    folder: String,
    epoch: u64,
}

impl Seen {
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Ask the server where these team profiles are now. One it no longer shows
/// this member is left out: hidden or gone.
pub async fn look(client: &super::client::TeamClient, team_id: &str, ids: &[&str]) -> Result<Vec<Seen>> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        folder: String,
    }
    let epoch = held_epoch(team_id).context("no permission from the team yet")?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<String> = ids.iter().map(|id| url::form_urlencoded::byte_serialize(id.as_bytes()).collect()).collect();
    let rows: Vec<Row> = client.get(&format!("/profiles?ids={}", list.join(","))).await?;
    Ok(rows.into_iter().map(|r| Seen { id: r.id, folder: r.folder, epoch }).collect())
}

/// Whether the rules moved since this look, so a pass on it is refused for
/// that alone. A profile just made moves them: it changes what the member sees.
pub fn moved_since(team_id: &str, seen: &Seen) -> bool {
    held_epoch(team_id).is_some_and(|now| now != seen.epoch)
}

/// A pass on a team profile, from the permission held for its team. A look
/// from under an older permission is not enough: the rules or the sight moved.
pub fn team_pass<A: Right>(team_id: &str, seen: &Seen) -> Result<Granted<A>> {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    let p = held.get(team_id).context("no permission from the team yet")?;
    if p.epoch() != seen.epoch {
        bail!("the team's rules changed since the profile was looked up");
    }
    p.pass(&SystemClock, &seen.id, &seen.folder)
}

/// What the held permission lets this member do with one team profile. Shown,
/// not trusted: a door still asks for a pass, and the server decides again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rights {
    pub see: bool,
    pub start: bool,
    pub edit: bool,
    pub delete: bool,
    pub cookies: bool,
    pub take_out: bool,
}

/// The rights on a team profile; none while no permission is held or it ran out.
pub fn rights(team_id: &str, profile_id: &str, folder: &str) -> Rights {
    let t = Target::profile(profile_id, folder);
    let may = |a| allows(team_id, a, &t);
    Rights {
        see: may(Action::See),
        start: may(Action::Start),
        edit: may(Action::Edit),
        delete: may(Action::Delete),
        cookies: may(Action::Cookies),
        take_out: may(Action::TakeOut),
    }
}

/// Whether the held permission allows `action` on `target` now.
pub fn allows(team_id: &str, action: Action, target: &Target<'_>) -> bool {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id)
        .is_some_and(|p| !p.expired(&SystemClock) && decide(&p.claims.grants, action, target).is_allowed())
}

/// Whether the held permission lists the team's items of `kind` (team_model `reads_items`).
pub fn reads_items(team_id: &str, kind: &str) -> bool {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id).is_some_and(|p| !p.expired(&SystemClock) && team_model::reads_items(&p.claims.grants, kind))
}

/// Whether the server hands this member a team proxy's login: See, and a use for it.
pub fn may_use_proxy(team_id: &str, proxy_id: &str) -> bool {
    // A proxy is in no folder, as the server judges it.
    let t = Target { profile_id: Some(proxy_id), folder: None };
    allows(team_id, Action::See, &t)
        && [Action::Start, Action::Edit, Action::TakeOut].into_iter().any(|a| allows(team_id, a, &t))
}

// ---- a team's permission ----

/// Who a permission has to be for.
pub struct Expect<'a> {
    /// The team's public key, from `/me` at join.
    pub key: &'a str,
    pub team: &'a str,
    /// The member this launcher joined as.
    pub member: &'a str,
}

/// Why a permission was not believed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// The team's key on file is not a key: join again.
    NoKey,
    /// Not signed by the team's key, or changed since it was.
    Signature,
    /// Not a permission at all.
    Garbled,
    Expired,
    OtherTeam,
    OtherMember,
    /// An edition from before one already taken: replayed, not issued now.
    Older,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Rejected::NoKey => "no key to check the team's permission with: join the team again",
            Rejected::Signature => "the permission is not signed by the team's key",
            Rejected::Garbled => "the permission is not readable",
            Rejected::Expired => "the permission has run out, or this computer's clock is ahead",
            Rejected::OtherTeam => "the permission is for another team",
            Rejected::OtherMember => "the permission is for another member",
            Rejected::Older => "the permission is older than one this launcher already had",
        })
    }
}

impl std::error::Error for Rejected {}

#[derive(Debug, Clone, Deserialize)]
struct Claims {
    sub: String,
    team: String,
    epoch: u64,
    iat: u64,
    exp: u64,
    grants: Vec<Grant>,
}

/// A permission that checked out, and when it runs out on the boot clock.
#[derive(Debug, Clone)]
pub struct Permission {
    claims: Claims,
    /// Receipt on the boot clock, a clock nobody can set.
    received: Duration,
    /// Receipt plus `exp - iat`, on the same clock.
    until: Duration,
    /// How far this computer's wall clock read past `iat` at receipt.
    lead: u64,
}

/// Whether a team's key can check a permission at all: 32 bytes, base64url.
pub fn is_key(key: &str) -> bool {
    URL_SAFE_NO_PAD.decode(key.trim()).is_ok_and(|k| k.len() == 32)
}

/// Check a signed permission. Expiry is the earlier of the wall-clock `exp`
/// and `exp - iat` counted from now on the boot clock.
pub fn verify(token: &str, expect: &Expect<'_>, clock: &dyn Clock) -> Result<Permission, Rejected> {
    if !is_key(expect.key) {
        return Err(Rejected::NoKey);
    }
    let raw = URL_SAFE_NO_PAD.decode(expect.key.trim()).map_err(|_| Rejected::NoKey)?;
    let mut v = Validation::new(Algorithm::EdDSA);
    // Expiry is checked below, on both clocks.
    v.validate_exp = false;
    v.validate_aud = false;
    v.set_required_spec_claims(&["exp"]);
    let key = DecodingKey::from_ed_der(&raw);
    let data = jsonwebtoken::decode::<Claims>(token, &key, &v).map_err(|e| match e.kind() {
        ErrorKind::InvalidSignature | ErrorKind::InvalidAlgorithm => Rejected::Signature,
        _ => Rejected::Garbled,
    })?;
    let claims = data.claims;
    if claims.team != expect.team {
        return Err(Rejected::OtherTeam);
    }
    if claims.sub != expect.member {
        return Err(Rejected::OtherMember);
    }
    let window = Duration::from_secs(claims.exp.saturating_sub(claims.iat));
    let received = clock.since_boot();
    let lead = clock.wall().saturating_sub(claims.iat);
    let p = Permission { until: received + window, received, lead, claims };
    if window.is_zero() || p.expired(clock) {
        return Err(Rejected::Expired);
    }
    Ok(p)
}

impl Permission {
    pub fn epoch(&self) -> u64 {
        self.claims.epoch
    }

    /// The edition this is, to refuse an older one later.
    pub fn floor(&self) -> crate::team::Floor {
        crate::team::Floor { epoch: self.claims.epoch, iat: self.claims.iat }
    }

    /// An older edition than `floor`: an earlier epoch, issued no later than
    /// that one. A server set back to an older copy still issues later ones.
    pub fn older_than(&self, floor: crate::team::Floor) -> bool {
        self.claims.epoch < floor.epoch && self.claims.iat <= floor.iat
    }

    pub fn expired(&self, clock: &dyn Clock) -> bool {
        clock.wall() >= self.claims.exp || clock.since_boot() >= self.until
    }

    /// What is left of it, zero once it has run out.
    fn left(&self, clock: &dyn Clock) -> Duration {
        let by_boot = self.until.saturating_sub(clock.since_boot());
        let by_wall = Duration::from_secs(self.claims.exp.saturating_sub(clock.wall()));
        by_boot.min(by_wall)
    }

    /// A third before the end, and not before a third of the window has passed.
    /// A clock running ahead cuts every permission short by its lead: then
    /// halfway through what it had, so the next one comes before it runs out.
    pub fn refresh_in(&self, clock: &dyn Clock) -> Duration {
        let window = Duration::from_secs(self.claims.exp.saturating_sub(self.claims.iat));
        let usable = window.min(Duration::from_secs(self.claims.exp.saturating_sub(self.claims.iat + self.lead)));
        let now = clock.since_boot();
        let soonest = (self.received + (window / 3).min(usable / 2)).saturating_sub(now);
        self.left(clock).saturating_sub(window / 3).max(soonest)
    }

    /// This computer's clock, ahead of the server's by more than a minute: by how much.
    pub fn clock_ahead(&self) -> Option<Duration> {
        (self.lead > 60).then(|| Duration::from_secs(self.lead))
    }

    /// A pass on one profile of the team, by the team's own rule.
    pub fn pass<A: Right>(&self, clock: &dyn Clock, profile_id: &str, folder: &str) -> Result<Granted<A>> {
        if self.expired(clock) {
            bail!("the team's permission has run out: the team server has to be reachable, and this computer's clock right");
        }
        if !decide(&self.claims.grants, A::ACTION, &Target::profile(profile_id, folder)).is_allowed() {
            return Err(CmdError::Forbidden(Need { right: action_name(A::ACTION), on: "profile" }).into());
        }
        Ok(Granted { on: On::Profile { id: profile_id.into(), folder: folder.into() }, _right: PhantomData })
    }
}

fn held() -> &'static RwLock<HashMap<String, Permission>> {
    static HELD: OnceLock<RwLock<HashMap<String, Permission>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

/// The team's permission, checked against what was saved at join.
fn checked(team_id: &str, token: &str) -> Result<Permission, Rejected> {
    let link = crate::team::link(team_id).ok().flatten().ok_or(Rejected::NoKey)?;
    let expect = Expect { key: &link.perm_key, team: &link.id, member: &link.member_id };
    verify(token, &expect, &SystemClock)
}

/// The rules a team's permission carries, checked but not held.
pub fn rules(team_id: &str, token: &str) -> Result<Vec<Grant>, Rejected> {
    Ok(checked(team_id, token)?.claims.grants)
}

/// Check the team's permission against what was saved at join, and hold it
/// in place of the last one. Answers its epoch.
pub fn accept(team_id: &str, token: &str) -> Result<u64, Rejected> {
    let p = checked(team_id, token)?;
    let mut held = held().write().unwrap_or_else(|e| e.into_inner());
    let newest = held.get(team_id).map(Permission::floor).unwrap_or_default();
    if p.older_than(crate::team::floor(team_id)) || p.older_than(newest) {
        return Err(Rejected::Older);
    }
    if let Err(e) = crate::team::raise_floor(team_id, p.floor()) {
        eprintln!("[launcher] team {team_id}: the permission's edition was not saved: {e:#}");
    }
    let epoch = p.epoch();
    held.insert(team_id.to_string(), p);
    Ok(epoch)
}

/// Drop what is held for the team: it was left or withdrawn.
pub fn forget(team_id: &str) {
    held().write().unwrap_or_else(|e| e.into_inner()).remove(team_id);
}

pub fn held_epoch(team_id: &str) -> Option<u64> {
    held().read().unwrap_or_else(|e| e.into_inner()).get(team_id).map(Permission::epoch)
}

/// When the held permission should be renewed; None when none is held.
pub fn refresh_in(team_id: &str) -> Option<Duration> {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id).map(|p| p.refresh_in(&SystemClock))
}

/// How far this computer's clock is ahead of the team server's, past a minute.
pub fn clock_ahead(team_id: &str) -> Option<Duration> {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id).and_then(Permission::clock_ahead)
}

/// How long the held permission has left, by the earlier of its clocks; None when none is held.
pub fn time_left(team_id: &str) -> Option<Duration> {
    let held = held().read().unwrap_or_else(|e| e.into_inner());
    held.get(team_id).map(|p| p.left(&SystemClock))
}

/// As if this computer slept for `by`: the boot clock moved on, the wall clock did not.
#[cfg(test)]
pub fn slept(team_id: &str, by: Duration) {
    let mut held = held().write().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = held.get_mut(team_id) {
        p.until = p.until.saturating_sub(by);
        p.received = p.received.saturating_sub(by);
    }
}

// ---- clocks ----

/// The two clocks a permission's life is measured on.
pub trait Clock: Send + Sync {
    /// Seconds since 1970 by the wall clock, which anyone can set.
    fn wall(&self) -> u64;
    /// Time since boot, sleep included. Nobody can set it back.
    fn since_boot(&self) -> Duration;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn wall(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn since_boot(&self) -> Duration {
        boot::since_boot()
    }
}

#[cfg(target_os = "macos")]
mod boot {
    use std::time::Duration;

    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        // Counts while asleep, unlike mach_absolute_time.
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }

    pub fn since_boot() -> Duration {
        let mut tb = Timebase { numer: 0, denom: 0 };
        // SAFETY: one reads a counter, the other fills the struct it is given.
        let (ticks, ok) = unsafe { (mach_continuous_time(), mach_timebase_info(&mut tb)) };
        if ok != 0 || tb.denom == 0 {
            return Duration::from_nanos(ticks);
        }
        let nanos = ticks as u128 * tb.numer as u128 / tb.denom as u128;
        Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod boot {
    use std::time::Duration;

    pub fn since_boot() -> Duration {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime fills the timespec it is given.
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
            return Duration::ZERO;
        }
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

#[cfg(windows)]
mod boot {
    use std::time::Duration;

    pub fn since_boot() -> Duration {
        let mut t: u64 = 0;
        // SAFETY: fills the counter, in 100 ns units, sleep included.
        unsafe { windows_sys::Win32::System::WindowsProgramming::QueryInterruptTime(&mut t) };
        Duration::from_nanos(t.saturating_mul(100))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android", windows)))]
mod boot {
    use std::time::{Duration, Instant};

    // No sleep-counting clock wired here: time since the launcher started.
    pub fn since_boot() -> Duration {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A team key pair, made for one test: the signing half and the public one.
    struct Key {
        signing: EncodingKey,
        public: String,
    }

    fn key() -> Key {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        let sk = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let der = sk.to_pkcs8_der().unwrap();
        Key {
            signing: EncodingKey::from_ed_der(der.as_bytes()),
            public: URL_SAFE_NO_PAD.encode(sk.verifying_key().to_bytes()),
        }
    }

    /// A clock the test sets by hand.
    struct FakeClock {
        wall: AtomicU64,
        boot_secs: AtomicU64,
    }

    impl FakeClock {
        fn at(wall: u64, boot_secs: u64) -> Self {
            FakeClock { wall: AtomicU64::new(wall), boot_secs: AtomicU64::new(boot_secs) }
        }
        fn set(&self, wall: u64, boot_secs: u64) {
            self.wall.store(wall, Ordering::SeqCst);
            self.boot_secs.store(boot_secs, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn wall(&self) -> u64 {
            self.wall.load(Ordering::SeqCst)
        }
        fn since_boot(&self) -> Duration {
            Duration::from_secs(self.boot_secs.load(Ordering::SeqCst))
        }
    }

    fn sign(key: &Key, claims: serde_json::Value) -> String {
        jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, &key.signing).unwrap()
    }

    fn claims(iat: u64, exp: u64) -> serde_json::Value {
        json!({
            "sub": "m1", "team": "t1", "epoch": 3, "iat": iat, "exp": exp,
            "grants": [
                { "scope": { "kind": "folder", "name": "Ads" }, "action": "start", "allow": true },
                { "scope": { "kind": "profile", "id": "p9" }, "action": "start", "allow": false },
            ],
        })
    }

    fn expect(key: &Key) -> Expect<'_> {
        Expect { key: &key.public, team: "t1", member: "m1" }
    }

    #[test]
    fn a_permission_signed_by_the_team_for_us_is_believed() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        assert_eq!(p.epoch(), 3);
        assert!(!p.expired(&clock));
    }

    #[test]
    fn a_permission_not_ours_is_refused() {
        let (ours, theirs) = (key(), key());
        let clock = FakeClock::at(1_000, 50);
        let good = sign(&ours, claims(1_000, 1_900));

        let foreign = sign(&theirs, claims(1_000, 1_900));
        assert_eq!(verify(&foreign, &expect(&ours), &clock).unwrap_err(), Rejected::Signature);

        // A refusal turned into an allowance after signing: the signature stops matching.
        let mut parts: Vec<String> = good.split('.').map(str::to_owned).collect();
        let mut body: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
        body["grants"][1]["allow"] = json!(true);
        parts[1] = URL_SAFE_NO_PAD.encode(body.to_string());
        assert_eq!(verify(&parts.join("."), &expect(&ours), &clock).unwrap_err(), Rejected::Signature);

        // Signed with a shared secret instead of the team's key.
        let hs = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims(1_000, 1_900),
            &EncodingKey::from_secret(ours.public.as_bytes()),
        )
        .unwrap();
        assert_eq!(verify(&hs, &expect(&ours), &clock).unwrap_err(), Rejected::Signature);

        let mut other_team = claims(1_000, 1_900);
        other_team["team"] = json!("t2");
        assert_eq!(verify(&sign(&ours, other_team), &expect(&ours), &clock).unwrap_err(), Rejected::OtherTeam);

        let mut other_member = claims(1_000, 1_900);
        other_member["sub"] = json!("m2");
        assert_eq!(
            verify(&sign(&ours, other_member), &expect(&ours), &clock).unwrap_err(),
            Rejected::OtherMember
        );

        let expired = sign(&ours, claims(100, 900));
        assert_eq!(verify(&expired, &expect(&ours), &clock).unwrap_err(), Rejected::Expired);

        assert_eq!(verify("not.a.permission", &expect(&ours), &clock).unwrap_err(), Rejected::Garbled);
        let no_key = Expect { key: "", ..expect(&ours) };
        assert_eq!(verify(&good, &no_key, &clock).unwrap_err(), Rejected::NoKey);
        let short_key = Expect { key: "AAAA", ..expect(&ours) };
        assert_eq!(verify(&good, &short_key, &clock).unwrap_err(), Rejected::NoKey);
    }

    #[test]
    fn setting_the_wall_clock_back_does_not_stretch_a_permission() {
        let ours = key();
        let clock = FakeClock::at(10_000, 50);
        let p = verify(&sign(&ours, claims(10_000, 10_900)), &expect(&ours), &clock).unwrap();
        // 899 s later on the boot clock, with the wall clock set back an hour.
        clock.set(10_000 - 3_600, 50 + 899);
        assert!(!p.expired(&clock));
        clock.set(10_000 - 3_600, 50 + 900);
        assert!(p.expired(&clock), "the boot clock says the window is over");
    }

    #[test]
    fn sleep_counts_against_a_permission() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        // The lid was shut for an hour and the wall clock did not catch up.
        clock.set(1_010, 50 + 3_600);
        assert!(p.expired(&clock));
    }

    #[test]
    fn the_wall_clock_ends_a_permission_first_when_it_is_earlier() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        // Received late: half of the window was already gone by the wall clock.
        let p = verify(&sign(&ours, claims(550, 1_450)), &expect(&ours), &clock).unwrap();
        clock.set(1_449, 60);
        assert!(!p.expired(&clock));
        clock.set(1_450, 61);
        assert!(p.expired(&clock), "only 11 s on the boot clock, but exp has passed");
    }

    #[test]
    fn a_permission_is_renewed_a_third_before_its_end() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        assert_eq!(p.refresh_in(&clock), Duration::from_secs(600));
        clock.set(1_100, 150);
        assert_eq!(p.refresh_in(&clock), Duration::from_secs(500));
        clock.set(1_700, 750);
        assert_eq!(p.refresh_in(&clock), Duration::ZERO);
    }

    #[test]
    fn a_clock_running_ahead_gets_the_next_permission_before_this_one_runs_out() {
        let ours = key();
        // Twelve minutes ahead: each permission is good for three by the wall clock.
        let clock = FakeClock::at(1_720, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        assert_eq!(p.clock_ahead(), Some(Duration::from_secs(720)));
        assert_eq!(p.refresh_in(&clock), Duration::from_secs(90));
        clock.set(1_810, 140);
        assert!(!p.expired(&clock), "asked for again with time to spare");
        clock.set(1_900, 230);
        assert!(p.expired(&clock));
        assert_eq!(p.refresh_in(&clock), Duration::ZERO);
        // After a sleep, at once.
        clock.set(5_000, 50 + 3_600);
        assert_eq!(p.refresh_in(&clock), Duration::ZERO);
        // A clock that is right asks a third before the end, as ever.
        let right = FakeClock::at(1_000, 50);
        let q = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &right).unwrap();
        assert_eq!((q.clock_ahead(), q.refresh_in(&right)), (None, Duration::from_secs(600)));
    }

    #[test]
    fn an_earlier_edition_is_older_unless_issued_later() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        let floor = |epoch, iat| crate::team::Floor { epoch, iat };
        assert!(p.older_than(floor(4, 1_000)));
        assert!(p.older_than(floor(4, 1_200)));
        assert!(!p.older_than(floor(3, 1_200)), "the same edition");
        assert!(!p.older_than(floor(2, 900)));
        // A server put back to an older copy counts epochs again, but later.
        assert!(!p.older_than(floor(9, 999)));
    }

    #[test]
    fn a_team_pass_follows_the_teams_rule_and_its_clock() {
        let ours = key();
        let clock = FakeClock::at(1_000, 50);
        let p = verify(&sign(&ours, claims(1_000, 1_900)), &expect(&ours), &clock).unwrap();
        assert!(p.pass::<Start>(&clock, "p1", "Ads").is_ok());
        assert!(p.pass::<Start>(&clock, "p9", "Ads").is_err(), "refused on the profile");
        assert!(p.pass::<Start>(&clock, "p2", "Shops").is_err(), "no rule");
        assert!(p.pass::<Cookies>(&clock, "p1", "Ads").is_err(), "Start is not Cookies");
        clock.set(1_000, 50 + 900);
        assert!(p.pass::<Start>(&clock, "p1", "Ads").is_err(), "run out");
        // With nothing held, a team grants nothing.
        let seen = Seen { id: "p1".into(), folder: "Ads".into(), epoch: 3 };
        assert!(team_pass::<Start>("no-such-team", &seen).is_err());
    }

    #[test]
    fn the_personal_space_grants_everything_on_its_own_profiles_only() {
        let _root = crate::store::test_root("perm-personal");
        let dir = crate::store::profiles_dir().unwrap();
        std::fs::write(dir.join("p9.json"), json!({ "_meta": { "id": "p9" } }).to_string()).unwrap();
        assert!(start("p9").is_ok() && cookies("p9").is_ok());
        // A team's profile, or none at all: not a personal pass.
        assert!(start("team-p1").is_err() && cookies("team-p1").is_err());
        assert!(start("../p9").is_err());
    }

    #[test]
    fn the_boot_clock_moves_with_real_time() {
        let a = SystemClock.since_boot();
        std::thread::sleep(Duration::from_millis(30));
        let b = SystemClock.since_boot();
        assert!(a > Duration::ZERO);
        assert!(b >= a + Duration::from_millis(25), "{a:?} then {b:?}");
        assert!(b < a + Duration::from_secs(5), "{a:?} then {b:?}");
    }
}
