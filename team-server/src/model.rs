//! What a team is made of, and who may do what inside it.
//!
//! A team is a SPACE of its own, not a view over somebody's own profiles. A
//! new team starts empty: the person who created it still has their own
//! profiles and proxies, and nothing of theirs is visible to anyone until they
//! move it in on purpose. Everything a member creates while working in the
//! team is created in the team, never in the creator's own space — otherwise
//! the first proxy a colleague adds would quietly become the owner's.

use serde::{Deserialize, Serialize};

// The rights model lives in a crate the launcher shares, so both decide alike.
pub use team_model::*;

/// A team profile's own fields: what the list shows and what the server sorts
/// and addresses by. The profile's settings — fingerprint, proxy binding,
/// cookies — travel and are stored as one blob beside these, sealed at rest
/// with the server's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamProfile {
    pub id: String,
    pub name: String,
    pub folder: String,
    /// Bumped on every write; the launcher uses it to tell its copy from a newer one.
    pub revision: u64,
    pub updated_at: u64,
    pub updated_by: String,
    /// When it was made, unix seconds; None for a row from before the server kept it.
    pub created_at: Option<u64>,
}

/// A member of the team. The token itself is never stored — only its hash, so
/// a stolen database cannot be used to act as anybody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    /// False once the admin turns the member off: their live channel is cut
    /// and no new permission can be issued, whatever grants still say.
    pub active: bool,
    pub created_at: u64,
    /// Bumped when this member's grants change, or a profile, proxy or folder
    /// comes into or out of their sight. Every permission carries it, so an older one reads as stale.
    pub epoch: u64,
    /// Their last request, to the minute; None until they first use their invite.
    pub last_seen_at: Option<u64>,
}

/// The admin's rights as a grant table: every action, on the whole team.
pub fn admin_grants() -> Vec<Grant> {
    Action::ALL.iter().map(|a| Grant { scope: Scope::Team, action: *a, allow: true }).collect()
}
