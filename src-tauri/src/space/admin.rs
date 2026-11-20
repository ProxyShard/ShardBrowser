//! The Team page: the admin's commands go with the admin token this launcher
//! joined with; a member reads only their own rules, as the team signed them.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use team_model::Grant;

use super::client::{Method, TeamClient, TeamError};

/// A member as the admin's list shows them. `last_seen_at` is None until the
/// invite is first used: invited, not active yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMember {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub created_at: u64,
    #[serde(default)]
    pub epoch: u64,
    #[serde(default)]
    pub last_seen_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamFolder {
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub profiles: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamProfileRef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamInfo {
    pub id: String,
    pub name: String,
}

/// What the admin's Team page reads in one go.
#[derive(Debug, Clone, Serialize)]
pub struct TeamOverview {
    pub team: TeamInfo,
    pub members: Vec<TeamMember>,
    pub folders: Vec<TeamFolder>,
    pub profiles: Vec<TeamProfileRef>,
    /// Each member's whole rule table, by member id.
    pub grants: BTreeMap<String, Vec<Grant>>,
}

// No Debug on the two below: an invite carries the member's token.

/// Shown once: the server's address, "#" and the member's token. A re-issue
/// answers the same, the member's row as it is now included.
#[derive(Serialize)]
pub struct CreatedMember {
    pub member: TeamMember,
    pub invite: String,
}

#[derive(Deserialize)]
struct MemberRow {
    #[serde(flatten)]
    member: TeamMember,
    #[serde(default)]
    grants: Vec<Grant>,
}

/// A member as made or re-issued: the one answer with a token in it.
#[derive(Deserialize)]
struct WithToken {
    #[serde(flatten)]
    member: TeamMember,
    token: String,
}

#[derive(Deserialize)]
struct GrantsAck {
    grants: Vec<Grant>,
}

#[derive(Deserialize)]
struct Me {
    team: TeamInfo,
    /// The address the owner gave the server; the admin's only.
    #[serde(default)]
    public_url: Option<String>,
}

/// The team as its server names itself: `GET /me`.
pub async fn team_info(client: &TeamClient) -> Result<TeamInfo, TeamError> {
    Ok(client.get::<Me>("/me").await?.team)
}

/// The admin's client and the server's address; anyone else is refused here,
/// before a request goes out.
fn admin(team_id: &str) -> Result<(TeamClient, String)> {
    let link = crate::team::link(team_id)?.context("not a member of that team")?;
    if !link.admin {
        bail!("only the team's admin can do that");
    }
    Ok((TeamClient::for_team(team_id)?, link.server))
}

fn invite(server: &str, token: &str) -> String {
    format!("{}#{token}", server.trim_end_matches('/'))
}

/// Where member invites point: the address the owner gave the server, else
/// the one this admin joined by. Read beside the request it is for.
async fn invite_base(c: &TeamClient, joined_at: &str) -> String {
    let given = c.get::<Me>("/me").await.ok().and_then(|me| me.public_url);
    given.and_then(|u| crate::team::server_base(&u).ok()).unwrap_or_else(|| joined_at.to_string())
}

/// The host, when only this computer reaches the server by it.
fn local_only(base: &str) -> Option<String> {
    let url = url::Url::parse(base).ok()?;
    let local = match url.host()? {
        // The parser has lowercased it.
        url::Host::Domain(d) => d == "localhost" || d.ends_with(".localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback() || ip.is_unspecified(),
        url::Host::Ipv6(ip) => ip.is_loopback() || ip.is_unspecified(),
    };
    local.then(|| url.host_str().unwrap_or_default().to_string())
}

/// The invite, and a warning when nobody else can use it.
fn invite_from(base: &str, token: &str) -> String {
    if let Some(host) = local_only(base) {
        crate::notify_warning(format!(
            "This invite starts with {host}, which only this computer reaches: join the team by its network address, or start the server with SHARDBROWSER_TEAM_PUBLIC_URL."
        ));
    }
    invite(base, token)
}

/// One path segment: a folder may be named anything, "/" and "#" included.
/// "." and ".." are dropped from a URL whatever the escaping, and nothing is named so.
pub(super) fn seg(s: &str) -> Result<String> {
    if matches!(s, "." | "..") {
        bail!("no such thing");
    }
    Ok(s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect())
}

fn member_path(id: &str) -> Result<String> {
    Ok(format!("/admin/members/{}", seg(id)?))
}

fn folder_path(name: &str) -> Result<String> {
    Ok(format!("/admin/folders/{}", seg(name)?))
}

pub async fn overview(team_id: &str) -> Result<TeamOverview> {
    let (c, _) = admin(team_id)?;
    let (team, rows, folders, profiles) = tokio::try_join!(
        team_info(&c),
        c.get::<Vec<MemberRow>>("/admin/members"),
        c.get::<Vec<TeamFolder>>("/folders"),
        c.get::<Vec<TeamProfileRef>>("/profiles"),
    )?;
    let grants = rows.iter().map(|r| (r.member.id.clone(), r.grants.clone())).collect();
    let members = rows.into_iter().map(|r| r.member).collect();
    Ok(TeamOverview { team, members, folders, profiles, grants })
}

pub async fn member_create(team_id: &str, name: &str) -> Result<CreatedMember> {
    let (c, server) = admin(team_id)?;
    let body = json!({ "name": name });
    let made = c.json::<WithToken>(Method::POST, "/admin/members", Some(&body));
    let (made, base) = tokio::join!(made, invite_base(&c, &server));
    let made = made?;
    Ok(CreatedMember { invite: invite_from(&base, &made.token), member: made.member })
}

pub async fn member_set_active(team_id: &str, member_id: &str, active: bool) -> Result<TeamMember> {
    let (c, _) = admin(team_id)?;
    Ok(c.json(Method::PATCH, &member_path(member_id)?, Some(&json!({ "active": active }))).await?)
}

pub async fn member_delete(team_id: &str, member_id: &str) -> Result<()> {
    let (c, _) = admin(team_id)?;
    c.call(Method::DELETE, &member_path(member_id)?, None).await?;
    Ok(())
}

/// A new token for the member; the old one stops working at once, and they
/// read as invited again until the new one is used.
pub async fn member_reissue(team_id: &str, member_id: &str) -> Result<CreatedMember> {
    let (c, server) = admin(team_id)?;
    let path = format!("{}/token", member_path(member_id)?);
    let (made, base) = tokio::join!(c.json::<WithToken>(Method::POST, &path, None), invite_base(&c, &server));
    let made = made?;
    Ok(CreatedMember { invite: invite_from(&base, &made.token), member: made.member })
}

pub async fn grants(team_id: &str, member_id: &str) -> Result<Vec<Grant>> {
    let (c, _) = admin(team_id)?;
    Ok(c.get(&format!("{}/grants", member_path(member_id)?)).await?)
}

/// Answers the table as the server keeps it: duplicates merged, a refusal
/// beside an allowance kept as the refusal.
pub async fn set_grants(team_id: &str, member_id: &str, grants: &[Grant]) -> Result<Vec<Grant>> {
    let (c, _) = admin(team_id)?;
    let path = format!("{}/grants", member_path(member_id)?);
    let ack: GrantsAck = c.json(Method::PUT, &path, Some(&json!({ "grants": grants }))).await?;
    Ok(ack.grants)
}

/// The team's new name, kept in teams.json too; the caller tells the windows.
pub async fn rename(team_id: &str, name: &str) -> Result<(TeamInfo, crate::team::SpaceState)> {
    let (c, _) = admin(team_id)?;
    let info: TeamInfo = c.json(Method::PATCH, "/admin/team", Some(&json!({ "name": name }))).await?;
    if info.id == team_id {
        crate::team::rename(team_id, &info.name)?;
    }
    Ok((info, crate::team::state()?))
}

pub async fn folder_create(team_id: &str, name: &str, color: Option<&str>) -> Result<TeamFolder> {
    let (c, _) = admin(team_id)?;
    let body = json!({ "name": name, "color": color.unwrap_or("") });
    Ok(c.json(Method::POST, "/admin/folders", Some(&body)).await?)
}

pub async fn folder_rename(team_id: &str, name: &str, next: &str) -> Result<TeamFolder> {
    let (c, _) = admin(team_id)?;
    Ok(c.json(Method::PATCH, &folder_path(name)?, Some(&json!({ "name": next }))).await?)
}

pub async fn folder_color(team_id: &str, name: &str, color: &str) -> Result<TeamFolder> {
    let (c, _) = admin(team_id)?;
    Ok(c.json(Method::PATCH, &folder_path(name)?, Some(&json!({ "color": color }))).await?)
}

pub async fn folder_delete(team_id: &str, name: &str) -> Result<()> {
    let (c, _) = admin(team_id)?;
    c.call(Method::DELETE, &folder_path(name)?, None).await?;
    Ok(())
}

// ---- the team's trash: the server judges each row (See to list, Delete to restore or purge) ----

/// A row of the team's trash, as far as this launcher's token may see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamTrashRow {
    pub id: String,
    /// profile, proxy, bookmark, extension, automation or tag.
    pub kind: String,
    pub name: String,
    /// The folder a profile was deleted from; "" for anything else.
    #[serde(default)]
    pub folder: String,
    /// A proxy's own kind (socks5, http…) and country; "" for anything else.
    #[serde(default)]
    pub proxy_kind: String,
    #[serde(default)]
    pub country: String,
    pub revision: u64,
    pub updated_at: u64,
    pub updated_by: String,
    pub deleted_at: u64,
    /// A member id, or "admin".
    pub deleted_by: String,
    /// When the server purges it on its own (unix seconds).
    pub purge_at: u64,
}

/// What a restore put back, at a revision past the one it was deleted at.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeamRestored {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestoredAnswer {
    Profile(Back),
    Proxy(Back),
    Item(Back),
}

