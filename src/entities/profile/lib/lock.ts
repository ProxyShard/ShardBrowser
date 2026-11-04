import { LOCK_REFUSALS } from "../../../shared/api/lockRefusals.ts";

/// Password-protected profiles (#127): what the list and the editor read off a
/// row or a stored profile. Free of runtime imports but the refusals, so
/// `npm run test:unit` runs it as it is.

/** The fewest characters a profile's password takes (the backend's rule). */
export const PASSWORD_MIN = 8;

/** A row whose browser data, fingerprint and proxy are sealed by its password. */
export const isProtected = (p: { protected?: boolean } | null | undefined): boolean => p?.protected === true;

/** What `profile_get` answers for a protected profile: its `_meta` and name, no fingerprint. Never an editor's base. */
export function isLockedStub(stored: unknown): boolean {
  if (!stored || typeof stored !== "object") return false;
  const s = stored as { _meta?: { lock?: unknown }; navigator?: unknown };
  return s._meta?.lock != null && s.navigator === undefined;
}

/** Why a new password is not taken yet: too short, or the repeat differs. Null when both are empty or fine. */
export function passwordProblem(password: string, repeat: string): "short" | "mismatch" | null {
  if (password === "" && repeat === "") return null;
  if ([...password].length < PASSWORD_MIN) return "short";
  return password === repeat ? null : "mismatch";
}

const says = (e: unknown, words: string) => String(e instanceof Error ? e.message : e).toLowerCase().includes(words);

/** The backend said the password was wrong. */
export const isWrongPassword = (e: unknown): boolean => says(e, LOCK_REFUSALS.wrongPassword);
/** The backend said the new password is too short. */
export const isTooShort = (e: unknown): boolean => says(e, LOCK_REFUSALS.tooShort);
