// Runtime cache: download ShardX engine + Widevine CDM + fingerprint
// library from the ProxyShard CDN, extract into a per-user cache dir,
// place Widevine inside the engine bundle, remember etags so subsequent
// runs are zero-network. Mirrors src-tauri/src/runtime.rs in the launcher.
import { closeSync, createWriteStream, existsSync, mkdirSync, openSync, readdirSync, readFileSync, readSync, renameSync, rmSync, writeFileSync, chmodSync, copyFileSync, cpSync, lstatSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { homedir, platform as osPlatform, arch as osArch } from "node:os";
import { join, dirname, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { spawnSync } from "node:child_process";
import AdmZip from "adm-zip";

export const PUB_BASE = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
export const CHROMIUM_VERSION = "152.0.7977.65";
/** This SDK's own version, compared against the manifest's `min_sdk_version`. */
export const SDK_VERSION = "2.0.3";
// Version manifest (GitHub raw) — one tiny GET yields every archive's current
// etag, so we never poll R2/S3 (no per-archive HEAD). Changed archives are then
// pulled from PUB_BASE.
export const MANIFEST_URL = "https://raw.githubusercontent.com/ProxyShard/ShardBrowser/main/runtime.json";

export function defaultCacheDir(): string {
  const plat = osPlatform();
  if (plat === "darwin") return join(homedir(), "Library", "Application Support", "shardx-sdk");
  if (plat === "win32")  return join(process.env.LOCALAPPDATA ?? homedir(), "shardx-sdk");
  return join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "shardx-sdk");
}

export interface Archive { key: string; label: string; }

export interface HostSpec {
  browser: Archive;
  widevine: Archive | null;
  binarySubpath: string[];
  widevineSubpath: string[];
}

export function hostSpec(): HostSpec {
  const plat = osPlatform();
  const arch = osArch();
  if (plat === "darwin" && arch === "arm64") {
    return {
      browser:  { key: "ShardX-Mac-arm64.zip",          label: "ShardX browser (macOS arm64)" },
      widevine: { key: "ShardX-Widevine-Mac-arm64.zip", label: "Widevine CDM" },
      binarySubpath:   ["ShardX-Mac-arm64", "ShardX.app", "Contents", "MacOS", "ShardX"],
      widevineSubpath: ["ShardX-Mac-arm64", "ShardX.app", "Contents", "Frameworks",
                        "ShardX Framework.framework", "Versions", CHROMIUM_VERSION,
                        "Libraries", "WidevineCdm"],
    };
  }
  if (plat === "win32" && arch === "x64") {
    return {
      browser:  { key: "ShardX-Windows.zip",     label: "ShardX browser (Windows x64)" },
      widevine: { key: "ShardX-Widevine-Win.zip", label: "Widevine CDM" },
      binarySubpath:   ["ShardX-Windows", "chrome.exe"],
      widevineSubpath: ["ShardX-Windows", "WidevineCdm"],
    };
  }
  if (plat === "linux" && arch === "x64") {
    return {
      browser:  { key: "ShardX-Linux.zip",         label: "ShardX browser (Linux x64)" },
      widevine: { key: "ShardX-Widevine-Linux.zip", label: "Widevine CDM" },
      binarySubpath:   ["ShardX-Linux", "chrome"],
      widevineSubpath: ["ShardX-Linux", "WidevineCdm"],
    };
  }
  throw new Error(`Unsupported host: ${plat}/${arch}. ShardX ships mac-arm64, win-x64, linux-x64.`);
}

export const FINGERPRINTS_ARCHIVE: Archive = {
  key: "ShardX-Fingerprints.zip",
  label: "Fingerprint library",
};
const FINGERPRINTS_TOP_DIR = "shardx-fingerprints";

export type ProgressCb = (label: string, received: number, total: number) => void;

interface Manifest {
  browser_etag?: string;
  widevine_etag?: string;
  fingerprints_etag?: string;
  /** Chromium version of the engine binary last extracted on disk. The update
   *  is detected by comparing this (or the on-disk version) to the manifest's
   *  chromium version — robust where the etag check failed. */
  installed_chromium_version?: string;
  /** Build stamp of the archive last extracted; fallback when it ships none. */
  installed_engine_build?: string;
  installed_grease_brand?: string;
  installed_grease_version?: string;
  installed_tls?: Record<string, unknown>;
}

const ACQUIRE_BROWSER_SESSION = Symbol("acquireBrowserSession");

interface RuntimeActivity {
  activeBrowsers: number;
  idleWaiters: Array<() => void>;
}

const runtimeActivities = new WeakMap<Runtime, RuntimeActivity>();

interface StagedReplacement {
  stagedPath: string;
  targetPath: string;
  backupPath: string;
}

class RuntimeRollbackError extends AggregateError {}

export class Runtime {
  readonly root: string;
  readonly spec: HostSpec;
  private readonly progress?: ProgressCb;
  private readonly _profilesRoot?: string;
  /** Set after a successful in-process install() so subsequent launches
   *  skip the R2 HEAD round-trip (~1 s over a clean connection).  Cleared
   *  by `install({force: true})`. */
  private _checkedInProcess = false;
  private _installPromise?: Promise<void>;
  private _installForceRequested = false;
  private _installForceActive = false;
  private _activationPending = false;
  private _launchWaiters: Array<() => void> = [];
  private _recoveryError?: Error;
  /** Engine chromium version from the manifest (fallback to the build-time
   *  constant). Used by launch to normalise profile UA + client_hints. */
  private _chromiumVersion: string = CHROMIUM_VERSION;
  /** GREASE brand/version from the manifest (rotates per release; can't be
   *  derived from the version number). Applied to profiles on launch. */
  private _greaseBrand?: string;
  private _greaseVersion?: string;
  /** TLS block from the manifest (signature_algorithms / cipher_suites).
   *  Applied to profiles on launch so the JA4 hash keeps matching the Chrome
   *  version the profile claims — 152 added the ML-DSA signature schemes, and
   *  a profile still carrying the previous list is a mismatch. */
  private _tls?: Record<string, unknown>;

  constructor(opts: { cacheDir?: string; progress?: ProgressCb; profilesDir?: string } = {}) {
    this.root = opts.cacheDir ?? defaultCacheDir();
    mkdirSync(this.root, { recursive: true });
    this._profilesRoot = opts.profilesDir ? resolve(opts.profilesDir) : undefined;
    this.progress = opts.progress;
    this.spec = hostSpec();
    runtimeActivities.set(this, { activeBrowsers: 0, idleWaiters: [] });
    const unfinishedUpdate = readdirSync(this.root, { withFileTypes: true })
      .find((entry) => entry.isDirectory() && entry.name.startsWith(".update-"));
    if (unfinishedUpdate) {
      this._recoveryError = new Error(
        `unfinished runtime update requires recovery: ${join(this.root, unfinishedUpdate.name)}`,
      );
    }
  }

  get manifestPath(): string  { return join(this.root, "manifest.json"); }
  get binaryPath(): string    { return join(this.root, ...this.spec.binarySubpath); }
  get fingerprintsDir(): string {
    const d = join(this.root, "fingerprints");
    mkdirSync(d, { recursive: true });
    return d;
  }
  /** Per-profile user-data-dir root. Defaults to `<cacheDir>/profiles/`;
   *  override via `new ShardX({ profilesDir })` or per-launch
   *  `userDataDir`. Resolved path is logged at launch time. */
  get profilesRoot(): string {
    const d = this._profilesRoot ?? join(this.root, "profiles");
    mkdirSync(d, { recursive: true });
    return d;
  }
  get installed(): boolean    { return existsSync(this.binaryPath); }
  /** Engine chromium version (manifest-driven; set on install()). */
  get chromiumVersion(): string { return this._chromiumVersion; }
  /** GREASE brand from the manifest (e.g. "Not)A;Brand"); set on install(). */
  get greaseBrand(): string | undefined { return this._greaseBrand; }
  /** GREASE version from the manifest (e.g. "24"); set on install(). */
  get greaseVersion(): string | undefined { return this._greaseVersion; }
  /** TLS overrides from the manifest; set on install(). */
  get tls(): Record<string, unknown> | undefined { return this._tls; }

  /** Chromium version of the engine actually on disk (mac Framework
   *  `Versions/<ver>/`, win `<ver>.manifest`), or undefined on Linux. */
  private installedEngineVersion(): string | undefined {
    try {
      const plat = osPlatform();
      if (plat === "darwin") {
        const versions = join(this.root, "ShardX-Mac-arm64", "ShardX.app", "Contents",
          "Frameworks", "ShardX Framework.framework", "Versions");
        const v = readdirSync(versions).find((n) => n !== "Current" && /^\d/.test(n));
        return v;
      }
      if (plat === "win32") {
        // Only accept a `<version>.manifest` whose stem parses as a version,
        // so a stray/leftover manifest can't feed a bogus version.
        return readdirSync(join(this.root, "ShardX-Windows"))
          .filter((f) => f.endsWith(".manifest"))
          .map((f) => f.replace(/\.manifest$/, ""))
          .find((s) => /^\d/.test(s) && s.includes("."));
      }
      return undefined; // linux: no on-disk version marker
    } catch { return undefined; }
  }

  /** Effective installed version. Trusts the version recorded at install time
   *  (authoritative — written only after a successful extract) over re-reading
   *  it off disk, which can carry stale files from a previous version. On-disk
   *  detection is the fallback for legacy installs with no recorded version. */
  private effectiveInstalledVersion(local: Manifest): string | undefined {
    return local.installed_chromium_version ?? this.installedEngineVersion();
  }

  /** Build stamp on disk: `<engine>/shardx-build` when the archive ships one,
   *  else what was recorded at install time. */
  private installedEngineBuild(local: Manifest): string | undefined {
    try {
      const stamp = readFileSync(
        join(this.root, this.spec.binarySubpath[0], "shardx-build"), "utf8",
      ).trim();
      if (stamp) return stamp;
    } catch { /* archive predates the stamp */ }
    return local.installed_engine_build || undefined;
  }

  /** Dotted-numeric compare; non-numeric parts count as 0. */
  private static versionLt(a: string, b: string): boolean {
    const parts = (v: string) =>
      v.split(/[.\-+]/).map((p) => (/^\d+$/.test(p) ? Number(p) : 0));
    const pa = parts(a), pb = parts(b);
    for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
      const x = pa[i] ?? 0, y = pb[i] ?? 0;
      if (x !== y) return x < y;
    }
    return false;
  }

  /** Chromium version, `engine_build`, or the archive hash — any one of them
   *  means the engine moved. An unknown local value is not a mismatch. */
  private engineOutdated(
    local: Manifest,
    remote: { archives: Record<string, string>; chromiumVersion?: string; engineBuild?: string },
  ): boolean {
    if (remote.chromiumVersion !== undefined
        && this.effectiveInstalledVersion(local) !== remote.chromiumVersion) {
      return true;
    }
    const differs = (have?: string, want?: string) => !!have && !!want && have !== want;
    if (differs(this.installedEngineBuild(local), remote.engineBuild)) return true;
    return differs(local.browser_etag, remote.archives[this.spec.browser.key]);
  }

  // ---- manifest ----

  private loadManifest(): Manifest {
    try { return JSON.parse(readFileSync(this.manifestPath, "utf8")); }
    catch { return {}; }
  }
  private saveManifest(m: Manifest, stagingRoot = this.root): void {
    const tmp = join(stagingRoot, `manifest-${randomUUID()}.tmp`);
    try {
      writeFileSync(tmp, JSON.stringify(m, null, 2));
      renameSync(tmp, this.manifestPath);
    } finally {
      rmSync(tmp, { force: true });
    }
  }

  // ---- install ----

  async install(opts: { force?: boolean } = {}): Promise<void> {
    this.throwIfRecoveryRequired();
    const force = !!opts.force;
    if (this._installPromise) {
      if (force && !this._installForceActive) this._installForceRequested = true;
      return this._installPromise;
    }
    if (this._checkedInProcess && !force) return;

    const pending = (async () => {
      let nextForce = force;
      do {
        this._installForceRequested = false;
        this._installForceActive = nextForce;
        await this.installOnce(nextForce);
        nextForce = this._installForceRequested;
      } while (nextForce);
    })();
    this._installPromise = pending;
    try {
      await pending;
    } finally {
      if (this._installPromise === pending) {
        this._installPromise = undefined;
        this._installForceActive = false;
        this._installForceRequested = false;
      }
    }
  }

  private async installOnce(force: boolean): Promise<void> {
    const previous = this.loadManifest();
    const local = { ...previous };
    const remote = await this.fetchManifest();

    // Refused rather than installed: this SDK could not configure it.
    if (remote.minSdkVersion && Runtime.versionLt(SDK_VERSION, remote.minSdkVersion)) {
      throw new Error(
        `this engine build needs @proxyshard/shardx ${remote.minSdkVersion} or newer — `
        + `you are on ${SDK_VERSION}; upgrade the SDK first`,
      );
    }
    // Unknown is never a mismatch, so adopt a stamp once or the first bump
    // never lands.
    if (this.installed && remote.engineBuild && !this.installedEngineBuild(local)) {
      local.installed_engine_build = remote.engineBuild;
    }
    // Manifest unreachable → don't force a re-download.
    const needBrowser = force || !this.installed || this.engineOutdated(local, remote);
    const needWidevine = !!this.spec.widevine && (needBrowser || !local.widevine_etag);
    const remoteFp = remote.archives[FINGERPRINTS_ARCHIVE.key];
    const fingerprintsPath = this.fingerprintsDir;
    const fpDirHasJson = readdirSync(fingerprintsPath).some((f) => f.endsWith(".json"));
    const needFingerprints = force || !fpDirHasJson
      || (remoteFp !== undefined && local.fingerprints_etag !== remoteFp);

    if (!needBrowser && !needWidevine && !needFingerprints) {
      if (remote.chromiumVersion !== undefined) {
        local.installed_chromium_version = remote.chromiumVersion;
      }
      local.installed_chromium_version ??= CHROMIUM_VERSION;
      if (remote.greaseBrand !== undefined) local.installed_grease_brand = remote.greaseBrand;
      if (remote.greaseVersion !== undefined) local.installed_grease_version = remote.greaseVersion;
      if (remote.tls !== undefined) local.installed_tls = remote.tls;
      this.saveManifest(local);
      this.applyInstalledMetadata(local);
      this._checkedInProcess = true;
      return;
    }

    const stagingRoot = join(this.root, `.update-${randomUUID()}`);
    mkdirSync(stagingRoot, { recursive: true });
    let activationSucceeded = false;
    let preserveStagingForRecovery = false;
    try {
      const replacements: StagedReplacement[] = [];
      const backups = join(stagingRoot, ".backups");
      mkdirSync(backups);

      if (needBrowser) {
        local.browser_etag = await this.downloadAndExtract(this.spec.browser, stagingRoot);
        if (remote.engineBuild) local.installed_engine_build = remote.engineBuild;
        if (this.spec.widevine) {
          local.widevine_etag = await this.downloadAndExtract(this.spec.widevine, stagingRoot);
          this.placeWidevine(stagingRoot);
        }

        const stagedBinary = join(stagingRoot, ...this.spec.binarySubpath);
        if (!existsSync(stagedBinary) || !lstatSync(stagedBinary).isFile()
            || !hasExpectedExecutableSignature(stagedBinary)) {
          throw new Error(`runtime archive is missing a valid executable: ${this.spec.binarySubpath.join("/")}`);
        }
        if (osPlatform() !== "win32") fixUnixExecBits(stagingRoot);
        replacements.push({
          stagedPath: join(stagingRoot, this.spec.binarySubpath[0]),
          targetPath: join(this.root, this.spec.binarySubpath[0]),
          backupPath: join(backups, "engine"),
        });
      } else if (needWidevine && this.spec.widevine) {
        local.widevine_etag = await this.downloadAndExtract(this.spec.widevine, stagingRoot);
        this.placeWidevine(stagingRoot);
        replacements.push({
          stagedPath: join(stagingRoot, ...this.spec.widevineSubpath),
          targetPath: join(this.root, ...this.spec.widevineSubpath),
          backupPath: join(backups, "widevine"),
        });
      }

      if (needFingerprints) {
        const stagedFingerprints = join(stagingRoot, "fingerprints");
        cpSync(fingerprintsPath, stagedFingerprints, { recursive: true });
        await this.installFingerprints(stagedFingerprints);
        if (!readdirSync(stagedFingerprints).some((f) => f.endsWith(".json"))) {
          throw new Error("fingerprint archive contains no JSON profiles");
        }
        if (remoteFp !== undefined) local.fingerprints_etag = remoteFp;
        replacements.push({
          stagedPath: stagedFingerprints,
          targetPath: fingerprintsPath,
          backupPath: join(backups, "fingerprints"),
        });
      }

      if (remote.chromiumVersion !== undefined) {
        local.installed_chromium_version = remote.chromiumVersion;
      } else if (needBrowser) {
        local.installed_chromium_version = CHROMIUM_VERSION;
      }
      if (remote.greaseBrand !== undefined) local.installed_grease_brand = remote.greaseBrand;
      else if (needBrowser) delete local.installed_grease_brand;
      if (remote.greaseVersion !== undefined) local.installed_grease_version = remote.greaseVersion;
      else if (needBrowser) delete local.installed_grease_version;
      if (remote.tls !== undefined) local.installed_tls = remote.tls;
      else if (needBrowser) delete local.installed_tls;
      await this.activateWhenIdle(replacements, local, stagingRoot);
      activationSucceeded = true;
      this._checkedInProcess = true;
    } catch (error) {
      preserveStagingForRecovery = error instanceof RuntimeRollbackError;
      if (!activationSucceeded && !(error instanceof RuntimeRollbackError)
          && this.hasUsableInstalledRuntime()) {
        this.applyInstalledMetadata(previous);
        this._checkedInProcess = true;
        if (!force) {
          const message = error instanceof Error ? error.message : String(error);
          console.warn(`[shardx] runtime update failed; keeping installed runtime: ${message}`);
          return;
        }
      }
      throw error;
    } finally {
      try {
        const backupRoot = join(stagingRoot, ".backups");
        const hasUnrestoredBackups = existsSync(backupRoot) && readdirSync(backupRoot).length > 0;
        if (activationSucceeded || (!hasUnrestoredBackups && !preserveStagingForRecovery)) {
          rmSync(stagingRoot, { recursive: true, force: true });
        }
      } catch (cleanupError) {
        const message = cleanupError instanceof Error ? cleanupError.message : String(cleanupError);
        console.warn(`[shardx] runtime update staging cleanup failed at ${stagingRoot}: ${message}`);
      }
    }
  }

  private applyInstalledMetadata(local: Manifest): void {
    this._chromiumVersion = local.installed_chromium_version
      ?? this.installedEngineVersion()
      ?? CHROMIUM_VERSION;
    this._greaseBrand = local.installed_grease_brand;
    this._greaseVersion = local.installed_grease_version;
    this._tls = local.installed_tls;
  }

  private hasUsableInstalledRuntime(): boolean {
    try {
      if (!lstatSync(this.binaryPath).isFile()
          || !hasExpectedExecutableSignature(this.binaryPath)) return false;
      return readdirSync(this.fingerprintsDir).some((name) => {
        if (!name.endsWith(".json")) return false;
        try {
          const path = join(this.fingerprintsDir, name);
          if (!lstatSync(path).isFile()) return false;
          JSON.parse(readFileSync(path, "utf8"));
          return true;
        } catch {
          return false;
        }
      });
    } catch {
      return false;
    }
  }

  private throwIfRecoveryRequired(): void {
    if (!this._recoveryError) return;
    throw new Error(
      `runtime cache requires recovery after an interrupted or incomplete update: ${this._recoveryError.message}`,
      { cause: this._recoveryError },
    );
  }

  private async waitForBrowserSessions(): Promise<void> {
    const activity = runtimeActivities.get(this)!;
    while (activity.activeBrowsers > 0) {
      await new Promise<void>((resolve) => activity.idleWaiters.push(resolve));
    }
  }

  private async activateWhenIdle(
    replacements: StagedReplacement[],
    manifest: Manifest,
    stagingRoot: string,
  ): Promise<void> {
    this._activationPending = true;
    try {
      await this.waitForBrowserSessions();
      this.activateStagedUpdate(replacements, manifest, stagingRoot);
      this.applyInstalledMetadata(manifest);
    } finally {
      this._activationPending = false;
      for (const resolve of this._launchWaiters.splice(0)) resolve();
    }
  }

  async [ACQUIRE_BROWSER_SESSION](): Promise<() => void> {
    while (this._activationPending) {
      await new Promise<void>((resolve) => this._launchWaiters.push(resolve));
    }
    this.throwIfRecoveryRequired();
    const activity = runtimeActivities.get(this)!;
    activity.activeBrowsers++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      activity.activeBrowsers--;
      if (activity.activeBrowsers === 0) {
        for (const resolve of activity.idleWaiters.splice(0)) resolve();
      }
    };
  }

  private activateStagedUpdate(
    replacements: StagedReplacement[],
    manifest: Manifest,
    stagingRoot: string,
  ): void {
    const moved: Array<StagedReplacement & { backedUp: boolean; activated: boolean }> = [];
    try {
      for (const replacement of replacements) {
        const state = { ...replacement, backedUp: false, activated: false };
        moved.push(state);
        if (existsSync(replacement.targetPath)) {
          renameSync(replacement.targetPath, replacement.backupPath);
          state.backedUp = true;
        }
        mkdirSync(dirname(replacement.targetPath), { recursive: true });
        renameSync(replacement.stagedPath, replacement.targetPath);
        state.activated = true;
      }
      this.saveManifest(manifest, stagingRoot);
    } catch (error) {
      const rollbackErrors: unknown[] = [];
      for (const state of moved.reverse()) {
        try {
          if (state.backedUp && !existsSync(state.backupPath)) {
            throw new Error(`runtime backup is missing: ${state.backupPath}`);
          }
          if (state.activated && existsSync(state.targetPath)) {
            rmSync(state.targetPath, { recursive: true, force: true });
          }
          if (state.backedUp) {
            mkdirSync(dirname(state.targetPath), { recursive: true });
            renameSync(state.backupPath, state.targetPath);
          }
        } catch (rollbackError) {
          rollbackErrors.push(rollbackError);
        }
      }
      if (rollbackErrors.length > 0) {
        const recoveryError = new RuntimeRollbackError(
          [error, ...rollbackErrors],
          `runtime update failed and rollback was incomplete; inspect ${stagingRoot} for recovery data`,
        );
        this._recoveryError = recoveryError;
        throw recoveryError;
      }
      throw error;
    }
  }

  // ---- helpers ----

  /** Fetch the version manifest (GitHub raw) — one request that yields every
   *  archive's current etag + the chromium version, replacing per-archive HEADs
   *  against R2/S3. Empty archives / undefined version when unreachable. */
  private async fetchManifest(): Promise<{ archives: Record<string, string>; chromiumVersion?: string; engineBuild?: string; minSdkVersion?: string; greaseBrand?: string; greaseVersion?: string; tls?: Record<string, unknown> }> {
    try {
      const r = await fetch(MANIFEST_URL);
      if (!r.ok) return { archives: {} };
      const data = await r.json() as { archives?: Record<string, string>; chromium_version?: string; engine_build?: string | number; min_sdk_version?: string; grease_brand?: string; grease_version?: string; tls?: Record<string, unknown> };
      const str = (v: unknown) => (typeof v === "string" ? v : undefined);
      return {
        archives: (data && typeof data.archives === "object" && data.archives) || {},
        chromiumVersion: str(data?.chromium_version),
        // Written as a number or a string; both mean the same thing.
        engineBuild: typeof data?.engine_build === "number"
          ? String(data.engine_build)
          : str(data?.engine_build),
        minSdkVersion: str(data?.min_sdk_version),
        greaseBrand: str(data?.grease_brand),
        greaseVersion: str(data?.grease_version),
        tls: (data?.tls && typeof data.tls === "object" && !Array.isArray(data.tls))
          ? data.tls as Record<string, unknown>
          : undefined,
      };
    } catch { return { archives: {} }; }
  }

  private async downloadAndExtract(arch: Archive, dest: string): Promise<string> {
    const url = `${PUB_BASE}/${arch.key}`;
    mkdirSync(dest, { recursive: true });
    const tmp = join(dest, `.${arch.key}.tmp`);

    const r = await fetch(url);
    if (!r.ok || !r.body) throw new Error(`download ${arch.key}: HTTP ${r.status}`);
    const etag = r.headers.get("etag")?.replace(/^"|"$/g, "") ?? "";
    const total = Number(r.headers.get("content-length") ?? 0);

    let received = 0;
    const reader = r.body.getReader();
    const out = createWriteStream(tmp);
    const stream = new Readable({
      async read() {
        const { value, done } = await reader.read();
        if (done) { this.push(null); return; }
        received += value.byteLength;
        if (arch.label) {/* throttle: rounded percent */}
        this.push(Buffer.from(value));
      },
    });
    // Wire progress in a parallel listener so pipeline stays clean.
    if (this.progress) {
      stream.on("data", () => this.progress!(arch.label, received, total));
    }
    await pipeline(stream, out);
    if (Number.isFinite(total) && total > 0 && received !== total) {
      throw new Error(`download ${arch.key}: incomplete response (${received}/${total} bytes)`);
    }

    // Extract.  IMPORTANT: on macOS/Linux shell out to the system
    // `unzip` instead of adm-zip — adm-zip writes symlinks as ordinary
    // text files (every `Versions/Current/...` link in a `.app`
    // framework becomes a 24-byte regular file) and drops the +x bit
    // on every helper executable.  The result extracts cleanly but
    // fails to launch — GPU helper can't find the framework dylib.
    if (osPlatform() === "win32") {
      new AdmZip(tmp).extractAllTo(dest, /*overwrite*/ true);
    } else {
      systemUnzip(tmp, dest);
    }
    rmSync(tmp, { force: true });
    return etag;
  }

  private placeWidevine(root: string): void {
    if (!this.spec.widevine) return;
    const wrapper = this.spec.widevine.key.replace(/\.zip$/, "");
    const src = join(root, wrapper, "WidevineCdm");
    if (!existsSync(src)) throw new Error(`Widevine archive is missing ${wrapper}/WidevineCdm`);
    const dst = join(root, ...this.spec.widevineSubpath);
    if (existsSync(dst)) rmSync(dst, { recursive: true, force: true });
    mkdirSync(dirname(dst), { recursive: true });
    renameSync(src, dst);
    rmSync(join(root, wrapper), { recursive: true, force: true });
    if (!lstatSync(dst).isDirectory() || readdirSync(dst).length === 0) {
      throw new Error("Widevine archive did not produce the expected runtime files");
    }
  }

  private async installFingerprints(destination: string): Promise<void> {
    const url = `${PUB_BASE}/${FINGERPRINTS_ARCHIVE.key}`;
    const staging = join(destination, ".staging");
    mkdirSync(staging, { recursive: true });
    const tmp = join(staging, "bundle.zip");
    try {
      const r = await fetch(url);
      if (!r.ok || !r.body) throw new Error(`download fingerprints: HTTP ${r.status}`);
      const total = Number(r.headers.get("content-length") ?? 0);

      let received = 0;
      const reader = r.body.getReader();
      const out = createWriteStream(tmp);
      const stream = new Readable({
        async read() {
          const { value, done } = await reader.read();
          if (done) { this.push(null); return; }
          received += value.byteLength;
          this.push(Buffer.from(value));
        },
      });
      if (this.progress) {
        stream.on("data", () => this.progress!(FINGERPRINTS_ARCHIVE.label, received, total));
      }
      await pipeline(stream, out);
      if (Number.isFinite(total) && total > 0 && received !== total) {
        throw new Error(`download fingerprints: incomplete response (${received}/${total} bytes)`);
      }

      // Fingerprints bundle is plain JSON files — adm-zip is fine here.
      new AdmZip(tmp).extractAllTo(staging, true);

      const srcDir = join(staging, FINGERPRINTS_TOP_DIR);
      const walk = existsSync(srcDir) ? srcDir : staging;
      const names = readdirSync(walk).filter((name) => name.endsWith(".json"));
      if (names.length === 0) throw new Error("fingerprint archive contains no JSON profiles");
      for (const name of names) {
        const src = join(walk, name);
        JSON.parse(readFileSync(src, "utf8"));
        // Bundled templates overwrite their old copies; user-added JSON files stay intact.
        const target = join(destination, name);
        try {
          if (lstatSync(target).isSymbolicLink()) rmSync(target);
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        }
        copyFileSync(src, target);
      }
    } finally {
      rmSync(staging, { recursive: true, force: true });
    }
  }
}

