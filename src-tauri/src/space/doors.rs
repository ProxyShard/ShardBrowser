//! Every door into the launcher — each Tauri command and each local API route —
//! and what it needs in a team space; [`clear`] enforces a write's rule there.

#[cfg(test)]
use std::collections::BTreeSet;

use team_model::{Action, Target};

use super::error::{CmdError, Need as Wanted};
use super::perm;
use Need::*;
use On::*;
use Right::*;
use When::*;

/// What a right is held on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum On {
    /// The profile the door acts on; each of them, for a group.
    Profile,
    /// The folder a profile is made in or moved to.
    Folder,
    /// The team itself: its proxy list, bookmarks, extensions and projects.
    Team,
    /// The proxy the door acts on or binds.
    Proxy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Right {
    See,
    Create,
    Edit,
    Delete,
    Start,
    Cookies,
    TakeOut,
    Admin,
}

/// When a right applies to a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum When {
    Always,
    /// It makes something new.
    New,
    /// It changes something that exists.
    Existing,
    /// It binds a proxy by id.
    BindsProxy,
    /// It stores a proxy given as a line.
    InlineProxy,
    /// It moves the profile to another folder (Create is on the destination).
    MovesFolder,
    /// It changes the profile's extensions.
    ChangesExtensions,
    /// The profile is not open on this device, so it is opened briefly.
    NotOpen,
    /// The answer carries a running profile's CDP endpoint.
    ShowsCdp,
    /// The folder's profiles go with it.
    DeletesProfiles,
    /// Each profile the run starts.
    EachStart,
}

