/// A password-protected profile's refusals in the backend's own words (#127 contract):
/// the mock words its refusals with these, and errors.ts reads them back.
/// Kept free of imports, so node tests and the mock read it as it is.

export const LOCK_REFUSALS = {
  /** The call needs the profile's password and got none. */
  needsPassword: "this profile is protected by a password",
  wrongPassword: "wrong password",
  /** Fewer than `PASSWORD_MIN` characters. */
  tooShort: "the password is too short",
  /** Protect, unprotect or change while its browser runs. */
  closeFirst: "close the profile first",
  /** Protect while personal sync carries it. */
  syncOn: "turn off sync for this profile first",
  /** Clone, cookies, a synced or automation start, personal sync, a proxy assigned while locked. */
  notAvailable: "not available for a password-protected profile",
} as const;
