/// The backend's "launcher-warning" texts about team and synced profiles, in its own words
/// (space/open.rs, space/live.rs, space/admin.rs, space/sync/cycle.rs, lib.rs). The mock raises
/// these and the app reads them back (useLauncherWarning). Kept free of imports, so
/// `npm run test:unit` checks them against the Rust sources.

export const TEAM_WARNINGS = {
  /** A team profile's session was saved from another device first (open.rs Outcome::Stale). */
  stale: (name: string) => `${name} was saved from another device while it was open here, so the changes made here were not kept.`,
  /** Its session could not be sent back (Outcome::NotSaved). */
  notSaved: (name: string, why: string) => `${name} closed, and its session was not saved to the team: ${why}`,
  /** Not sent, kept here for the next start (Outcome::Unsent): notSaved with this tail. Before notSaved when read. */
  unsent: (name: string, why: string) =>
    `${name} closed, and its session was not saved to the team: ${why}; it is kept on this computer and sent when the launcher starts next`,
  /** The first upload the server did not take; it is sent again until it does (open.rs send_back). */
  resending: (name: string, why: string) =>
    `${name} closed, but the team server did not take its session: it is kept here and sent again until the server takes it (${why}).`,
  /** The team's permission runs out early by this computer's clock (open.rs look_at_permission). */
  clockAhead: (min: number | string, name: string) =>
    `This computer's clock is ${min} min ahead of the team server's, so the team's permission runs short: set the clock right, or ${name} may close when the permission runs out.`,
  /** A start the server gave only some of the profile's extensions (items.rs). */
  extensionsMissing: (name: string, n: number | string) =>
    `${name} started without ${n} of its extensions: the team server did not give them to this launcher.`,
  /** Quit waits for the open team and synced profiles, and seals the protected ones (lib.rs exit_app). */
  quitting: "Saving the team, synced and protected profiles open here before quitting…",
  withdrew: (team: string) => `${team} withdrew this launcher's access.`,
  replaced: (team: string) => `${team} replaced this launcher's invite: paste the new one from the team's admin.`,
  permission: (why: string) => `The team's permission did not check out: ${why}`,
  anotherTeam: (team: string, server: string) => `${team} at ${server} answers as another team now: was it started without its data?`,
  closesSoon: (name: string, min: number | string) =>
    `The team server has not been reachable for a while: ${name} closes in ${min} min unless it answers.`,
  lostLease: (name: string, why: string) =>
    `${name}: ${why} while this computer could not reach the team, so it was closed here and the changes made here were not kept.`,
  tooBig: (name: string, sites: number | string) => `${name}: its session was over 256 MB, so the storage of ${sites} site(s) was left out.`,
  localInvite: (host: string) =>
    `This invite starts with ${host}, which only this computer reaches: join the team by its network address, or start the server with SHARDBROWSER_TEAM_PUBLIC_URL.`,
  /** A synced profile's conflict copy (cycle.rs keep_both); the profile list says it too. */
  profileConflict: (name: string) =>
    `${name} was changed on another computer too, so both versions are kept: the changes made here are in “${name} (conflict copy)”.`,
  projectConflict: (name: string) =>
    `“${name}” was changed on another computer too, so both versions are kept: the changes made here are in “${name} (conflict copy)”.`,
  proxySetDifferently: (id: string) => `The proxy ${id} was set differently on another computer: that version is kept.`,
  proxyChanged: (name: string) => `The proxy ${name} was changed on another computer too: that version is kept.`,
  /** `kind`: "tag", "bookmark" or "extension" (cycle.rs ItemKind::key). */
  itemSetDifferently: (kind: string) => `A ${kind} was set differently on another computer: that version is kept.`,
  itemChanged: (name: string) => `“${name}” was changed on another computer too: that version is kept.`,
  /** A synced extension's package left out here (sync/cycle/extension.rs). */
  extensionIdKept: (name: string) => `“${name}” from another computer would change its ID here, so the version on this computer is kept.`,
  extensionOlderKept: (name: string) => `“${name}” from another computer is older than the version here, so the version on this computer is kept.`,
  extensionNewerKept: (name: string, mine: string, there: string) =>
    `“${name}” is newer on this computer (${mine}) than on another one (${there}), so the version on this computer is kept.`,
  /** Delete all data could not take the team links off this computer (reset.rs). */
  teamLinksKept: (why: string) => `The team connections could not be removed: ${why}`,
} as const;

/** The name live.rs gives a team it no longer knows the name of. */
export const UNNAMED_TEAM = "A team";

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** A warning's words as a whole-text pattern: each argument is a group, in order. */
export function warningRe(words: string | ((...args: string[]) => string)): RegExp {
  if (typeof words === "string") return new RegExp(`^${esc(words)}$`);
  const marks = Array.from({ length: words.length }, (_, i) => String.fromCharCode(0xe000 + i));
  let re = esc(words(...marks));
  // Each argument's first place is its group (they come in order); a repeat is a back-reference.
  marks.forEach((m, i) => {
    const at = re.indexOf(m);
    re = re.slice(0, at) + "(.+?)" + re.slice(at + 1).split(m).join(`\\${i + 1}`);
  });
  return new RegExp(`^${re}$`, "s");
}