#[derive(Debug)]
enum Need {
    /// Machine level (settings, the engine, the billing account): never checked.
    Machine,
    /// Own sessions and local marks: nothing to check, stopping is always allowed.
    Nothing,
    /// Each right whose condition the request meets.
    Rights(&'static [(When, Right, On)]),
    /// The CDP pass kept with the running profile, judged again at each use (`space::cdp_pass`).
    CdpPass,
    /// Not offered in a team space.
    Off,
    /// Sent with this launcher's token to the team's server, which judges each request
    /// and refuses in its own words.
    Server,
    /// Refused while a team profile is open on this device (`open::refuse_while_open`).
    NoTeamSessionOpen,
}

const SEE_P: Need = Rights(&[(Always, See, Profile)]);
const CREATE_F: Need = Rights(&[
    (Always, Create, Folder),
    (BindsProxy, See, Proxy),
    (ChangesExtensions, Cookies, Profile),
]);
const CREATE_F_API: Need = Rights(&[
    (Always, Create, Folder),
    (BindsProxy, See, Proxy),
    (InlineProxy, Create, Team),
    (ChangesExtensions, Cookies, Profile),
]);
const DELETE_P: Need = Rights(&[(Always, Delete, Profile)]);
const START: Need = Rights(&[(Always, Start, Profile)]);
const START_CDP: Need = Rights(&[(Always, Start, Profile), (Always, Cookies, Profile)]);
const COOKIES: Need = Rights(&[(Always, Cookies, Profile), (NotOpen, Start, Profile)]);
/// Judged per row by `space::running`.
const RUNNING: Need = Rights(&[(ShowsCdp, Cookies, Profile)]);
const RUN: Need = Rights(&[
    (Always, See, Team),
    (EachStart, Start, Profile),
    (EachStart, Cookies, Profile),
]);
const EDIT_P: Need = Rights(&[
    (Always, Edit, Profile),
    (MovesFolder, Create, Folder),
    (BindsProxy, See, Proxy),
    (InlineProxy, Create, Team),
    (ChangesExtensions, Cookies, Profile),
]);
const ADMIN: Need = Rights(&[(Always, Admin, Team)]);
const FOLDER_DELETE: Need = Rights(&[(Always, Admin, Team), (DeletesProfiles, Delete, Profile)]);
const SEE_X: Need = Rights(&[(Always, See, Proxy)]);
const CREATE_T: Need = Rights(&[(Always, Create, Team)]);
const SEE_T: Need = Rights(&[(Always, See, Team)]);
const EDIT_T: Need = Rights(&[(Always, Edit, Team)]);
const DELETE_T: Need = Rights(&[(Always, Delete, Team)]);
const UPSERT_T: Need = Rights(&[(New, Create, Team), (Existing, Edit, Team)]);
const TAKE_OUT: Need = Rights(&[(Always, TakeOut, Team)]);

/// Tauri commands by their name in `generate_handler!`, routes as "METHOD /path".
const RULES: &[(&str, Need)] = &[
    // Profiles.
    ("profile_list", SEE_P),
    ("profile_get", SEE_P),
    // A password-protected profile is this machine's own: no team profile has one (#127).
    ("profile_protect", Off),
    ("profile_unprotect", Off),
    ("profile_change_password", Off),
    ("profile_unlock", Off),
    (
        "profile_save",
        Rights(&[
            (New, Create, Folder),
            (Existing, Edit, Profile),
            (MovesFolder, Create, Folder),
            (BindsProxy, See, Proxy),
            (ChangesExtensions, Cookies, Profile),
        ]),
    ),
    ("profile_create_from_template", CREATE_F),
    ("profile_import", CREATE_F),
    (
        "profile_clone",
        Rights(&[(Always, Create, Folder), (Always, See, Profile), (BindsProxy, See, Proxy)]),
    ),
    (
        "profile_bind_proxy",
        Rights(&[(Always, Edit, Profile), (BindsProxy, See, Proxy)]),
    ),
    (
        "profile_set_folder",
        Rights(&[(Always, Edit, Profile), (Always, Create, Folder)]),
    ),
    // Past the trash in a team.
    ("profile_delete", DELETE_P),
    ("profile_set_pin", Nothing),
    ("folder_rename", ADMIN),
    ("folder_delete", FOLDER_DELETE),
    ("folder_list", SEE_P),
    // Keeping an empty folder makes a team folder; its look is the team's too.
    ("folder_add", ADMIN),
    ("folder_meta_set", ADMIN),
    // The interface's old copy belongs to the personal space only.
    ("folder_import", Off),
    ("profile_focus_window", Nothing),
    // Starting and running.
    ("launch", START),
    ("sync_launch", START),
    ("launch_cancel", Nothing),
    ("process_list", RUNNING),
    ("process_kill", Nothing),
    ("team_sessions", Nothing),
    ("sync_status", Nothing),
    ("sync_set_paused", Nothing),
    ("sync_arrange", Nothing),
    ("sync_stop", Nothing),
    ("sync_set_excluded", Nothing),
    ("sync_close_panel", Nothing),
    ("helper_profiles", Nothing),
    ("helper_fields", Nothing),
    ("helper_fill", Nothing),
    ("helper_show", Nothing),
    ("helper_close", Nothing),
    ("helper_dismiss", Nothing),
    // Cookies.
    ("cookies_export", COOKIES),
    ("cookies_export_to_file", COOKIES),
    ("cookies_import", COOKIES),
    // Automation: projects, runs, the attached browser.
    ("automation_available", Machine),
    ("automation_list", SEE_T),
    ("automation_create", CREATE_T),
    ("automation_save", EDIT_T),
    ("automation_delete", DELETE_T),
    ("automation_duplicate", CREATE_T),
    ("automation_import", CREATE_T),
    ("automation_export", TAKE_OUT),
    ("automation_launch", START_CDP),
    ("automation_attach", CdpPass),
    ("automation_screencast", CdpPass),
    ("automation_call", CdpPass),
    ("automation_pick", CdpPass),
    ("automation_detach", Nothing),
    ("automation_attached", Nothing),
    ("automation_run", RUN),
    ("automation_run_stop", Nothing),
    ("automation_run_status", Nothing),
    ("automation_fleet", Nothing),
    ("automation_fleet_window", Nothing),
    ("automation_display", Machine),
    ("automation_tls_fingerprints", Machine),
    ("automation_modules", Machine),
    ("automation_module_install", Machine),
    ("automation_module_remove", Machine),
    ("automation_module_permissions", Machine),
    ("automation_module_grant", Machine),
    ("automation_modules_dir", Machine),
    // Trash.
    ("trash_list", Off),
    ("trash_restore", Off),
    ("trash_purge", Off),
    ("trash_empty", Off),
    // Extensions and bookmarks: the team's, judged on the whole team.
    ("extension_list", SEE_T),
    ("extension_import", CREATE_T),
    ("extension_import_url", CREATE_T),
    ("extension_delete", DELETE_T),
    // Updates from the Web Store: the library's, this machine's own in v1.
    ("extension_updates", SEE_T),
    ("extension_updates_check", SEE_T),
    ("extension_update", EDIT_T),
    ("extension_link_source", EDIT_T),
    ("extension_update_cancel", Machine),
    ("extension_update_skip", Machine),
    ("bookmark_list", SEE_T),
    ("bookmark_save", UPSERT_T),
    ("bookmark_delete", DELETE_T),
    ("bookmark_delete_many", DELETE_T),
    // Tags are the team's; which profile carries one is the profile's.
    ("tag_list", SEE_T),
    ("tag_save", UPSERT_T),
    ("tag_delete", DELETE_T),
    ("tag_assign", Rights(&[(Always, Edit, Profile)])),
    ("tag_import", Off),
    // Proxies.
    ("proxy_list", SEE_X),
    (
        "proxy_save",
        Rights(&[(New, Create, Team), (Existing, Edit, Proxy)]),
    ),
    ("proxy_delete", Rights(&[(Always, Delete, Proxy)])),
    ("proxy_check", SEE_X),
    ("proxy_check_udp", SEE_X),
    ("proxy_geo", SEE_X),
    ("proxy_full_test", SEE_X),
    ("proxy_history", SEE_X),
    ("proxy_last_test", SEE_X),
    ("proxy_bulk_import", CREATE_T),
    ("proxy_bulk_save", CREATE_T),
    ("proxy_bulk_parse", Machine),
    ("ps_import_order", CREATE_T),
    // Data folder and reset.
    ("data_root_get", Machine),
    ("data_root_migrate", NoTeamSessionOpen),
    ("reset::app_reset_plan", NoTeamSessionOpen),
    ("reset::app_reset", NoTeamSessionOpen),
    ("reset::app_reset_backup", Machine),
    ("reset::app_reset_keep_storage", Machine),
    // Only while a reset is unfinished, when nothing starts or opens.
    ("reset::app_reset_pending", Machine),
    ("reset::app_reset_undo", Machine),
    ("reset::app_reset_finish", Machine),
    ("reset::app_reset_set_aside", Machine),
    // The exit closes the team profiles first.
    ("reset::app_restart", Machine),
    // Machine level.
    ("clipboard_write", Machine),
    ("clipboard_read", Machine),
    ("host_platform", Machine),
    ("host_screen", Machine),
    ("host_hardware", Machine),
    ("enrich_picks_for_preset", Machine),
    ("fingerprint_list", Machine),
    ("fingerprint_get", Machine),
    ("fingerprint_import", Machine),
    ("fingerprint_delete", Machine),
    ("fingerprint_dir", Machine),
    ("gpu_caps", Machine),
    ("gpu_caps_compat", Machine),
    ("read_text_file", Machine),
    ("settings_get", Machine),
    ("settings_save", Machine),
    ("settings_load_error", Machine),
    ("api_info", Machine),
    ("api_regenerate_token", Machine),
    ("ps_get_key", Machine),
    ("ps_set_key", Machine),
    ("ps_me", Machine),
    ("ps_orders", Machine),
    ("ps_order", Machine),
    ("ps_active", Machine),
    ("ps_products", Machine),
    ("ps_trial_offer", Machine),
    ("ps_available_count", Machine),
    ("ps_resi_isps", Machine),
    ("ps_calculate", Machine),
    ("ps_purchase", Machine),
    ("ps_add_bandwidth", Machine),
    ("ps_profile_traffic", Machine),
    ("ps_usage", Machine),
    ("ps_renew", Machine),
    ("ps_set_tag", Machine),
    ("ps_countries", Machine),
    ("ps_regions", Machine),
    ("ps_cities", Machine),
    ("ps_signature_set", Machine),
    ("space_state", Machine),
    ("space_set", Machine),
    ("team_join", Machine),
    ("team_leave", Machine),
    // The Team page: the admin's, sent with the admin token.
    ("team_admin_overview", ADMIN),
    ("team_admin_member_create", ADMIN),
    ("team_admin_member_set_active", ADMIN),
    ("team_admin_member_delete", ADMIN),
    ("team_admin_member_reissue", ADMIN),
    ("team_admin_grants", ADMIN),
    ("team_admin_set_grants", ADMIN),
    ("team_admin_rename", ADMIN),
    ("team_admin_folder_create", ADMIN),
    ("team_admin_folder_rename", ADMIN),
    ("team_admin_folder_color", ADMIN),
    ("team_admin_folder_delete", ADMIN),
    // The team's trash: See where a row was to list it, See and Delete there to restore or purge.
    ("team_trash_list", Server),
    ("team_trash_restore", Server),
    ("team_trash_purge", Server),
    // A member's own rules: theirs to read, whatever they hold.
    ("team_my_access", Nothing),
    // Personal sync: this machine's connection to the person's own space.
    ("sync_state", Machine),
    ("sync_connect", Machine),
    ("sync_disconnect", Machine),
    ("sync_set_kinds", Machine),
    ("sync_now", Machine),
    ("sync_invite", Machine),
    ("sync_set_device_name", Machine),
    // A personal profile's sync: no team row has one.
    ("profile_sync_set", Off),
    ("profile_sync_take", Off),
    ("mcp_download", Machine),
    ("runtime::runtime_status", Machine),
    ("runtime::runtime_install", Machine),
    ("runtime::launcher_update_check", Machine),
    ("app_quit_asking", Machine),
    ("app_quit_confirmed", Machine),
    ("app_quit_cancelled", Machine),
    // Local API.
    ("GET /health", Machine),
    ("GET /profiles", SEE_P),
    ("POST /profiles", CREATE_F_API),
    ("POST /folders/:folder/profiles", CREATE_F_API),
    // Lives only on this device while open; never written to the server.
    ("POST /profiles/temporary", CREATE_T),
    ("GET /profiles/:id", SEE_P),
    ("PATCH /profiles/:id", EDIT_P),
    ("DELETE /profiles/:id", DELETE_P),
    ("POST /profiles/:id/start", START_CDP),
    ("POST /profiles/:id/stop", Nothing),
    ("GET /profiles/:id/cookies", COOKIES),
    ("POST /profiles/:id/cookies", COOKIES),
    ("GET /folders", SEE_P),
    ("PATCH /folders/:folder", ADMIN),
    ("DELETE /folders/:folder", FOLDER_DELETE),
    ("GET /fingerprint/new", Machine),
    ("GET /fingerprint/new/:platform", Machine),
    ("GET /fingerprints", Machine),
    ("GET /running", RUNNING),
    ("GET /proxies", SEE_X),
    ("POST /proxies", CREATE_T),
    ("DELETE /proxies/:id", Rights(&[(Always, Delete, Proxy)])),
    ("GET /extensions", SEE_T),
    ("POST /extensions", CREATE_T),
    ("DELETE /extensions/:id", DELETE_T),
    ("GET /extensions/updates", SEE_T),
    ("POST /extensions/updates/check", SEE_T),
    ("POST /extensions/updates/apply", EDIT_T),
    ("GET /bookmarks", SEE_T),
    ("POST /bookmarks", UPSERT_T),
    ("DELETE /bookmarks/:id", DELETE_T),
    ("GET /trash", Off),
    ("DELETE /trash/:id", Off),
    ("POST /trash/:id/restore", Off),
    ("GET /automation/projects", SEE_T),
    ("POST /automation/projects", CREATE_T),
    ("GET /automation/projects/:id", SEE_T),
    ("PUT /automation/projects/:id", EDIT_T),
    ("DELETE /automation/projects/:id", DELETE_T),
    ("POST /automation/projects/:id/duplicate", CREATE_T),
    ("GET /automation/projects/:id/export", TAKE_OUT),
    ("POST /automation/projects/:id/run", RUN),
    ("POST /automation/projects/:id/stop", Nothing),
    ("GET /automation/projects/:id/status", Nothing),
    ("POST /automation/import", CREATE_T),
    ("GET /automation/runs", Nothing),
    ("GET /automation/modules", Machine),
    ("POST /automation/modules", Machine),
    ("DELETE /automation/modules/:id", Machine),
];

/// The doors whose rule is enforced here in a team, through [`clear`]. Each
/// reaches the space's writes as `Door("name")`; the rest are still a table.
pub const ENFORCED: &[&str] = &[
    "profile_save",
    "profile_create_from_template",
    "profile_import",
    "profile_clone",
    "profile_bind_proxy",
    "profile_set_folder",
    "profile_delete",
    "folder_rename",
    "folder_delete",
    "folder_add",
    "folder_meta_set",
    "tag_assign",
    "tag_save",
    "tag_delete",
    "bookmark_save",
    "bookmark_delete",
    "bookmark_delete_many",
    "extension_import",
    "extension_import_url",
    "extension_delete",
    "extension_update",
    "extension_link_source",
    "automation_create",
    "automation_save",
    "automation_delete",
    "automation_duplicate",
    "automation_import",
    "proxy_save",
    "proxy_delete",
    "proxy_bulk_import",
    "proxy_bulk_save",
    "ps_import_order",
    "POST /profiles",
    "POST /folders/:folder/profiles",
    "PATCH /profiles/:id",
    "DELETE /profiles/:id",
    "PATCH /folders/:folder",
    "DELETE /folders/:folder",
    "POST /proxies",
    "DELETE /proxies/:id",
    "POST /extensions",
    "DELETE /extensions/:id",
    "POST /extensions/updates/apply",
    "POST /bookmarks",
    "DELETE /bookmarks/:id",
    "POST /automation/projects",
    "PUT /automation/projects/:id",
    "DELETE /automation/projects/:id",
    "POST /automation/projects/:id/duplicate",
    "POST /automation/import",
];

/// A door by its name in [`RULES`]: a write cannot reach a team without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Door(pub &'static str);

/// What a request asks of its door, for the conditions in the door's rule.
#[derive(Debug, Default, Clone)]
pub struct Ask<'a> {
    /// The profile acted on, and the folder it is in now; it must be seen.
    pub profile: Option<(&'a str, &'a str)>,
    /// A profile being made, and the folder it lands in.
    pub made: Option<(&'a str, &'a str)>,
    /// The proxy acted on; it must be seen.
    pub proxy: Option<&'a str>,
    /// The folder a profile is made in or moved to.
    pub folder: Option<&'a str>,
    pub new: bool,
    pub moves_folder: bool,
    /// The proxy a profile gets bound to, when the binding changes.
    pub binds_proxy: Option<&'a str>,
    pub inline_proxy: bool,
    pub changes_extensions: bool,
    pub deletes_profiles: bool,
}