#[derive(Deserialize)]
struct Back {
    #[serde(default)]
    kind: String,
    id: String,
    name: String,
    revision: u64,
}

fn trash_path(id: &str, kind: &str) -> Result<(String, String)> {
    let query = if kind.is_empty() { String::new() } else { format!("?kind={}", seg(kind)?) };
    Ok((format!("/trash/{}", seg(id)?), query))
}

pub async fn trash_list(team_id: &str) -> Result<Vec<TeamTrashRow>> {
    let c = TeamClient::for_team(team_id)?;
    Ok(c.get("/trash").await?)
}

/// Back where it was, a profile with its session; its lists here read again at once.
pub async fn trash_restore(team_id: &str, id: &str, kind: &str) -> Result<TeamRestored> {
    let c = TeamClient::for_team(team_id)?;
    let (path, query) = trash_path(id, kind)?;
    tag_name_free(&c, id, kind).await?;
    let back: RestoredAnswer = c.json(Method::POST, &format!("{path}/restore{query}"), None).await?;
    let (kind, b) = match back {
        RestoredAnswer::Profile(b) => ("profile".to_string(), b),
        RestoredAnswer::Proxy(b) => ("proxy".to_string(), b),
        RestoredAnswer::Item(b) => (b.kind.clone(), b),
    };
    // A profile may bring its folder back too.
    super::cache::refetch(team_id, if kind == "profile" { "folder" } else { &kind }).await;
    Ok(TeamRestored { kind, id: b.id, name: b.name, revision: b.revision })
}

