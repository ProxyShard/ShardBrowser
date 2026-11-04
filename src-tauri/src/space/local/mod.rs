//! The personal space: everything kept in this machine's own files.
//! Visible only to `crate::space`; everyone else goes through its facade.

pub(in crate::space) mod automation;
pub(in crate::space) mod bookmarks;
pub(in crate::space) mod cookies;
pub(in crate::space) mod extensions;
pub(in crate::space) mod folders;
pub(in crate::space) mod profile;
pub(in crate::space) mod protect;
pub(in crate::space) mod proxy;
pub(in crate::space) mod seal;
pub(in crate::space) mod tags;
pub(in crate::space) mod trash;