impl Ask<'_> {
    /// Whether the condition holds; None for one only a start or a run judges.
    fn holds(&self, when: When) -> Option<bool> {
        Some(match when {
            Always => true,
            New => self.new,
            Existing => !self.new,
            BindsProxy => self.binds_proxy.is_some(),
            InlineProxy => self.inline_proxy,
            MovesFolder => self.moves_folder,
            ChangesExtensions => self.changes_extensions,
            DeletesProfiles => self.deletes_profiles,
            NotOpen | ShowsCdp | EachStart => return None,
        })
    }
}

fn action(r: Right) -> Option<Action> {
    Some(match r {
        See => Action::See,
        Create => Action::Create,
        Edit => Action::Edit,
        Delete => Action::Delete,
        Start => Action::Start,
        Cookies => Action::Cookies,
        TakeOut => Action::TakeOut,
        Admin => return None,
    })
}

fn on_name(on: On) -> &'static str {
    match on {
        Profile => "profile",
        Folder => "folder",
        Team => "team",
        Proxy => "proxy",
    }
}

fn proxy_target(id: &str) -> Target<'_> {
    Target { profile_id: Some(id), folder: None }
}

/// The door's rule, enforced on the team in use: every right whose condition
/// the request meets. Something the member may not See is missing, not refused.
pub fn clear(door: Door, team: &str, ask: &Ask<'_>) -> Result<(), CmdError> {
    let broken = |why: &str| CmdError::Refused { status: 500, code: "internal".into(), message: format!("{}: {why}", door.0) };
    // Only the doors listed as enforced write to a team at all.
    if !ENFORCED.contains(&door.0) {
        return Err(broken("not a door that writes to a team"));
    }
    let Some((_, need)) = RULES.iter().find(|(d, _)| *d == door.0) else {
        return Err(broken("no rule for this door"));
    };
    let Rights(list) = need else { return Err(broken("enforced without a rule of rights")) };
    if !perm::usable(team) {
        return Err(perm::no_permission());
    }
    let may = |a: Action, t: &Target<'_>| perm::allows(team, a, t);
    if let Some((id, folder)) = ask.profile {
        if !may(Action::See, &Target::profile(id, folder)) {
            return Err(CmdError::Missing(format!("no profile {id} in this team")));
        }
    }
    if let Some(id) = ask.proxy {
        if !may(Action::See, &proxy_target(id)) {
            return Err(CmdError::Missing(format!("no proxy {id} in this team")));
        }
    }
    for &(when, right, on) in *list {
        match ask.holds(when) {
            Some(true) => {}
            Some(false) => continue,
            None => return Err(broken("a condition not judged here")),
        }
        let allowed = match (action(right), on) {
            (None, _) => crate::team::link(team).ok().flatten().is_some_and(|l| l.admin),
            (Some(a), Profile) => {
                let Some((id, folder)) = ask.profile.or(ask.made) else { return Err(broken("names no profile")) };
                may(a, &Target::profile(id, folder))
            }
            (Some(a), Folder) => {
                let Some(f) = ask.folder else { return Err(broken("names no folder")) };
                may(a, &Target::folder(f))
            }
            (Some(a), Team) => may(a, &Target::team()),
            (Some(a), Proxy) => {
                let id = if when == BindsProxy { ask.binds_proxy } else { ask.proxy };
                let Some(id) = id else { return Err(broken("names no proxy")) };
                may(a, &proxy_target(id))
            }
        };
        if !allowed {
            let right = action(right).map_or("admin", perm::action_name);
            return Err(CmdError::Forbidden(Wanted { right, on: on_name(on) }));
        }
    }
    Ok(())
}

