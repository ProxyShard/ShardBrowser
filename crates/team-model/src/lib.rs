//! Who may do what inside a team. The server and the launcher both call
//! [`decide`] from here, so they never disagree about which grant wins.

use serde::{Deserialize, Serialize};

/// Who the admin token acts as on member routes: every right, its own personal
/// space; leases, permissions and `updated_by` carry this id. No member has it.
pub const ADMIN_ID: &str = "admin";

/// What a member may be allowed to do. Kept short on purpose: every action
/// here has to be enforced somewhere in the launcher, and a right nobody
/// checks is worse than no right at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// The item exists for this member at all. Without it the profile is not
    /// listed, cannot be searched, and its folder may not even be shown.
    See,
    /// Open the browser on it.
    Start,
    /// Change its settings: name, fingerprint, proxy binding, notes.
    Edit,
    /// Make new profiles and proxies **in the team**. A team-level action:
    /// scoped to a folder it means "new items land in that folder".
    Create,
    /// Move to the team's trash, and restore or purge from it.
    Delete,
    /// Read or write the browsing session: cookie export and import. Separate
    /// from Edit because this is the one that carries a live account out of
    /// the team.
    Cookies,
    /// Take an item out of the team space — into a personal space or a file.
    /// Separate from Delete: losing a profile and walking off with it are
    /// different events.
    TakeOut,
}

impl Action {
    /// Every action: for granting or checking the lot.
    pub const ALL: [Action; 7] = [
        Action::See,
        Action::Start,
        Action::Edit,
        Action::Create,
        Action::Delete,
        Action::Cookies,
        Action::TakeOut,
    ];

    /// Actions that say nothing about one profile — they are about the team or
    /// a folder in it. A grant naming a single profile cannot carry them.
    pub fn is_team_level(self) -> bool {
        matches!(self, Action::Create)
    }
}

/// What a grant is about. Narrower beats wider when the two disagree, so a
/// team can be given a folder and then have one profile in it taken away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Everything in the team.
    Team,
    /// One folder of the team, by name — the same name the launcher shows.
    Folder { name: String },
    /// One profile, by id.
    Profile { id: String },
}

impl Scope {
    /// How specific this scope is. Higher wins when two grants both match.
    fn specificity(&self) -> u8 {
        match self {
            Scope::Team => 0,
            Scope::Folder { .. } => 1,
            Scope::Profile { .. } => 2,
        }
    }
}

/// One line of the permission table: this member, this scope, this action —
/// allowed or refused. A refusal is stored, not implied, so "the whole folder
/// except this one profile" is one row rather than a list of everything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub scope: Scope,
    pub action: Action,
    pub allow: bool,
}

/// The decision for one (profile, action) pair, with the grant that made it —
/// the launcher shows the reason when it refuses, and "no rule" reads very
/// differently from "refused on purpose".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed { by: Scope },
    Refused { by: Scope },
    NoRule,
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allowed { .. })
    }
}

/// What the member is being asked about. A team-level action (Create) carries
/// no profile; a folder may still narrow it.
#[derive(Debug, Clone)]
pub struct Target<'a> {
    pub profile_id: Option<&'a str>,
    pub folder: Option<&'a str>,
}

impl<'a> Target<'a> {
    pub fn profile(id: &'a str, folder: &'a str) -> Self {
        Target { profile_id: Some(id), folder: Some(folder) }
    }
    pub fn folder(name: &'a str) -> Self {
        Target { profile_id: None, folder: Some(name) }
    }
    pub fn team() -> Self {
        Target { profile_id: None, folder: None }
    }
}

/// Decide one action against a member's grants.
///
/// The rule is: among the grants that match, the narrowest one decides, and a
/// refusal at that width beats an allowance at the same width. Nothing
/// matching is not an allowance — a member starts with nothing and is given
/// what they need.
pub fn decide(grants: &[Grant], action: Action, target: &Target<'_>) -> Decision {
    let mut best: Option<&Grant> = None;
    for g in grants.iter().filter(|g| g.action == action) {
        let matches = match &g.scope {
            Scope::Team => true,
            Scope::Folder { name } => target.folder.is_some_and(|f| f == name),
            Scope::Profile { id } => target.profile_id.is_some_and(|p| p == id),
        };
        if !matches {
            continue;
        }
        // A grant naming one profile cannot carry a team-level action: it
        // would read as "may create inside this profile", which is nothing.
        if action.is_team_level() && matches!(g.scope, Scope::Profile { .. }) {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let (new, old) = (g.scope.specificity(), b.scope.specificity());
                // Same width and they disagree: the refusal stands.
                new > old || (new == old && !g.allow && b.allow)
            }
        };
        if better {
            best = Some(g);
        }
    }
    match best {
        None => Decision::NoRule,
        Some(g) if g.allow => Decision::Allowed { by: g.scope.clone() },
        Some(g) => Decision::Refused { by: g.scope.clone() },
    }
}

/// The kinds of the team's other items, each under `/items/<kind>`. They live
/// in no folder: their rights are the team's own (See, Create, Edit, Delete).
pub const ITEM_KINDS: [&str; 5] = ["bookmark", "extension", "automation", "tag", "template"];

