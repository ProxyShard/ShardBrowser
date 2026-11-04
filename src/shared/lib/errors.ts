import { t } from "../i18n";
import { SYNC_REFUSALS } from "../../entities/sync/model/types";
import { refusal } from "../../entities/sync/lib/refusals";
import { EXTENSION_TOO_BIG_RE, FORBIDDEN_RE, TEAM_REFUSALS } from "../api/teamRefusals";
import { LOCK_REFUSALS } from "../api/lockRefusals";
import { AUTOMATION_REFUSALS } from "../api/automationRefusals";
import { fmtBytes } from "./format";

const LEASED = refusal(SYNC_REFUSALS.leased);
/** A fixed refusal as a pattern, its dots taken literally. */
const phrase = (words: string) => new RegExp(words.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i");

/** A right's name as the rights editor shows it; each key called literally. */
function rightWords(right: string): string {
  switch (right.toLowerCase()) {
    case "see": return t("team.actSee");
    case "start": return t("team.actStart");
    case "edit": return t("team.actEdit");
    case "create": return t("team.actCreate");
    case "delete": return t("team.actDelete");
    case "cookies": return t("team.actCookies");
    default: return t("team.actTakeOut");
  }
}

function needsWords(right: string, on: string): string {
  if (right.toLowerCase() === "admin") return t("errors.teamAdminOnly");
  const name = rightWords(right);
  // Start runs without it; what needs it is driving the browser (CDP, automation, the Local API).
  if (on === "profile" && right.toLowerCase() === "cookies") return t("errors.teamNeedsCookies");
  if (on === "profile") return t("rights.needsProfile", { right: name });
  if (on === "proxy") return t("rights.needsProxy", { right: name });
  if (on === "folder") return t("errors.teamNeedsFolder", { right: name });
  return t("rights.needsTeam", { right: name });
}

/// Backend failures in words a person can act on (WRITE-01; plan ruling 5).
/// The backend rejects with English prose today (backend B13 asks for codes).
/// A message this table knows becomes one translated sentence and its raw
/// text, which often holds an id or a path, is dropped; anything else is shown
/// as the backend wrote it, as the muted detail under a translated title.
const KNOWN: ReadonlyArray<readonly [RegExp, string]> = [
  // A write refused for its space (SPACE_REFUSALS): the second is a bug, never meant to show.
  [/\bswitched spaces before this was saved\b/i, "errors.spaceChanged"],
  [/\bhas to name the space it was asked in\b/i, "errors.spaceUnknown"],
  // A team space's own refusals; before the network's, which a team's offline words carry.
  [/\bnot offered in a team space yet\b/i, "errors.personalOnly"],
  [/\bnot offered for a team's profiles yet\b/i, "errors.teamNotYet"],
  [/\bnot offered in a team space\b/i, "errors.notInTeam"],
  // The team server answered, but not as one (client.rs Garbled): before "did not answer".
  [/\bdid not answer as a team server\b/i, "errors.notTeamServer"],
  // The encrypted link to it (client.rs WireFault, and the vouch).
  [/\bdoes not answer as an encrypted team server\b/i, "errors.teamNotEncrypted"],
  [/\bis a newer team server than this launcher\b/i, "errors.teamServerNewer"],
  [/\bis not this team.s server: it shows another key\b/i, "errors.teamOtherKey"],
  [/\bcould not prove it is the server this token is from\b|\bchecks permissions with another key\b/i, "errors.teamUnproven"],
  [/\banswered the live channel wrongly\b/i, "errors.teamGarbled"],
  [/\bthe session could not be written here\b/i, "errors.sessionWriteFailed"],
  [/\bthe team server did not answer\b|\bno current permission from the team\b/i, "errors.teamOffline"],
  // The team's permission (perm.rs, live.rs).
  [/\bno key to check the team's permission with\b/i, "errors.permNoKey"],
  [/\bthe (?:team's )?permission has run out\b/i, "errors.permExpired"],
  [/\bthe permission is (?:not signed by the team's key|not readable|for another team|for another member|older than one this launcher already had)\b/i, "errors.permInvalid"],
  [/\bno permission from the team yet\b/i, "errors.permNotYet"],
  [/\bthe team's rules changed since the profile was looked up\b/i, "errors.rulesChanged"],
  [/\bthe space is already being switched\b/i, "errors.alreadySwitching"],
  // A team profile's session as it opens (pack.rs, open.rs).
  [/\bthis session was saved by a newer launcher\b/i, "errors.sessionNewer"],
  [/\bthe session unpacks to more than \d+ MB\b/i, "errors.sessionTooBig"],
  [/\bthe profile was closed before its browser started\b/i, "errors.closedBeforeStart"],
  [/\bstill opening or being saved here\b/i, "errors.teamBusy"],
  [phrase(TEAM_REFUSALS.folderInTrash), "errors.teamFolderInTrash"],
  [phrase(TEAM_REFUSALS.teamExtensionsNotYet), "errors.teamExtensionsNotYet"],
  // A profile's own password (#127).
  [phrase(LOCK_REFUSALS.needsPassword), "errors.passwordNeeded"],
  [phrase(LOCK_REFUSALS.wrongPassword), "errors.wrongPassword"],
  [phrase(LOCK_REFUSALS.tooShort), "errors.passwordTooShort"],
  [phrase(LOCK_REFUSALS.closeFirst), "errors.closeProfileFirst"],
  [phrase(LOCK_REFUSALS.syncOn), "errors.passwordSyncOn"],
  [phrase(LOCK_REFUSALS.notAvailable), "errors.lockedNotAvailable"],
  // The automation studio's start (lib.rs automation_launch, space::cdp_pass).
  [phrase(AUTOMATION_REFUSALS.openWithout), "errors.openWithoutAutomation"],
  [phrase(AUTOMATION_REFUSALS.didNotClose), "errors.didNotClose"],
  [phrase(AUTOMATION_REFUSALS.noDebugPort), "errors.noDebugPort"],
  [phrase(AUTOMATION_REFUSALS.drivenByRun), "errors.drivenByRun"],
  [phrase(AUTOMATION_REFUSALS.notAutomated), "errors.notAutomated"],
  // Web Store updates and links (updates.rs, extensions.rs); before the network's words.
  [/\bthe Web Store has no extension with that id\b/i, "errors.extensionNotInStore"],
  [/\bby its files, that is a different extension\b|\bthat is a different extension: its key gives it\b/i, "errors.extensionDifferent"],
  [/\bthe Web Store marks this extension as harmful\b/i, "errors.extensionFlagged"],
  [/\bthis extension is being updated already\b/i, "errors.extensionBusy"],
  [/\bthe extension is being updated(?: right now)?; try again in a minute\b/i, "errors.extensionBusyNow"],
  [/\bthe extension is not linked to the Web Store yet\b/i, "errors.extensionUnlinked"],
  [/\bit is linked to the Web Store item \S+ already\b|\bthe extension is linked to another Web Store item\b/i, "errors.extensionLinkedAlready"],
  [/\bthe Web Store does not know this extension\b/i, "errors.extensionGone"],
  [/\bwould change (?:this|the) extension's ID\b|\bits key gives it the ID \S+, not the Web Store item\b/i, "errors.extensionIdChange"],
  [/\bthe Web Store's package failed its check\b|\bthe download does not match the Web Store's hash\b|\bthe download is \d+ bytes, the Web Store said\b|\bthe CRX is not signed by the Web Store\b|\bthe Web Store did not send a CRX3 package\b|\bthe Web Store named version \S+ but sent\b/i, "errors.extensionBadPackage"],
  [/\bthe (?:update|package) (?:could not|cannot) be unpacked\b/i, "errors.extensionUnpack"],
  // A file added to the library (extensions.rs): no package, or no extension in it.
  [/\bnot a zip\/crx archive\b|\bunsupported CRX version\b|\bCRX header runs past the end\b/i, "errors.extensionNotPackage"],
  [/\bno manifest\.json inside\b/i, "errors.extensionNoManifest"],
  [/\bthe extension's folder is in use\b/i, "errors.extensionFolderInUse"],
  [/\bthe update was cancelled\b/i, "errors.updateCancelled"],
  [/\bthe Web Store offers no package of it right now\b/i, "errors.extensionNoPackage"],
  [/\bcould not reach the Web Store\b|\bthe Web Store did not answer\b|\bthe Web Store answered \d{3}\b|\bthe Web Store's answer\b/i, "errors.webStoreFailed"],
  [/\balready_in_library \{/, "errors.alreadyInLibrary"],
  [/\bAn extension of this profile is being updated\b/i, "errors.extensionUpdating"],
  [phrase(TEAM_REFUSALS.badProxy), "errors.teamBadProxy"],
  [phrase(TEAM_REFUSALS.stillOpen), "errors.teamStillOpen"],
  [phrase(TEAM_REFUSALS.switchWhileStarting), "errors.switchWhileStarting"],
  [phrase(TEAM_REFUSALS.switchWhileMoving), "errors.switchWhileMoving"],
  [phrase(TEAM_REFUSALS.launchWhileSwitching), "errors.launchWhileSwitching"],
  [/\bthe profile's guard process did not start\b/i, "errors.guardFailed"],
  // Saved by someone else since it was read (CmdError::Stale, and a profile's own check).
  [/\bchanged after you opened it\b/i, "errors.changedElsewhere"],
  // A folder's name may hold spaces.
  [/^no [a-z]+ .+ in this team$/i, "errors.teamGone"],
  [/^no [a-z]+ .+ in your own space$/i, "errors.ownGone"],
  [/^the team server answered \d{3}\b/i, "errors.teamStatus"],
  [/\bmigration is already running\b/i, "errors.migrationRunning"],
  [/\bis already running\b/i, "errors.alreadyRunning"],
  [/\bnot installed yet\b/i, "errors.engineMissing"],
  // A start or a GPU check while the engine folder is swapped (runtime.rs UPDATING).
  [/\bthe ShardBrowser engine is being updated\b/i, "errors.engineUpdating"],
  [/\bHost platform has no published ShardBrowser engine archive\b/i, "errors.engineNoPlatform"],
  [/\bbound to a proxy that no longer exists\b/i, "errors.proxyMissing"],
  [/\bauth failed\b|\bno acceptable auth method\b/i, "errors.proxyAuth"],
  [/\btimed out\b|\bos error (?:60|10060)\b/i, "errors.timedOut"],
  [/error sending request|dns error|connection refused|network is unreachable|\bos error (?:51|61|65|10051|10061|10065)\b/i, "errors.offline"],
  [/every geo source failed/i, "errors.geoFailed"],
  [/expected value at line|EOF while parsing|invalid type: |key must be a string|trailing characters at line/i, "errors.badJson"],
  [/no such file or directory|\bos error 2\b|cannot find the (?:file|path) specified/i, "errors.notFound"],
];

/** Backend error codes (B13) to keys, for when commands reject with `{ code }`. */
const CODES: Readonly<Record<string, string>> = {
  already_running: "errors.alreadyRunning",
  engine_missing: "errors.engineMissing",
  proxy_missing: "errors.proxyMissing",
  proxy_unreachable: "errors.proxyUnreachable",
  bad_clipboard_json: "errors.badJson",
  offline: "errors.offline",
  trash_failed: "errors.trashFailed",
};

/** The backend's own words, or undefined when there are none to show. */
export function rawError(e: unknown): string | undefined {
  if (e === undefined || e === null) return undefined;
  if (e instanceof Error) return e.message.trim() || undefined;
  if (typeof e === "string") return e.replace(/^Error:\s*/, "").trim() || undefined;
  if (typeof e === "object") {
    const m = (e as { message?: unknown }).message;
    return typeof m === "string" && m.trim() ? m.trim() : undefined;
  }
  return String(e);
}

/** The line under an error toast's title: a translated sentence for a known
 *  failure, else the backend's words; undefined when there is nothing to say. */
export function explainError(e: unknown): string | undefined {
  if (e && typeof e === "object" && !(e instanceof Error)) {
    const code = (e as { code?: unknown }).code;
    if (typeof code === "string" && CODES[code]) return t(CODES[code]);
  }
  const raw = rawError(e);
  if (!raw) return undefined;
  // A synced profile's lease names the device that holds it.
  const lease = LEASED.exec(raw);
  if (lease) return t("errors.openElsewhere", { device: lease[1] });
  // A team's door refused it (space/error.rs `Forbidden`): the right, and what it is held on.
  const need = FORBIDDEN_RE.exec(raw);
  if (need) return needsWords(need[1], need[2]);
  const big = EXTENSION_TOO_BIG_RE.exec(raw);
  if (big) return t("errors.extensionTooBig", { n: big[1], max: big[2] });
  // An extension's update or unpack (extensions/swap.rs, extensions.rs): both sizes in MB.
  const room = /\bnot enough free disk space (?:for the update|to unpack it): it needs (?:about )?(\d+) MB,? (?:and )?(\d+) MB is free\b/i.exec(raw);
  if (room) return t("errors.diskSpace", { need: fmtBytes(Number(room[1]) * 2 ** 20), free: fmtBytes(Number(room[2]) * 2 ** 20) });
  // runtime.rs runtime_install, refused for the launcher's version.
  const old = /\bneeds ShardBrowser Launcher (\S+) or newer\b.*?\byou are on (\S+?)\.(?:\s|$)/i.exec(raw);
  if (old) return t("errors.engineNeedsLauncher", { need: old[1], have: old[2] });
  for (const [re, key] of KNOWN) if (re.test(raw)) return t(key);
  return raw;
}
