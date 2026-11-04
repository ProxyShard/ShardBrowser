import type { ColumnFit, ColumnFits } from "../../shared/lib/tableColumns";
import { t, type Lang } from "../../shared/i18n";
import { stampMs } from "../../shared/lib/utils";
import { fmtDuration } from "../../shared/lib/format";
import { tagChipsWidth } from "../../shared/lib/measureText";
import { formatRelativeRun, type ProfileColumnId, type ProfileMeta } from "../../entities/profile";
import type { ProxyEntry } from "../../entities/proxy";

// The px ProfileRow's cells draw beside their text. Keep in step with it.
/** A pinned name's pin, or a synced one's cloud: the 14 px glyph and the cell's 6 px gap (`gap-1.5`). */
const PIN = 14 + 6;
/** A Folder chip's side padding (`px-1.5`, both sides). */
const CHIP = 12;
/** A proxy's country flag, or the globe in its place (17 px tall, 4:3), and the gap after it (`gap-1.5`). */
const FLAG = 23 + 6;
/** The tags a row shows as chips before "+N" (TagChips). */
const SHOWN_TAGS = 2;
/** A status pill's sides beside its word (the kit's medium Badge): 4 and
 *  7 px of padding, the lead's 14 px slot (a dot or a spinner) and its 3 px
 *  gap; a pill with no lead (Failed to start, Proxy missing) has 7 px each
 *  side. Keep in step with the kit. */
const PILL_LEAD = 4 + 7 + 14 + 3;
const PILL_BARE = 7 + 7;

/** The characters of a proxy's name (or host) its folded cell keeps before the ellipsis. */
const FOLDED_NAME = 10;

/** A day, ms: a run older than this reads by its calendar day (`formatRelativeRun`). */
const DAY = 24 * 3600_000;

/// What the Proxy cell shows, measured: "No proxy" or "Proxy missing", else
/// the flag (or a globe) and the proxy's name, or its address when it has none (or is
/// named by its address) with its port always whole (F17, TYPE-07); the type
/// is in the tooltip, not the cell. `clip` is the part of the name or host
/// the cell keeps: all of it, or its folded form's first characters.
function proxyFit(proxyOf: (id: string) => ProxyEntry | undefined, clip: (s: string) => string): ColumnFit<ProfileMeta> {
  const direct = t("profileRow.direct");
  const missing = t("profileRow.proxyMissing");
  const hidden = t("profileRow.proxyHidden");
  const locked = t("profileRow.locked");
  return (p, m) => {
    if (p.protected) return m(locked, "secondary");
    if (!p.proxy_id) return m(direct, "secondary");
    const x = proxyOf(p.proxy_id);
    if (!x) return m(p.origin === "team" ? hidden : missing, "secondary");
    // Every proxy has the slot: its flag, or a globe where it has none.
    const named = !!x.name && x.name !== `${x.host}:${x.port}`;
    return FLAG + (named ? m(clip(x.name), "secondary") : m(`${clip(x.host)}:${x.port}`, "mono"));
  };
}

/** The first `FOLDED_NAME` characters and an ellipsis, or all of a shorter text. */
function folded(text: string): string {
  const chars = [...text];
  return chars.length > FOLDED_NAME ? `${chars.slice(0, FOLDED_NAME).join("")}…` : text;
}

