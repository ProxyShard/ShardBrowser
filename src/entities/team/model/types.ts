// The team admin commands as the launcher's backend answers them: the names,
// arguments and results here are the contract the Rust side implements.

/** What a member may be allowed to do (team-model's `Action`). */
export type TeamAction = "see" | "start" | "edit" | "create" | "delete" | "cookies" | "take_out";

/** Every action, in the order the rights grid shows them. */
export const TEAM_ACTIONS: readonly TeamAction[] = ["see", "start", "edit", "create", "delete", "cookies", "take_out"];

/** What a grant is about; narrower beats wider. */
export type GrantScope =
  | { kind: "team" }
  | { kind: "folder"; name: string }
  | { kind: "profile"; id: string };

/** One line of a member's rights: allowed or refused, never implied. */
export type Grant = { scope: GrantScope; action: TeamAction; allow: boolean };

/** `last_seen_at`: unix seconds of their last request with the token they hold;
 *  null until they use their invite (a new invite makes it null again). */
export type TeamMember = { id: string; name: string; active: boolean; created_at: number; epoch: number; last_seen_at: number | null };
export type TeamFolder = { name: string; color: string; profiles: number };
export type TeamProfileRef = { id: string; name: string; folder: string };
export type TeamInfo = { id: string; name: string };

/** What the admin's Team page reads in one go. */
export type TeamOverview = {
  team: TeamInfo;
  members: TeamMember[];
  folders: TeamFolder[];
  profiles: TeamProfileRef[];
  /** Each member's rules by member id, for the list's Access column; left out, the column is blank. */
  grants?: Record<string, Grant[]>;
};

/** Shown once: the invite is the server's address, "#" and the member's token.
 *  A new invite answers the same, the member read as invited again. */
export type CreatedMember = { member: TeamMember; invite: string };

/** What the team's trash holds (space/admin.rs `TeamTrashRow`). */
export type TeamTrashKind = "profile" | "proxy" | "bookmark" | "extension" | "automation" | "tag" | "template";

/** A row of the team's trash, as far as this launcher's token may see it. Times are unix seconds. */
export type TeamTrashRow = {
  id: string;
  kind: TeamTrashKind;
  name: string;
  /** The folder a profile was deleted from ("": none); "" for every other kind. */
  folder: string;
  /** A proxy's own type and country; "" for every other kind. */
  proxy_kind: string;
  country: string;
  /** The revision it was deleted at; a restore answers revision + 1. */
  revision: number;
  updated_at: number;
  updated_by: string;
  deleted_at: number;
  /** A member id, or "admin". */
  deleted_by: string;
  /** When the server purges it on its own (deleted_at + 30 days). */
  purge_at: number;
};

export type TeamRestored = { kind: TeamTrashKind; id: string; name: string; revision: number };

/** Every team command: invoke name, its arguments and what it answers. A
 *  refusal rejects with the server's message as TeamError prints it. */
export type TeamCommands = {
  team_admin_overview: { args: { teamId: string }; result: TeamOverview };
  team_admin_member_create: { args: { teamId: string; name: string }; result: CreatedMember };
  team_admin_member_set_active: { args: { teamId: string; memberId: string; active: boolean }; result: TeamMember };
  team_admin_member_delete: { args: { teamId: string; memberId: string }; result: null };
  team_admin_member_reissue: { args: { teamId: string; memberId: string }; result: CreatedMember };
  team_admin_grants: { args: { teamId: string; memberId: string }; result: Grant[] };
  team_admin_set_grants: { args: { teamId: string; memberId: string; grants: Grant[] }; result: Grant[] };
  team_admin_rename: { args: { teamId: string; name: string }; result: TeamInfo };
  team_admin_folder_create: { args: { teamId: string; name: string; color?: string }; result: TeamFolder };
  team_admin_folder_rename: { args: { teamId: string; name: string; next: string }; result: TeamFolder };
  team_admin_folder_color: { args: { teamId: string; name: string; color: string }; result: TeamFolder };
  team_admin_folder_delete: { args: { teamId: string; name: string }; result: null };
  team_my_access: { args: { teamId: string }; result: Grant[] };
  // The team's trash: no epoch, the server judges each row by this launcher's token.
  team_trash_list: { args: { teamId: string }; result: TeamTrashRow[] };
  team_trash_restore: { args: { teamId: string; id: string; kind: TeamTrashKind }; result: TeamRestored };
  team_trash_purge: { args: { teamId: string; id: string; kind: TeamTrashKind }; result: null };
};

export type TeamCommand = keyof TeamCommands;