export function acquireBrowserSessionLease(runtime: Runtime): Promise<() => void> {
  return runtime[ACQUIRE_BROWSER_SESSION]();
}

/** Extract via /usr/bin/unzip — preserves symlinks and permission
 *  bits that adm-zip silently drops.  Required for any macOS .app
 *  bundle (Versions/Current symlinks + Helper exec bits).
 *
 *  Accepts exit code 0 (clean) and 1 (warnings — e.g. "backslashes in
 *  path" for archives zipped on Windows; extraction still completes
 *  correctly).  Only 2+ are real fatal errors per unzip(1). */
function systemUnzip(archive: string, dest: string): void {
  mkdirSync(dest, { recursive: true });
  const r = spawnSync("unzip", ["-q", "-o", archive, "-d", dest], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  if (r.error) {
    if ((r.error as NodeJS.ErrnoException).code === "ENOENT") {
      throw new Error(
        "system `unzip` not found — install with `apt install unzip` / `brew install unzip`",
      );
    }
    throw r.error;
  }
  if ((r.status ?? 0) > 1) {
    const err = r.stderr?.toString().slice(0, 400) ?? `exit ${r.status}`;
    throw new Error(`unzip failed for ${archive} (exit ${r.status}): ${err}`);
  }
}

/** ELF + Mach-O magic bytes; first 4 bytes tell us a file is a native
 *  executable that needs the +x bit, regardless of what zip stored. */
const NATIVE_MAGIC: ReadonlyArray<Buffer> = [
  Buffer.from([0x7f, 0x45, 0x4c, 0x46]),                  // ELF
  Buffer.from([0xfe, 0xed, 0xfa, 0xcf]),                  // Mach-O 64 BE
  Buffer.from([0xcf, 0xfa, 0xed, 0xfe]),                  // Mach-O 64 LE
  Buffer.from([0xfe, 0xed, 0xfa, 0xce]),                  // Mach-O 32 BE
  Buffer.from([0xce, 0xfa, 0xed, 0xfe]),                  // Mach-O 32 LE
  Buffer.from([0xca, 0xfe, 0xba, 0xbe]),                  // Mach-O universal BE
  Buffer.from([0xbe, 0xba, 0xfe, 0xca]),                  // Mach-O universal LE
];

function hasExpectedExecutableSignature(path: string): boolean {
  const fd = openSync(path, "r");
  const header = Buffer.alloc(4);
  try {
    if (readSync(fd, header, 0, header.length, 0) !== header.length) return false;
  } finally {
    closeSync(fd);
  }
  if (osPlatform() === "win32") return header[0] === 0x4d && header[1] === 0x5a;
  if (osPlatform() === "linux") return header.equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]));
  return NATIVE_MAGIC.some((magic) => header.equals(magic));
}

/** Walk `root` and add +x to every file whose first 4 bytes match a known
 *  native-binary magic.  Required because Windows zip producers don't
 *  store Unix exec bits, so chrome / chrome_crashpad_handler / chrome_sandbox
 *  all come out non-executable on Linux. */
function fixUnixExecBits(root: string): void {
  const walk = (dir: string): void => {
    for (const ent of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, ent.name);
      if (ent.isSymbolicLink()) continue;
      if (ent.isDirectory()) { walk(p); continue; }
      if (!ent.isFile()) continue;
      try {
        const fd = openSync(p, "r");
        const buf = Buffer.alloc(4);
        readSync(fd, buf, 0, 4, 0);
        closeSync(fd);
        if (NATIVE_MAGIC.some((m) => buf.equals(m))) {
          chmodSync(p, lstatSync(p).mode | 0o111);
        }
      } catch { /* skip unreadable / racing files */ }
    }
  };
  walk(root);
}