/// A tag or a profile template comes back only under a name no live one has: the server
/// does not look, and two of one name could not be edited here.
async fn tag_name_free(c: &TeamClient, id: &str, kind: &str) -> Result<()> {
    let kind = if kind.is_empty() { "tag" } else { kind };
    if kind != "tag" && kind != "template" {
        return Ok(());
    }
    let trash: Vec<TeamTrashRow> = c.get("/trash").await?;
    let Some(row) = trash.iter().find(|r| r.id == id && r.kind == kind) else { return Ok(()) };
    let live: Vec<super::remote::ItemRow> = c.get(&format!("/items/{kind}")).await?;
    if kind == "template" {
        let held = super::local::templates::TemplateState { templates: super::remote::templates(&live) };
        if let Some(t) = held.named(&row.name, id) {
            bail!("a template named {} already exists", t.name);
        }
    } else if let Some(t) = super::remote::tags(&live, &[]).named(&row.name, id) {
        bail!("a tag named {} already exists", t.name);
    }
    Ok(())
}

/// Gone for good, a profile's session with it.
pub async fn trash_purge(team_id: &str, id: &str, kind: &str) -> Result<()> {
    let c = TeamClient::for_team(team_id)?;
    let (path, query) = trash_path(id, kind)?;
    c.call(Method::DELETE, &format!("{path}{query}"), None).await?;
    Ok(())
}

/// This member's rules about what they may see, from the signed permission.
/// Checked, not held: only the live channel takes an edition, as it rechecks what is open.
pub async fn my_access(team_id: &str) -> Result<Vec<Grant>> {
    let c = TeamClient::for_team(team_id)?;
    let issued = c.permission().await?;
    Ok(super::perm::rules(team_id, &issued.permission)?)
}

#[cfg(test)]
mod tests {
    use super::super::client::Code;
    use super::*;
    use crate::team::{self, Space};
    use serde_json::Value;
    use shardbrowser_team_server::{Action, NewMember, Scope, TestServer};

    /// A real team server in-process, joined from this launcher with its admin token.
    struct Team {
        // Stopped before its data goes with the root.
        srv: TestServer,
        _root: crate::store::TestRoot,
        id: String,
    }

    impl Team {
        async fn new() -> Team {
            let root = crate::store::test_root("team-admin").with_config();
            let srv = TestServer::start(root.path.join("server")).await.unwrap();
            team::join(&srv.url, &srv.admin_token).await.unwrap();
            let id = srv.server.team_id().to_string();
            Team { srv, _root: root, id }
        }

        fn add(&self, name: &str, grants: &[Grant]) -> NewMember {
            self.srv.add_member(name, grants).unwrap()
        }

        /// A request in the member's name, from a device of theirs: joined, so held to the team's key.
        fn as_member(&self, token: &str) -> TeamClient {
            TeamClient::held_to(&self.srv.url, token, "their-device", &self.srv.server.public_key())
        }

        async fn make_profile(&self, id: &str, folder: &str) {
            let body = json!({ "name": id.to_uppercase(), "folder": folder, "data": "" });
            let admin = self.as_member(&self.srv.admin_token);
            admin.call(Method::PUT, &format!("/profiles/{id}"), Some(&body)).await.unwrap();
        }