/// A run's time as a cell words it ("5 min ago", "Yesterday", "Aug 29"), or
/// `none` with no run. Past a day a time reads by its calendar day:
/// formatted once per day, not once per row (1,000 rows in under 10 ms).
function runWords(lang: Lang, now: number) {
  const byDay = new Map<string, string>();
  return (stamp: string | null | undefined, none: string) => {
    const at = stampMs(stamp);
    if (at === null) return none;
    if (now - at < DAY) return formatRelativeRun(at, now, lang);
    const d = new Date(at);
    const key = `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
    let label = byDay.get(key);
    if (label === undefined) byDay.set(key, (label = formatRelativeRun(at, now, lang)));
    return label;
  };
}

/// The Status pill at its widest for a row: its own last run (or Never run),
/// or any of the live words every row can pass through. A pill never folds,
/// so this is the column's floor as well as its fit. In English and Chinese
/// the widest ("Proxy missing", 94 px) fits the column's 96 px minimum,
/// which then holds as before; Russian's words are longer, and widen it
/// instead of being cut ("Не запускался").
function statusFit(lang: Lang, now: number): ColumnFits<ProfileColumnId, ProfileMeta>["status"] {
  const when = runWords(lang, now);
  const neverRun = t("status.neverRun");
  const withLead = ["status.running", "status.starting", "status.stopping", "status.cancelled"].map((k) => t(k));
  const bare = [t("status.failedToStart"), t("profileRow.proxyMissing")];
  return (p, m) => Math.max(
    PILL_LEAD + m(when(p.last_launched_at, neverRun), "medium"),
    ...withLead.map((w) => PILL_LEAD + m(w, "medium")),
    ...bare.map((w) => PILL_BARE + m(w, "medium")),
    // Open on another device names it.
    p.sync?.on && p.sync.open_on ? PILL_LEAD + m(t("status.openOn", { device: p.sync.open_on }), "medium") : 0,
  );
}

/// What each Profiles cell shows, for sizing its column to its content
/// (owner, 2026-09-30): the text ProfileRow draws, in the same type role,
/// plus its fixed parts. Status sizes to its widest pill (`statusFit`); Notes
/// to its text, so the room left over can show a note whole first.
/// Times read `now` as the list was measured, again each hour (the list's
/// fits follow an hour clock): "5 min ago" and "6 min ago" are as wide, but
/// "23 hours ago" and "Yesterday" are not (review M1).
export function profileFits({ proxyOf, tagsOf, lang, now }: {
  proxyOf: (id: string) => ProxyEntry | undefined;
  tagsOf: (profileId: string) => readonly { name: string }[];
  lang: Lang;
  now: number;
}): ColumnFits<ProfileColumnId, ProfileMeta> {
  const never = t("peek.never");
  const phone = t("profileTable.devicePhone");
  const desktop = t("profileTable.deviceDesktop");
  const when = runWords(lang, now);
  return {
    // The lock, the pin and the sync mark each take a pin's room.
    name: (p, m) => m(p.name, "ui") + (p.protected ? PIN : 0) + (p.pinned ? PIN : 0) + (p.sync?.on ? PIN : 0),
    status: statusFit(lang, now),
    folder: (p, m) => (p.folder ? CHIP + m(p.folder, "secondary") : 0),
    // The width at which TagChips' own fold (F18) shows the first two chips
    // whole, then "+N": one geometry for the fit and the fold.
    tags: (p, m) => {
      const tags = tagsOf(p.id);
      return tags.length === 0 ? 0 : tagChipsWidth(tags.map((x) => x.name), SHOWN_TAGS, (s) => m(s, "secondary"));
    },
    proxy: proxyFit(proxyOf, (s) => s),
    lastRun: (p, m) => m(when(p.last_launched_at, never), "secondary"),
    created: (p, m) => m(when(p.created_at, "—"), "secondary"),
    // The stored total: a running session adds minutes, rarely a digit.
    runTime: (p, m) => m(p.total_runtime_ms > 0 ? fmtDuration(p.total_runtime_ms, lang) : "—", "tabular"),
    device: (p, m) => m(p.mobile ? phone : desktop, "secondary"),
    extensions: (p, m) => m(String(p.extensions.length), "tabular"),
    notes: (p, m) => (p.protected ? m(t("profileRow.locked"), "secondary") : p.notes ? m(p.notes, "secondary") : 0),
  };
}

/// What the columns that fold show at their narrowest (controller rulings,
/// merge point 4): Tags one whole chip and "+N" (TagChips' fold at one
/// chip), Folder its one chip, Proxy its flag and about ten characters of
/// its name. Short of room these columns give way down to it, never further
/// (never "DC Ne…"); Name gives way after them. Status never folds: its
/// narrowest is its widest pill, so it is never cut either.
export function profileFolded({ proxyOf, tagsOf, lang, now }: {
  proxyOf: (id: string) => ProxyEntry | undefined;
  tagsOf: (profileId: string) => readonly { name: string }[];
  lang: Lang;
  now: number;
}): ColumnFits<ProfileColumnId, ProfileMeta> {
  return {
    status: statusFit(lang, now),
    proxy: proxyFit(proxyOf, folded),
    folder: (p, m) => (p.folder ? CHIP + m(p.folder, "secondary") : 0),
    tags: (p, m) => {
      const tags = tagsOf(p.id);
      return tags.length === 0 ? 0 : tagChipsWidth(tags.map((x) => x.name), 1, (s) => m(s, "secondary"));
    },
  };
}
