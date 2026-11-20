/// What makes a profile a phone, and the platform_version a phone may carry (#131).
/// No imports, so `npm run test:unit` runs it as it is.

type Block = Record<string, unknown> | null | undefined;
type Signals = { user_agent?: string; navigator?: Block; client_hints?: Block };

/** Whether a form or a stored config describes a phone, by the engine's three signals
 *  (profile.rs `claims_mobile`). A form carries only the user agent. */
export function claimsMobile(f: Signals): boolean {
  if (f.client_hints?.mobile === true) return true;
  if (/^android/i.test(String(f.client_hints?.platform ?? ""))) return true;
  return /android/i.test(String(f.user_agent ?? f.navigator?.user_agent ?? ""));
}

/** A version Chrome on Android reports: "<major>.0.0", Android 10 to 17 (platform_version.rs). */
export function isAndroidRelease(v: string): boolean {
  const m = /^(\d+)\.0\.0$/.exec(v);
  return !!m && Number(m[1]) >= 10 && Number(m[1]) <= 17;
}

type Versioned = { navigator?: Block; client_hints?: Block } | null | undefined;

/** A template's own platform_version, the one the engine reads first. */
export function templatePlatformVersion(payload: Versioned): string {
  const v = payload?.client_hints?.platform_version || payload?.navigator?.platform_version;
  return typeof v === "string" ? v : "";
}

/** The platform_version a template pick writes: the drawn one, else the template's own,
 *  never what the form held for the previous OS. */
export function pickedPlatformVersion(drawn: string | null | undefined, payload: Versioned): string {
  return drawn || templatePlatformVersion(payload);
}