/// Whether a member may make a profile somewhere: in the team, or in a folder a grant names.
pub fn creates_somewhere(grants: &[Grant]) -> bool {
    grants.iter().any(|g| {
        g.allow
            && g.action == Action::Create
            && match &g.scope {
                Scope::Team => decide(grants, Action::Create, &Target::team()).is_allowed(),
                Scope::Folder { name } => decide(grants, Action::Create, &Target::folder(name)).is_allowed(),
                Scope::Profile { .. } => false,
            }
    })
}

/// Whether a member reads the team's items of `kind`: the team's See, or, for a
/// profile template, any place they may make a profile in from it.
pub fn reads_items(grants: &[Grant], kind: &str) -> bool {
    decide(grants, Action::See, &Target::team()).is_allowed() || (kind == "template" && creates_somewhere(grants))
}

/// The short part of a profile that lists show beside its name. Not secret:
/// whoever may See the profile reads it. Sent beside the profile's sealed data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Card {
    /// The proxy the profile starts with, from the profile's own space.
    pub proxy_id: Option<String>,
    pub color: String,
    pub notes: String,
    pub tags: Vec<String>,
}

impl Card {
    /// Limits, in characters.
    pub const MAX_NOTES: usize = 2000;
    pub const MAX_TAGS: usize = 50;
    pub const MAX_TAG: usize = 64;
    pub const MAX_COLOR: usize = 32;
    pub const MAX_PROXY_ID: usize = 128;

