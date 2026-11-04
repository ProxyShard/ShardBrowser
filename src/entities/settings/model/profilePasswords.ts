import { create } from "zustand";
import { settingsGet } from "./api";

/// Settings > Profiles > Protect profiles with a password, as the profile list reads it:
/// read once at start, and set again by each save of the settings.
export const useProfilePasswords = create<{ on: boolean }>(() => ({ on: false }));

let asked = false;
/** Reads the switch once; a failed read leaves it off and tries again next time. */
export function loadProfilePasswords(): void {
  if (asked) return;
  asked = true;
  void settingsGet().then((s) => useProfilePasswords.setState({ on: s.profile_passwords === true }), () => { asked = false; });
}

/** Whether a password may be set on a profile now. */
export const passwordsOn = (): boolean => useProfilePasswords.getState().on;
