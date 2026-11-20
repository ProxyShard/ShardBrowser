import { invoke as tauriInvoke, type InvokeArgs, type InvokeOptions } from "@tauri-apps/api/core";
import { EPOCH_WRITES, SPACE_REFUSALS } from "./epochWrites";
import { epochForWrite, setSpaceEpoch, spaceEpoch } from "./edition";

export { onSpaceMoved, setSpaceEpoch, spaceEpoch, withEpoch } from "./edition";

/// The stores' way to the backend: a space write carries the edition of the space on screen,
/// so one sent after the launcher moved on is refused whole, and the space is read again.

/** The lists each space has: one asked while the space moved is asked again. */
const SPACE_READS: ReadonlySet<string> = new Set([
  "profile_list", "profile_get", "proxy_list", "tag_list", "folder_list",
  "bookmark_list", "extension_list", "extension_updates", "automation_list", "trash_list", "profile_template_list",
]);

let reading: Promise<number | undefined> | null = null;
let reread: () => Promise<unknown> = () => readEdition();

/** How the space is read again after a refusal: the space store's own read. */
export function setSpaceReader(fn: () => Promise<unknown>): void {
  reread = fn;
}

function readEdition(): Promise<number | undefined> {
  return reading ??= tauriInvoke<{ epoch?: number }>("space_state")
    .then((s) => { setSpaceEpoch(s?.epoch); return spaceEpoch(); }, () => spaceEpoch())
    .finally(() => { reading = null; });
}

const says = (e: unknown, words: string) => String(e instanceof Error ? e.message : e).includes(words);

/** Tauri's `invoke`, with the space's edition on the writes that need it. */
export async function invoke<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  if (SPACE_READS.has(cmd)) {
    // An answer from the space left meanwhile is asked again: it is the new space's then.
    for (let tries = 0; ; tries++) {
      const asked = spaceEpoch();
      const got = await tauriInvoke<T>(cmd, args, options);
      // Before the first edition is known there is no space to have left.
      if (asked === undefined || asked === spaceEpoch() || tries >= 2) return got;
    }
  }
  if (!EPOCH_WRITES.has(cmd)) return tauriInvoke<T>(cmd, args, options);
  // Read before any wait: a write `withEpoch` pinned carries the space it was asked in.
  const epoch = epochForWrite() ?? (await readEdition());
  const sent = epoch === undefined ? args : { ...(args as Record<string, unknown> | undefined), epoch };
  try {
    return await tauriInvoke<T>(cmd, sent, options);
  } catch (e) {
    if (says(e, SPACE_REFUSALS.spaceChanged)) {
      void reread().catch(() => {});
    } else if (says(e, SPACE_REFUSALS.noEpoch)) {
      // Never meant to happen: the edition is always sent once known.
      console.error(`[invoke] ${cmd} reached a team without the space's edition (a bug)`, e);
      void reread().catch(() => {});
    }
    throw e;
  }
}