    /// What is wrong with this card, or None when it is within the limits.
    pub fn problem(&self) -> Option<&'static str> {
        let long = |s: &str, max: usize| s.chars().count() > max;
        if self.proxy_id.as_deref().is_some_and(|p| long(p, Self::MAX_PROXY_ID)) {
            Some("a proxy id is at most 128 characters")
        } else if long(&self.color, Self::MAX_COLOR) {
            Some("a colour is at most 32 characters")
        } else if long(&self.notes, Self::MAX_NOTES) {
            Some("notes are at most 2000 characters")
        } else if self.tags.len() > Self::MAX_TAGS {
            Some("at most 50 tags")
        } else if self.tags.iter().any(|t| long(t, Self::MAX_TAG)) {
            Some("a tag is at most 64 characters")
        } else {
            None
        }
    }

    /// The bound proxy's id; blank is none.
    pub fn bound_proxy(&self) -> Option<&str> {
        self.proxy_id.as_deref().map(str::trim).filter(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(scope: Scope, action: Action) -> Grant {
        Grant { scope, action, allow: true }
    }
    fn refuse(scope: Scope, action: Action) -> Grant {
        Grant { scope, action, allow: false }
    }
    fn folder(name: &str) -> Scope {
        Scope::Folder { name: name.into() }
    }
    fn profile(id: &str) -> Scope {
        Scope::Profile { id: id.into() }
    }

    #[test]
    fn a_member_starts_with_nothing() {
        let d = decide(&[], Action::See, &Target::profile("p1", "Ads"));
        assert_eq!(d, Decision::NoRule);
        assert!(!d.is_allowed());
    }

    #[test]
    fn a_folder_grant_covers_its_profiles() {
        let g = vec![allow(folder("Ads"), Action::Start)];
        assert!(decide(&g, Action::Start, &Target::profile("p1", "Ads")).is_allowed());
        assert!(!decide(&g, Action::Start, &Target::profile("p2", "Shops")).is_allowed());
        // and says nothing about another action
        assert_eq!(decide(&g, Action::Edit, &Target::profile("p1", "Ads")), Decision::NoRule);
    }

    #[test]
    fn one_profile_can_be_taken_out_of_a_folder_grant() {
        let g = vec![
            allow(folder("Ads"), Action::Start),
            refuse(profile("p9"), Action::Start),
        ];
        assert!(decide(&g, Action::Start, &Target::profile("p1", "Ads")).is_allowed());
        assert_eq!(
            decide(&g, Action::Start, &Target::profile("p9", "Ads")),
            Decision::Refused { by: profile("p9") }
        );
    }

    #[test]
    fn a_profile_grant_beats_a_team_refusal() {
        let g = vec![
            refuse(Scope::Team, Action::Cookies),
            allow(profile("p3"), Action::Cookies),
        ];
        assert!(decide(&g, Action::Cookies, &Target::profile("p3", "Ads")).is_allowed());
        assert!(!decide(&g, Action::Cookies, &Target::profile("p4", "Ads")).is_allowed());
    }

    #[test]
    fn two_rules_of_the_same_width_resolve_to_the_refusal() {
        let g = vec![
            allow(folder("Ads"), Action::Delete),
            refuse(folder("Ads"), Action::Delete),
        ];
        assert_eq!(
            decide(&g, Action::Delete, &Target::profile("p1", "Ads")),
            Decision::Refused { by: folder("Ads") }
        );
        // The order they come in carries nothing.
        let g: Vec<Grant> = g.into_iter().rev().collect();
        assert_eq!(
            decide(&g, Action::Delete, &Target::profile("p1", "Ads")),
            Decision::Refused { by: folder("Ads") }
        );
    }

    #[test]
    fn creating_is_about_the_team_or_a_folder_never_a_profile() {
        // Named on the team: a member may add profiles and proxies to it.
        let g = vec![allow(Scope::Team, Action::Create)];
        assert!(decide(&g, Action::Create, &Target::team()).is_allowed());
        assert!(decide(&g, Action::Create, &Target::folder("Ads")).is_allowed());

        // Narrowed to one folder: only there.
        let g = vec![allow(folder("Ads"), Action::Create)];
        assert!(decide(&g, Action::Create, &Target::folder("Ads")).is_allowed());
        assert!(!decide(&g, Action::Create, &Target::folder("Shops")).is_allowed());
        // The team as a whole is not that folder.
        assert_eq!(decide(&g, Action::Create, &Target::team()), Decision::NoRule);

        // Named on one profile it means nothing and is ignored, rather than
        // quietly granting the team.
        let g = vec![allow(profile("p1"), Action::Create)];
        assert_eq!(decide(&g, Action::Create, &Target::team()), Decision::NoRule);
        assert_eq!(
            decide(&g, Action::Create, &Target::profile("p1", "Ads")),
            Decision::NoRule
        );
    }

    #[test]
    fn taking_a_profile_out_is_its_own_right() {
        // Everything inside the team, but it stays in the team.
        let g = vec![
            allow(Scope::Team, Action::See),
            allow(Scope::Team, Action::Start),
            allow(Scope::Team, Action::Edit),
            allow(Scope::Team, Action::Cookies),
        ];
        let t = Target::profile("p1", "Ads");
        assert!(decide(&g, Action::Start, &t).is_allowed());
        assert!(decide(&g, Action::Cookies, &t).is_allowed());
        assert_eq!(decide(&g, Action::TakeOut, &t), Decision::NoRule);
    }

    #[test]
    fn a_grant_on_a_folder_does_not_leak_to_a_profile_with_no_folder() {
        let g = vec![allow(folder(""), Action::See)];
        // A profile in no folder carries "" — the same name the launcher uses,
        // so the rule has to hold rather than match everything.
        assert!(decide(&g, Action::See, &Target::profile("p1", "")).is_allowed());
        assert!(!decide(&g, Action::See, &Target::profile("p2", "Ads")).is_allowed());
    }

    #[test]
    fn the_wire_shape_is_what_both_sides_read() {
        // The server signs these and the launcher parses them: the names are a contract.
        let g = vec![
            allow(Scope::Team, Action::TakeOut),
            refuse(folder("Ads"), Action::See),
            allow(profile("p1"), Action::Cookies),
        ];
        let wire = serde_json::to_value(&g).unwrap();
        assert_eq!(
            wire,
            serde_json::json!([
                { "scope": { "kind": "team" }, "action": "take_out", "allow": true },
                { "scope": { "kind": "folder", "name": "Ads" }, "action": "see", "allow": false },
                { "scope": { "kind": "profile", "id": "p1" }, "action": "cookies", "allow": true },
            ])
        );
        let back: Vec<Grant> = serde_json::from_value(wire).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn templates_are_read_by_whoever_may_make_a_profile_somewhere() {
        let folder_maker = vec![allow(folder("Ads"), Action::Create)];
        assert!(reads_items(&folder_maker, "template"));
        assert!(!reads_items(&folder_maker, "bookmark"), "other items still need the team's See");
        // A folder grant taken back at the same width makes nothing anywhere.
        let undone = vec![allow(folder("Ads"), Action::Create), refuse(folder("Ads"), Action::Create)];
        assert!(!reads_items(&undone, "template"));
        // A grant on one profile carries no Create.
        assert!(!reads_items(&[allow(profile("p1"), Action::Create)], "template"));
        assert!(reads_items(&[allow(Scope::Team, Action::See)], "template"));
        assert!(!reads_items(&[], "template"));
    }

    #[test]
    fn a_card_is_held_to_its_limits_in_characters() {
        let ok = Card {
            proxy_id: Some("x".repeat(128)),
            color: "ц".repeat(32),
            notes: "н".repeat(2000),
            tags: vec!["т".repeat(64); 50],
        };
        assert_eq!(ok.problem(), None, "multi-byte text counts by characters");
        let bad = [
            Card { proxy_id: Some("x".repeat(129)), ..Card::default() },
            Card { color: "c".repeat(33), ..Card::default() },
            Card { notes: "n".repeat(2001), ..Card::default() },
            Card { tags: vec!["t".into(); 51], ..Card::default() },
            Card { tags: vec!["t".repeat(65)], ..Card::default() },
        ];
        for c in bad {
            assert!(c.problem().is_some(), "{c:?}");
        }
        // Absent fields are empty, and a blank binding is none.
        let c: Card = serde_json::from_value(serde_json::json!({ "proxy_id": "  " })).unwrap();
        assert_eq!((c.bound_proxy(), c.tags.len()), (None, 0));
        assert_eq!(Card { proxy_id: Some("x1".into()), ..Card::default() }.bound_proxy(), Some("x1"));
    }
}