/// Every command in `generate_handler![...]`, as written there.
#[cfg(test)]
fn commands(lib: &str) -> BTreeSet<String> {
    let open = "generate_handler![";
    let start = lib.find(open).expect("lib.rs registers its commands") + open.len();
    let end = start + lib[start..].find(']').expect("the handler list ends");
    lib[start..end]
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .flat_map(|l| l.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Every method on every `.route(...)`, as "GET /profiles/:id".
#[cfg(test)]
pub(crate) fn routes(api: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = api;
    while let Some(at) = rest.find(".route(") {
        let args = &rest[at + ".route(".len()..];
        let mut depth = 1;
        let mut end = args.len();
        for (i, c) in args.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let call = &args[..end];
        let path = call.split('"').nth(1).expect("a route names its path");
        for method in ["get", "post", "put", "patch", "delete"] {
            let needle = format!("{method}(");
            let mut from = 0;
            while let Some(k) = call[from..].find(&needle) {
                let i = from + k;
                let joined = call[..i]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
                if !joined {
                    out.insert(format!("{} {path}", method.to_uppercase()));
                }
                from = i + needle.len();
            }
        }
        rest = &args[end..];
    }
    out
}

/// Whether a door is open to anyone in a team: machine level, or own sessions.
#[cfg(test)]
pub(crate) fn is_open(door: &str) -> bool {
    RULES.iter().any(|(d, n)| *d == door && matches!(n, Machine | Nothing))
}

/// What is wrong with a rule, if anything.
#[cfg(test)]
fn fault(need: &Need) -> Option<String> {
    let Rights(list) = need else { return None };
    if list.is_empty() {
        return Some("no rights listed".into());
    }
    let has = |w: When| list.iter().any(|&(when, _, _)| when == w);
    if has(New) != has(Existing) {
        return Some("names one of New and Existing without the other".into());
    }
    for &(_, right, on) in *list {
        if matches!(right, Admin | TakeOut) && on != Team {
            return Some(format!("{right:?} is held on the team, not on {on:?}"));
        }
    }
    None
}

#[cfg(test)]
#[test]
fn every_door_is_classified() {
    let doors: BTreeSet<String> = commands(include_str!("../lib.rs"))
        .into_iter()
        .chain(routes(include_str!("../api.rs")))
        .collect();
    let ruled: BTreeSet<String> = RULES.iter().map(|(d, _)| d.to_string()).collect();
    assert_eq!(ruled.len(), RULES.len(), "a door is listed twice in RULES");

    let unclassified: Vec<&String> = doors.difference(&ruled).collect();
    let stale: Vec<&String> = ruled.difference(&doors).collect();
    assert!(
        unclassified.is_empty() && stale.is_empty(),
        "every door needs a rule in space/doors.rs\n  no rule: {unclassified:?}\n  no such door: {stale:?}"
    );

    for (door, need) in RULES {
        if let Some(why) = fault(need) {
            panic!("{door}: {why} ({need:?})");
        }
    }
}

/// Each enforced door has a rule of rights, and the code reaches the space's
/// writes through it; every `Door("…")` the code names is one of them.
#[cfg(test)]
#[test]
fn every_enforced_door_is_ruled_and_reached() {
    let mut named = BTreeSet::new();
    for src in [include_str!("../lib.rs"), include_str!("../api.rs"), include_str!("../psapi.rs")] {
        let mut rest = src;
        while let Some(at) = rest.find("Door(\"") {
            let tail = &rest[at + 6..];
            let end = tail.find('"').expect("a door's name ends");
            named.insert(tail[..end].to_string());
            rest = &tail[end..];
        }
    }
    for door in ENFORCED {
        let rule = RULES.iter().find(|(d, _)| d == door).map(|(_, n)| n);
        assert!(matches!(rule, Some(Rights(_))), "{door}: enforced without a rule of rights");
        assert!(named.contains(*door), "{door}: enforced, but no write goes through it");
    }
    let stray: Vec<&String> = named.iter().filter(|d| !ENFORCED.contains(&d.as_str())).collect();
    assert!(stray.is_empty(), "named as a door but not enforced: {stray:?}");
}

/// Doors judged outside [`clear`], with the call each handler makes to judge them.
#[cfg(test)]
const GUARDED: &[(&str, &str, &str)] = &[
    ("process_list", "fn process_list(", "space::running()"),
    ("GET /running", "fn list_running(", "space::running()"),
    ("automation_attach", "fn automation_attach(", "space::cdp_pass("),
    ("automation_screencast", "fn automation_screencast(", "space::cdp_pass("),
    ("automation_call", "fn automation_call(", "space::cdp_pass("),
    ("automation_pick", "fn automation_pick(", "space::cdp_pass("),
    ("data_root_migrate", "fn run_with(", "refuse_while_busy()"),
    ("reset::app_reset_plan", "fn app_reset_plan(", "refuse_while_open()"),
    ("reset::app_reset", "fn app_reset(", "refuse_while_open()"),
];

/// Each door whose rule [`clear`] does not judge calls its own guard.
#[cfg(test)]
#[test]
fn every_door_judged_elsewhere_calls_its_guard() {
    let srcs = [
        include_str!("../lib.rs"),
        include_str!("../api.rs"),
        include_str!("../reset.rs"),
        include_str!("../migrate.rs"),
    ];
    let body = |f: &str| -> &str {
        let src = srcs.iter().find(|s| s.contains(f)).unwrap_or_else(|| panic!("no {f}"));
        let at = src.find(f).unwrap();
        let end = src[at..].find("\n}\n").map_or(src.len(), |e| at + e);
        &src[at..end]
    };
    let guarded: BTreeSet<&str> = GUARDED.iter().map(|(d, _, _)| *d).collect();
    let ruled: BTreeSet<&str> = RULES
        .iter()
        .filter(|(d, n)| match n {
            CdpPass | NoTeamSessionOpen => true,
            Rights(list) => !ENFORCED.contains(d) && list.iter().any(|&(w, _, _)| w == ShowsCdp),
            _ => false,
        })
        .map(|(d, _)| *d)
        .collect();
    assert_eq!(guarded, ruled, "a door judged outside clear() needs its guard listed");
    for (door, f, guard) in GUARDED {
        assert!(body(f).contains(guard), "{door}: {f} does not call {guard}");
    }
}

/// The parsers must see the real lists, or an empty RULES would pass.
#[cfg(test)]
#[test]
fn the_door_lists_are_read_whole() {
    let cmds = commands(include_str!("../lib.rs"));
    assert!(
        cmds.contains("profile_list") && cmds.contains("reset::app_restart"),
        "{cmds:?}"
    );
    let routes = routes(include_str!("../api.rs"));
    for door in [
        "GET /health",
        "PATCH /profiles/:id",
        "DELETE /automation/projects/:id",
    ] {
        assert!(routes.contains(door), "{door} not found in {routes:?}");
    }
}

/// Every window write that names its space takes the window's ticket, which a
/// team refuses without the epoch; none asks `space::ticket` and passes.
#[cfg(test)]
#[test]
fn every_window_write_with_an_epoch_takes_the_windows_ticket() {
    let lib = include_str!("../lib.rs");
    let mut named = Vec::new();
    for (at, _) in lib.match_indices("#[tauri::command]") {
        let rest = &lib[at..];
        let open = rest.find('{').expect("a command has a body");
        let (sig, body) = (&rest[..open], &rest[open..]);
        if !sig.contains("epoch: Option<u64>") {
            continue;
        }
        let name = sig.split("fn ").nth(1).and_then(|s| s.split('(').next()).unwrap_or("?").trim();
        let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
        assert!(body.contains("ticket(epoch)"), "{name}: takes epoch but no ticket from it");
        assert!(!body.contains("space::ticket(epoch)"), "{name}: a team write without its epoch would pass");
        named.push(name.to_string());
    }
    for door in ["profile_save", "profile_delete", "tag_save", "proxy_save"] {
        assert!(named.iter().any(|n| n == door), "{door} not found in {named:?}");
    }
}