        /// A team profile whose session a device left on the server.
        async fn with_session(&self, id: &str, folder: &str, session: &[u8]) {
            self.make_profile(id, folder).await;
            let admin = self.as_member(&self.srv.admin_token);
            let lease = format!("/profiles/{id}/lease");
            admin.call(Method::POST, &lease, None).await.unwrap();
            let file = self._root.path.join(format!("{id}-up.zip"));
            std::fs::write(&file, session).unwrap();
            let up = admin.upload(&format!("/profiles/{id}/session?base_session_rev=0"), &file).await.unwrap();
            assert_eq!(up.session_rev, 1);
            admin.call(Method::DELETE, &lease, None).await.unwrap();
        }

        /// The session the server keeps for `id`, read under a lease as a device would.
        async fn session_of(&self, id: &str) -> Vec<u8> {
            let admin = self.as_member(&self.srv.admin_token);
            let lease = format!("/profiles/{id}/lease");
            admin.call(Method::POST, &lease, None).await.unwrap();
            let file = self._root.path.join(format!("{id}-down.zip"));
            admin.download(&format!("/profiles/{id}/session"), &file).await.unwrap();
            admin.call(Method::DELETE, &lease, None).await.unwrap();
            std::fs::read(&file).unwrap()
        }

        async fn delete(&self, path: &str) {
            self.as_member(&self.srv.admin_token).call(Method::DELETE, path, None).await.unwrap();
        }
    }

    fn rule(scope: Scope, action: Action, allow: bool) -> Grant {
        Grant { scope, action, allow }
    }

    fn folder(name: &str) -> Scope {
        Scope::Folder { name: name.into() }
    }

    /// The text the interface gets, which errors.ts reads.
    #[track_caller]
    fn says<T>(r: Result<T>, what: &str) {
        match r {
            Ok(_) => panic!("expected a refusal saying {what:?}"),
            Err(e) => assert!(e.to_string().contains(what), "{e:#} does not say {what:?}"),
        }
    }

    #[tokio::test]
    async fn the_admin_reads_the_whole_team_in_one_go() {
        let t = Team::new().await;
        let vera_rules = [
            rule(Scope::Profile { id: "p9".into() }, Action::Start, false),
            rule(folder("Ads"), Action::Start, true),
            rule(Scope::Team, Action::See, true),
        ];
        let vera = t.add("Vera", &vera_rules);
        let ivan = t.add("Ivan", &[]);
        folder_create(&t.id, "Ads", Some("blue")).await.unwrap();
        t.make_profile("p1", "Ads").await;
        t.make_profile("p2", "").await;

        let o = overview(&t.id).await.unwrap();
        assert_eq!(o.team, TeamInfo { id: t.id.clone(), name: "Team".into() });
        let names: Vec<&str> = o.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Vera") && names.contains(&"Ivan"));
        assert!(o.members.iter().all(|m| m.active && m.last_seen_at.is_none()), "both only invited");
        // Sorted as the server keeps them: team, folder, profile.
        assert_eq!(o.grants[&vera.id], [vera_rules[2].clone(), vera_rules[1].clone(), vera_rules[0].clone()]);
        assert!(o.grants[&ivan.id].is_empty());
        assert_eq!(o.folders, [TeamFolder { name: "Ads".into(), color: "blue".into(), profiles: 1 }]);
        let mut profiles = o.profiles.clone();
        profiles.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            profiles,
            [
                TeamProfileRef { id: "p1".into(), name: "P1".into(), folder: "Ads".into() },
                TeamProfileRef { id: "p2".into(), name: "P2".into(), folder: "".into() },
            ]
        );

        // Her invite used: active from then on, while his is still waiting.
        t.as_member(&vera.token).get::<Value>("/me").await.unwrap();
        let o = overview(&t.id).await.unwrap();
        let seen = |id: &str| o.members.iter().find(|m| m.id == id).unwrap().last_seen_at;
        assert!(seen(&vera.id).is_some());
        assert_eq!(seen(&ivan.id), None);

