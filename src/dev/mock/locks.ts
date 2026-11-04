// Password-protected profiles (#127) as the backend keeps them: the commands that set,
// change, remove and open a password, and the checks the other handlers make. The mock
// keeps the profile readable in its state; only what it answers is sealed.
import type { MockState, StoredProfile } from "./fixtures";
import { metaOf, mockLock, syncLink } from "./fixtures";
import type { Handler } from "./handlers";
import { sleep } from "./store";
import { LOCK_REFUSALS } from "../../shared/api/lockRefusals";

/** The backend's minimum (seal.rs). */
const PASSWORD_MIN = 8;
/** About what Argon2id takes once per open, so a dialog's busy state shows. */
const KDF_MS = 250;
/** What a stub may carry in `_meta`: what profile_get gave, nothing sealed. */
const STUB_META = new Set(["id", "folder", "pinned", "color", "rev", "created_at", "last_launched_at", "total_runtime_ms", "lock"]);

function profileOf(state: MockState, id: unknown): StoredProfile {
  const p = state.profiles[String(id)];
  if (!p) throw `profile ${id} not found`;
  return p;
}

const opens = (p: StoredProfile, password: unknown) =>
  !!p._meta.lock && typeof password === "string" && mockLock(password, p._meta.lock.kdf.salt).key === p._meta.lock.key;

/** Throws the backend's words unless `password` opens it. */
export function checkPassword(p: StoredProfile, password: unknown): void {
  if (typeof password !== "string" || password === "") throw LOCK_REFUSALS.needsPassword;
  if (!opens(p, password)) throw LOCK_REFUSALS.wrongPassword;
}

/** A door a protected profile does not have: clone, cookies, a synced or automation start, a proxy. */
export function refuseLocked(state: MockState, id: unknown): void {
  if (state.profiles[String(id)]?._meta.lock) throw LOCK_REFUSALS.notAvailable;
}

/** What profile_get answers for a protected profile: no fingerprint keys. */
export function stubOf(p: StoredProfile) {
  const m = p._meta;
  return {
    _meta: {
      id: m.id, folder: m.folder, pinned: m.pinned, color: m.color ?? null, rev: m.rev, created_at: m.created_at,
      last_launched_at: m.last_launched_at, total_runtime_ms: m.total_runtime_ms, lock: m.lock,
    },
    name: p.name,
  };
}

/// profile_save of a protected profile without its password: a stub that changes its
/// name or color (an inline rename) is saved; anything more is refused. The row, or
/// null when the save is not one of these (it goes the usual way, with the password).
export function saveStub(prev: StoredProfile, incoming: Record<string, any>, password: unknown): ReturnType<typeof metaOf> | null {
  if (!prev._meta.lock) return null;
  if (password !== undefined) { checkPassword(prev, password); return null; }
  const extra = Object.keys(incoming).filter((k) => k !== "_meta" && k !== "name");
  const sealed = Object.keys(incoming._meta ?? {}).filter((k) => !STUB_META.has(k));
  if (extra.length > 0 || sealed.length > 0) throw LOCK_REFUSALS.needsPassword;
  const color = incoming._meta?.color ?? null;
  const changed = incoming.name !== prev.name || color !== (prev._meta.color ?? null);
  prev.name = String(incoming.name ?? prev.name);
  if (color) prev._meta.color = color; else delete prev._meta.color;
  if (changed) prev._meta.rev += 1;
  return metaOf(prev);
}

/** As launch.rs: a protected profile starts only with its password. */
export async function launchCheck(state: MockState, id: unknown, password: unknown): Promise<void> {
  const p = state.profiles[String(id)];
  if (!p?._meta.lock) return;
  await sleep(KDF_MS);
  checkPassword(p, password);
}

/** The checks protect, unprotect and change share: its browser closed. */
function closed(state: MockState, id: string): void {
  if (state.running[id]) throw LOCK_REFUSALS.closeFirst;
}

const newPassword = (password: unknown): string => {
  if (typeof password !== "string" || [...password].length < PASSWORD_MIN) throw LOCK_REFUSALS.tooShort;
  return password;
};

export const lockHandlers: Record<string, Handler> = {
  profile_protect: async ({ id, password }, ctx) => {
    const p = profileOf(ctx.state, id);
    closed(ctx.state, p._meta.id);
    if (p._meta.lock) throw LOCK_REFUSALS.needsPassword;
    if (syncLink.of(p._meta.id)?.on) throw LOCK_REFUSALS.syncOn;
    const pw = newPassword(password);
    await sleep(KDF_MS);
    p._meta.lock = mockLock(pw);
    p._meta.rev += 1;
    ctx.save();
    return metaOf(p);
  },
  profile_unprotect: async ({ id, password }, ctx) => {
    const p = profileOf(ctx.state, id);
    closed(ctx.state, p._meta.id);
    await sleep(KDF_MS);
    checkPassword(p, password);
    delete p._meta.lock;
    p._meta.rev += 1;
    ctx.save();
    return metaOf(p);
  },
  profile_change_password: async ({ id, oldPassword, newPassword: next }, ctx) => {
    const p = profileOf(ctx.state, id);
    closed(ctx.state, p._meta.id);
    await sleep(KDF_MS);
    checkPassword(p, oldPassword);
    p._meta.lock = mockLock(newPassword(next));
    ctx.save();
    return null;
  },
  profile_unlock: async ({ id, password }, { state }) => {
    const p = profileOf(state, id);
    await sleep(KDF_MS);
    checkPassword(p, password);
    return structuredClone(p);
  },
};
