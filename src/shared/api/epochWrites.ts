/// The writes that carry the space's edition: each backend command taking `epoch: Option<u64>`
/// (epochWrites.test.ts checks). No imports, so node tests and the mock read it as it is.
export const EPOCH_WRITES: ReadonlySet<string> = new Set([
  "automation_create", "automation_delete", "automation_duplicate", "automation_import", "automation_save",
  "bookmark_delete", "bookmark_delete_many", "bookmark_save",
  "extension_delete", "extension_import", "extension_import_url",
  "extension_link_source", "extension_update", "extension_updates_check",
  "folder_add", "folder_delete", "folder_import", "folder_meta_set", "folder_rename",
  "profile_bind_proxy", "profile_clone", "profile_create_from_template", "profile_delete", "profile_import",
  "profile_save", "profile_set_folder", "profile_set_pin",
  "proxy_bulk_import", "proxy_bulk_save", "proxy_delete", "proxy_save",
  "ps_import_order",
  "tag_assign", "tag_delete", "tag_import", "tag_save",
  "trash_empty", "trash_purge", "trash_restore",
]);

/// The backend's words when a write cannot be placed in a space.
export const SPACE_REFUSALS = {
  /** space/mod.rs NO_EPOCH: a team write sent without its edition. Never shown when the app works. */
  noEpoch: "a write in a team space has to name the space it was asked in (epoch): nothing was written",
  /** space/error.rs SPACE_CHANGED: the launcher moved to another space before the write landed. */
  spaceChanged: "the launcher switched spaces before this was saved: nothing was written",
} as const;