        // The shape the page reads; "invited" is a null, not a missing field.
        let shown = serde_json::to_value(&o).unwrap();
        for key in ["team", "members", "folders", "profiles", "grants"] {
            assert!(shown.get(key).is_some(), "{key} missing from {shown}");
        }
        let row = shown["members"].as_array().unwrap().iter().find(|m| m["id"] == ivan.id.as_str()).unwrap();
        assert_eq!(row["last_seen_at"], Value::Null);
        assert_eq!(shown["grants"][&vera.id][0], json!({ "scope": { "kind": "team" }, "action": "see", "allow": true }));
    }

    #[tokio::test]
    async fn a_member_is_made_switched_off_given_a_new_token_and_removed() {
        let t = Team::new().await;
        let made = member_create(&t.id, "  Dana  ").await.unwrap();
        assert_eq!((made.member.name.as_str(), made.member.active, made.member.last_seen_at), ("Dana", true, None));
        let (server, token) = made.invite.split_once('#').unwrap();
        assert_eq!(server, t.srv.url);
        let me: Value = t.as_member(token).get("/me").await.unwrap();
        assert_eq!(me["member"]["id"], made.member.id.as_str());
        says(member_create(&t.id, " ").await, "a member needs a name");

        let id = made.member.id.clone();
        let off = member_set_active(&t.id, &id, false).await.unwrap();
        assert_eq!((off.id.as_str(), off.active), (id.as_str(), false));
        assert!(off.epoch > made.member.epoch);
        let err = t.as_member(token).get::<Value>("/profiles").await.unwrap_err();
        assert!(err.to_string().contains("switched off"), "{err}");
        assert!(member_set_active(&t.id, &id, true).await.unwrap().active);

        // A new token: the old one is told apart from a removal.
        assert!(overview(&t.id).await.unwrap().members[0].last_seen_at.is_some(), "the invite was used");
        let again = member_reissue(&t.id, &id).await.unwrap();
        // Invited again until the new one is used: the row says so at once.
        assert_eq!((again.member.id.as_str(), again.member.active, again.member.last_seen_at), (id.as_str(), true, None));
        let (server, new_token) = again.invite.split_once('#').unwrap();
        assert_eq!(server, t.srv.url);
        assert_ne!(new_token, token);
        let err = t.as_member(token).get::<Value>("/profiles").await.unwrap_err();
        assert!(err.token_replaced(), "{err}");
        assert!(err.to_string().contains("does not know this token"), "{err}");
        t.as_member(new_token).get::<Value>("/profiles").await.unwrap();

        member_delete(&t.id, &id).await.unwrap();
        let err = t.as_member(new_token).get::<Value>("/profiles").await.unwrap_err();
        assert_eq!(err.code(), Some(&Code::Unauthorized));
        assert!(err.to_string().contains("this route needs a token"), "{err}");
        // Gone: each of them says so as errors.ts's "no such thing".
        for r in [member_delete(&t.id, &id).await.map(drop), member_reissue(&t.id, &id).await.map(drop)] {
            says(r, "no such thing");
        }
        says(member_set_active(&t.id, &id, true).await, "no such thing");
        assert_eq!(member_reissue(&t.id, "nobody").await.err().unwrap().to_string(), "no such thing");
        // The admin's own launcher kept its team throughout.
        assert!(team::has_token(&t.id));
        assert!(!team::link(&t.id).unwrap().unwrap().needs_invite);
    }

    #[tokio::test]
    async fn rules_are_read_and_written_as_the_server_keeps_them() {
        let t = Team::new().await;
        let vera = t.add("Vera", &[]);
        assert!(grants(&t.id, &vera.id).await.unwrap().is_empty());
        let sent = [
            rule(folder("Ads"), Action::Edit, true),
            rule(Scope::Team, Action::See, true),
            rule(folder("Ads"), Action::Edit, false),
            rule(Scope::Team, Action::See, true),
        ];
        let kept = set_grants(&t.id, &vera.id, &sent).await.unwrap();
        // One row a cell, the refusal over the allowance, in the server's order.
        assert_eq!(kept, [rule(Scope::Team, Action::See, true), rule(folder("Ads"), Action::Edit, false)]);
        assert_eq!(grants(&t.id, &vera.id).await.unwrap(), kept);

        for who in ["nobody", team_model::ADMIN_ID, "a/b"] {
            says(grants(&t.id, who).await, "no such thing");
            says(set_grants(&t.id, who, &sent).await, "no such thing");
        }
    }

    #[tokio::test]
    async fn renaming_the_team_renames_it_here_too() {
        let t = Team::new().await;
        let (info, st) = rename(&t.id, "  Growth  ").await.unwrap();
        assert_eq!(info, TeamInfo { id: t.id.clone(), name: "Growth".into() });
        assert_eq!(st.teams[0].name, "Growth");
        assert_eq!(st.space, Space::Team { team_id: t.id.clone() });
        team::drop_cache();
        assert_eq!(team::link(&t.id).unwrap().unwrap().name, "Growth", "kept in teams.json");
        assert_eq!(overview(&t.id).await.unwrap().team.name, "Growth");

        let long = "x".repeat(65);
        for bad in ["   ", long.as_str()] {
            says(rename(&t.id, bad).await, "a team name is 1 to 64 characters");
        }
        assert_eq!(team::link(&t.id).unwrap().unwrap().name, "Growth");
    }

    #[tokio::test]
    async fn folders_are_made_renamed_recolored_and_removed() {
        let t = Team::new().await;
        // Every name reaches the server whole, whatever it holds.
        let odd = "Ads / EU #1 50% ?x=1 ..";
        let made = folder_create(&t.id, odd, Some("blue")).await.unwrap();
        assert_eq!(made, TeamFolder { name: odd.into(), color: "blue".into(), profiles: 0 });
        says(folder_create(&t.id, odd, None).await, "a folder of that name is already there");
        let long = "c".repeat(201);
        says(folder_create(&t.id, "Shops", Some(long.as_str())).await, "a name or label is too long");
        assert_eq!(folder_create(&t.id, "Shops", None).await.unwrap().color, "");

        let renamed = folder_rename(&t.id, odd, "Ads EU").await.unwrap();
        assert_eq!((renamed.name.as_str(), renamed.color.as_str()), ("Ads EU", "blue"));
        says(folder_rename(&t.id, odd, "Other").await, "no such thing");
        says(folder_rename(&t.id, "Ads EU", "Shops").await, "a folder of that name is already there");
        assert_eq!(folder_color(&t.id, "Ads EU", "green").await.unwrap().color, "green");
        says(folder_color(&t.id, "Nowhere", "green").await, "no such thing");

        t.make_profile("p1", "Ads EU").await;
        says(folder_delete(&t.id, "Ads EU").await, "the folder is not empty");
        folder_delete(&t.id, "Shops").await.unwrap();
        says(folder_delete(&t.id, "Shops").await, "no such thing");
        let left: Vec<String> = overview(&t.id).await.unwrap().folders.into_iter().map(|f| f.name).collect();
        assert_eq!(left, ["Ads EU"]);

        // A path drops "." and "..": such a folder could never be reached again.
        for dots in [".", ".."] {
            says(folder_create(&t.id, dots, None).await, "a folder cannot be named . or ..");
            says(folder_rename(&t.id, "Ads EU", dots).await, "a folder cannot be named . or ..");
            says(folder_rename(&t.id, dots, "Other").await, "no such thing");
            says(folder_color(&t.id, dots, "red").await, "no such thing");
            says(folder_delete(&t.id, dots).await, "no such thing");
            says(member_reissue(&t.id, dots).await, "no such thing");
        }
    }

    #[tokio::test]
    async fn only_the_admin_reaches_the_admin_routes() {
        let t = Team::new().await;
        let vera = t.add("Vera", &[rule(Scope::Team, Action::See, true)]);
        team::join(&t.srv.url, &vera.token).await.unwrap();
        let only = "only the team's admin can do that";
        says(overview(&t.id).await, only);
        says(member_create(&t.id, "Ivan").await, only);
        says(member_set_active(&t.id, &vera.id, false).await, only);
        says(member_delete(&t.id, &vera.id).await, only);
        says(member_reissue(&t.id, &vera.id).await, only);
        says(grants(&t.id, &vera.id).await, only);
        says(set_grants(&t.id, &vera.id, &[]).await, only);
        says(rename(&t.id, "Mine").await, only);
        says(folder_create(&t.id, "Ads", None).await, only);
        says(folder_rename(&t.id, "Ads", "B").await, only);
        says(folder_color(&t.id, "Ads", "red").await, only);
        says(folder_delete(&t.id, "Ads").await, only);
        says(overview("no-such-team").await, "not a member of that team");
        says(my_access("no-such-team").await, "not a member of that team");
        // Refused here, not by a 401 that would have to be confirmed.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(team::has_token(&t.id));
        assert!(!team::link(&t.id).unwrap().unwrap().revoked);
    }

    #[tokio::test]
    async fn a_member_reads_only_the_rules_about_what_they_see() {
        let t = Team::new().await;
        // The admin token holds every right on the whole team.
        let all: Vec<Grant> = Action::ALL.iter().map(|a| rule(Scope::Team, *a, true)).collect();
        assert_eq!(my_access(&t.id).await.unwrap(), all);

        t.make_profile("p1", "Ads").await;
        t.make_profile("p9", "Hidden").await;
        let vera = t.add(
            "Vera",
            &[
                rule(folder("Ads"), Action::Start, true),
                rule(folder("Ads"), Action::See, true),
                // About what she cannot see: left out of what she is shown.
                rule(Scope::Profile { id: "p9".into() }, Action::Start, true),
                rule(folder("Hidden"), Action::Cookies, true),
            ],
        );
        team::join(&t.srv.url, &vera.token).await.unwrap();
        let mine = my_access(&t.id).await.unwrap();
        assert_eq!(mine, [rule(folder("Ads"), Action::See, true), rule(folder("Ads"), Action::Start, true)]);
        assert_eq!(super::super::perm::held_epoch(&t.id), None, "read, not taken");

        let admin = t.as_member(&t.srv.admin_token);
        let path = format!("/admin/members/{}", vera.id);
        admin.call(Method::PATCH, &path, Some(&json!({ "active": false }))).await.unwrap();
        says(my_access(&t.id).await, "this member is switched off");
    }

    #[test]
    fn a_path_segment_carries_any_name() {
        assert_eq!(seg("Ads").unwrap(), "Ads");
        assert_eq!(seg("a b/c#d?e%f").unwrap(), "a%20b%2Fc%23d%3Fe%25f");
        assert_eq!(seg("Мои").unwrap(), "%D0%9C%D0%BE%D0%B8");
        assert_eq!(seg("..Ads.").unwrap(), "..Ads.");
        // A URL would drop these, and the request would land on another route.
        for dots in [".", ".."] {
            assert_eq!(seg(dots).unwrap_err().to_string(), "no such thing");
        }
        assert_eq!(invite("http://203.0.113.7:8787/", "stm_x"), "http://203.0.113.7:8787#stm_x");
    }

    #[test]
    fn an_address_only_this_computer_reaches_is_told_apart() {
        for (base, host) in [
            ("http://0.0.0.0:8787", Some("0.0.0.0")),
            ("http://127.0.0.1:8787", Some("127.0.0.1")),
            ("http://127.8.9.1:8787", Some("127.8.9.1")),
            ("http://[::]:8787", Some("[::]")),
            ("http://[::1]:8787", Some("[::1]")),
            ("http://LocalHost:8787", Some("localhost")),
            ("http://team.localhost", Some("team.localhost")),
            ("http://203.0.113.7:8787", None),
            ("http://192.168.1.20:8787", None),
            ("https://team.example.com", None),
        ] {
            assert_eq!(local_only(base).as_deref(), host, "{base}");
        }
    }

    #[tokio::test]
    async fn invites_start_with_the_address_the_owner_gave_the_server() {
        let root = crate::store::test_root("team-admin-url").with_config();
        let mut cfg = shardbrowser_team_server::Config::new(root.path.join("server"));
        cfg.url = Some(shardbrowser_team_server::PublicUrl::new(Some("https://team.example.com/"), "0.0.0.0:8787").unwrap());
        let srv = TestServer::start_with(cfg).await.unwrap();
        // Joined by an address only this computer reaches, as on the server's own machine.
        team::join(&srv.url, &srv.admin_token).await.unwrap();
        let id = srv.server.team_id().to_string();
        let made = member_create(&id, "Dana").await.unwrap();
        assert!(made.invite.starts_with("https://team.example.com#stm_"), "{}", made.invite);
        let again = member_reissue(&id, &made.member.id).await.unwrap();
        assert!(again.invite.starts_with("https://team.example.com#stm_"), "{}", again.invite);
    }

    fn ids(rows: Vec<TeamTrashRow>) -> Vec<String> {
        rows.into_iter().map(|r| r.id).collect()
    }

    #[tokio::test]
    async fn a_profile_comes_back_from_the_trash_with_its_session() {
        let t = Team::new().await;
        t.with_session("p1", "Ads", b"cookies of p1").await;
        t.delete("/profiles/p1").await;
        assert!(!super::super::cache::has_profile(&t.id, "p1"));

        let rows = trash_list(&t.id).await.unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.id.as_str(), r.kind.as_str(), r.name.as_str(), r.folder.as_str(), r.deleted_by.as_str()),
            ("p1", "profile", "P1", "Ads", team_model::ADMIN_ID)
        );
        assert_eq!(r.purge_at - r.deleted_at, 30 * 24 * 60 * 60, "kept 30 days");
        // The shape the page reads: a proxy's fields are there, empty.
        let shown = serde_json::to_value(r).unwrap();
        assert_eq!((shown["proxy_kind"].as_str(), shown["country"].as_str()), (Some(""), Some("")));

        let back = trash_restore(&t.id, "p1", &r.kind).await.unwrap();
        let want = TeamRestored { kind: "profile".into(), id: "p1".into(), name: "P1".into(), revision: r.revision + 1 };
        assert_eq!(back, want);
        assert!(trash_list(&t.id).await.unwrap().is_empty());
        let row: Value = t.as_member(&t.srv.admin_token).get("/profiles/p1").await.unwrap();
        assert_eq!((row["folder"].as_str(), row["session_rev"].as_u64()), (Some("Ads"), Some(1)));
        assert_eq!(t.session_of("p1").await, b"cookies of p1", "the session came back with it");
        assert!(super::super::cache::has_profile(&t.id, "p1"), "listed here again at once");
        assert_eq!(trash_restore(&t.id, "p1", "profile").await.unwrap_err().to_string(), "no such thing");
    }

    #[tokio::test]
    async fn a_purge_is_final() {
        let t = Team::new().await;
        t.with_session("p1", "Ads", b"old session").await;
        t.delete("/profiles/p1").await;
        let admin = t.as_member(&t.srv.admin_token);
        admin.call(Method::PUT, "/items/bookmark/b1", Some(&json!({ "name": "Docs", "data": "" }))).await.unwrap();
        t.delete("/items/bookmark/b1").await;
        let mut rows = trash_list(&t.id).await.unwrap();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        let kinds: Vec<(&str, &str)> = rows.iter().map(|r| (r.id.as_str(), r.kind.as_str())).collect();
        assert_eq!(kinds, [("b1", "bookmark"), ("p1", "profile")]);

        // The kind names the row: a bookmark is not found as a profile.
        assert_eq!(trash_purge(&t.id, "b1", "profile").await.unwrap_err().to_string(), "no such thing");
        trash_purge(&t.id, "p1", "profile").await.unwrap();
        trash_purge(&t.id, "b1", "bookmark").await.unwrap();
        assert!(trash_list(&t.id).await.unwrap().is_empty());
        for r in [trash_restore(&t.id, "p1", "profile").await.map(drop), trash_purge(&t.id, "p1", "profile").await] {
            assert_eq!(r.unwrap_err().to_string(), "no such thing");
        }
        assert_eq!(admin.get::<Value>("/profiles/p1").await.unwrap_err().to_string(), "no such thing");
        // The id is free again, and none of the old session comes with it.
        t.make_profile("p1", "Ads").await;
        let row: Value = admin.get("/profiles/p1").await.unwrap();
        assert_eq!(row["session_rev"].as_u64(), Some(0));
    }

    /// Tag names are one of a kind here and the server does not look: a tag whose name came
    /// back in the meantime stays in the trash, with the reason.
    #[tokio::test]
    async fn a_tag_comes_back_only_under_a_free_name() {
        let t = Team::new().await;
        let admin = t.as_member(&t.srv.admin_token);
        let tag = |name: &str| json!({ "name": name, "data": super::super::remote::item_data(&json!({ "color": "violet" })) });
        admin.call(Method::PUT, "/items/tag/t-aaa", Some(&tag("VIP"))).await.unwrap();
        t.delete("/items/tag/t-aaa").await;
        admin.call(Method::PUT, "/items/tag/t-bbb", Some(&tag("vip"))).await.unwrap();
        for kind in ["tag", ""] {
            says(trash_restore(&t.id, "t-aaa", kind).await, "a tag named vip already exists");
        }
        assert_eq!(ids(trash_list(&t.id).await.unwrap()), ["t-aaa"], "left in the trash");
        // Another kind of the same id is not held up by it.
        admin.call(Method::PUT, "/items/bookmark/t-aaa", Some(&json!({ "name": "VIP", "data": "" }))).await.unwrap();
        t.delete("/items/bookmark/t-aaa").await;
        assert_eq!(trash_restore(&t.id, "t-aaa", "bookmark").await.unwrap().kind, "bookmark");
        // Its name free again: back as it was.
        t.delete("/items/tag/t-bbb").await;
        let back = trash_restore(&t.id, "t-aaa", "tag").await.unwrap();
        assert_eq!((back.kind.as_str(), back.name.as_str()), ("tag", "VIP"));
        let names: Vec<String> = super::super::cache::tags(&t.id).tags.into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["VIP"]);
    }

    #[tokio::test]
    async fn the_server_judges_who_may_bring_back_or_purge() {
        let t = Team::new().await;
        t.make_profile("p1", "Ads").await;
        t.make_profile("p2", "Shops").await;
        t.delete("/profiles/p1").await;
        t.delete("/profiles/p2").await;
        let see_ads = rule(folder("Ads"), Action::See, true);
        let vera = t.add("Vera", &[see_ads.clone()]);
        let dana = t.add("Dana", &[see_ads, rule(folder("Ads"), Action::Delete, true)]);

        // Vera sees what was in Ads, and is refused in the server's own words.
        team::join(&t.srv.url, &vera.token).await.unwrap();
        assert_eq!(ids(trash_list(&t.id).await.unwrap()), ["p1"]);
        assert_eq!(trash_restore(&t.id, "p1", "profile").await.unwrap_err().to_string(), "not allowed");
        assert_eq!(trash_purge(&t.id, "p1", "profile").await.unwrap_err().to_string(), "not allowed");
        // What she could not see where it was is not there for her.
        assert_eq!(trash_restore(&t.id, "p2", "profile").await.unwrap_err().to_string(), "no such thing");
        // Her own trash commands are still not offered in a team.
        let tk = super::super::Ticket::now();
        assert_eq!(super::super::trash::restore(&tk, "p1").unwrap_err().to_string(), super::super::NOT_IN_TEAM);
        assert_eq!(super::super::trash::purge(&tk, "p1").unwrap_err().to_string(), super::super::NOT_IN_TEAM);
        assert!(super::super::trash::list().unwrap().is_empty());

        // Dana holds Delete on Ads: the server lets her, and so does this launcher.
        team::join(&t.srv.url, &dana.token).await.unwrap();
        assert_eq!(trash_restore(&t.id, "p1", "profile").await.unwrap().id, "p1");
        team::join(&t.srv.url, &t.srv.admin_token).await.unwrap();
        assert_eq!(ids(trash_list(&t.id).await.unwrap()), ["p2"]);
        says(trash_list("no-such-team").await, "not a member of that team");
    }
}
