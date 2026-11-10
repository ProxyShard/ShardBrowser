// Runtime cache: download ShardX engine + Widevine CDM + fingerprint
// library from the ProxyShard CDN, extract into a per-user cache dir,
// place Widevine inside the engine bundle, remember etags so subsequent
// runs are zero-network. Mirrors src-tauri/src/runtime.rs in the launcher.
import { closeSync, createWriteStream, existsSync, fstatSync, fsyncSync, linkSync, mkdirSync, openSync, readdirSync, readFileSync, readlinkSync, readSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync, writeSync, chmodSync, copyFileSync, lstatSync } from "node:fs";
import * as nodeFs from "node:fs";
import { mkdir } from "node:fs/promises";
import { homedir, hostname, platform as osPlatform, arch as osArch } from "node:os";
import { join, dirname, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import AdmZip from "adm-zip";

export const PUB_BASE = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
export const CHROMIUM_VERSION = "157.0.8103.50";
/** This SDK's own version, compared against the manifest's `min_sdk_version`. */
export const SDK_VERSION = "2.1.6";
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
  /** What the manifest said about the installed engine, kept for runs that
   *  cannot take it from the manifest: offline, or skipping an update this
   *  SDK is too old for. */
  installed_grease_brand?: string;
  installed_grease_version?: string;
  installed_android_chromium_version?: string;
  installed_tls?: Record<string, unknown>;
}

export class Runtime {
  readonly root: string;
  readonly spec: HostSpec;
  private readonly progress?: ProgressCb;
  private readonly _profilesRoot?: string;
  /** Set after a successful in-process install() so subsequent launches
   *  skip the R2 HEAD round-trip (~1 s over a clean connection).  Cleared
   *  by `install({force: true})`. */
  private _checkedInProcess = false;
  /** Engine chromium version from the manifest (fallback to the build-time
   *  constant). Used by launch to normalise profile UA + client_hints. */
  private _chromiumVersion: string = CHROMIUM_VERSION;
  /** GREASE brand/version from the manifest (rotates per release; can't be
   *  derived from the version number). Applied to profiles on launch. */
  private _greaseBrand?: string;
  private _greaseVersion?: string;
  /** Chrome for Android build of the engine's major, for phone profiles. */
  private _androidVersion?: string;
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
  /** Chrome for Android build phone profiles report; set on install(). */
  get androidChromiumVersion(): string | undefined { return this._androidVersion; }
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
  private saveManifest(m: Manifest): void {
    // Whole or not at all: other processes read it (adoptInstalled).
    const tmp = `${this.manifestPath}.tmp`;
    writeFileSync(tmp, JSON.stringify(m, null, 2));
    renameSync(tmp, this.manifestPath);
  }

  // ---- install ----

  async install(opts: { force?: boolean } = {}): Promise<void> {
    const force = !!opts.force;
    if (this._checkedInProcess && !force) { this.adoptInstalled(); return; }
    // One install at a time over this cache, across processes and the SDKs.
    const lock = await InstallLock.acquire(this.root);
    try {
      // Another call of this process did it while this one waited.
      if (this._checkedInProcess && !force) { this.adoptInstalled(); return; }
      await this.installLocked(force, lock);
    } finally {
      lock.release();
    }
  }

  /** Another process may have swapped the engine since this one checked: the
   *  version profiles claim follows the engine on disk, from its record. */
  private adoptInstalled(): void {
    const local = this.loadManifest();
    const v = local.installed_chromium_version;
    if (!v || v === this._chromiumVersion) return;
    this._chromiumVersion = v;
    this._greaseBrand = local.installed_grease_brand;
    this._greaseVersion = local.installed_grease_version;
    this._androidVersion = local.installed_android_chromium_version;
    this._tls = local.installed_tls;
  }

  private async installLocked(force: boolean, lock: InstallLock): Promise<void> {
    const top = this.spec.binarySubpath[0];
    // A run that died between the two renames of an update left the engine
    // as `.previous`: put back before anything asks whether it is installed.
    recoverEngine(this.root, top);
    sweepLeftovers(this.root, top, this.spec.browser.key, this.spec.binarySubpath);
    const local = this.loadManifest();
    const remote = await this.fetchManifest();
    // An engine this SDK is too old to drive is not installed, but the one
    // already on disk keeps running. Refusing outright stopped every run on an
    // engine that worked yesterday, for an update nobody had asked for. With
    // nothing installed, or a reinstall asked for, there is nothing to keep.
    const tooOld = !!remote.minSdkVersion && Runtime.versionLt(SDK_VERSION, remote.minSdkVersion);
    if (tooOld && (force || !this.installed)) {
      throw new Error(
        `this engine build needs @proxyshard/shardx ${remote.minSdkVersion} or newer — `
        + `you are on ${SDK_VERSION}; upgrade the SDK first`,
      );
    }
    // Remember the engine version + grease so launch can normalise profiles.
    // Without the manifest, what is ALREADY installed — not the version this
    // SDK was published with. Falling back to the constant made an offline
    // run record 152 over an engine that is 156, and profiles then declared
    // a version the engine does not have. Skipping an update, the manifest
    // describes an engine that is not the one on disk, so it is not read.
    const offered = tooOld ? undefined : remote;
    this._chromiumVersion =
      offered?.chromiumVersion ?? this.effectiveInstalledVersion(local) ?? CHROMIUM_VERSION;
    this._greaseBrand = offered?.greaseBrand ?? local.installed_grease_brand;
    this._greaseVersion = offered?.greaseVersion ?? local.installed_grease_version;
    this._androidVersion = offered?.androidChromiumVersion ?? local.installed_android_chromium_version;
    this._tls = offered?.tls ?? local.installed_tls;
    if (tooOld) {
      console.warn(
        `[shardx] engine ${remote.chromiumVersion ?? "update"} needs @proxyshard/shardx `
        + `${remote.minSdkVersion} or newer (this is ${SDK_VERSION}); staying on the installed `
        + `engine ${this._chromiumVersion}. Upgrade the SDK to get it.`,
      );
      this._checkedInProcess = true;
      return;
    }
    // Unknown is never a mismatch, so adopt a stamp once or the first bump
    // never lands.
    if (this.installed && remote.engineBuild && !this.installedEngineBuild(local)) {
      local.installed_engine_build = remote.engineBuild;
    }
    // Manifest unreachable → don't force a re-download.
    const needBrowser = force || !this.installed || this.engineOutdated(local, remote);
    let swapped = false;
    if (needBrowser) {
      // Nothing installed is touched until a complete new tree is unpacked
      // beside it (see installEngine). The old tree still has to go whole — a
      // leftover `<old>.manifest` or a stale lib beside the new ones pinned the
      // detected version and the SDK re-downloaded for ever — but a download or
      // an archive that fails must never be what removes it: that left the
      // machine with no engine and nothing to fall back to.
      try {
        local.browser_etag = await this.installEngine(remote.archives[this.spec.browser.key], force, lock);
        swapped = true;
      } catch (e) {
        // With nothing installed, or a reinstall asked for, there is nothing to stay on.
        if (force || !this.installed) throw e;
        // The engine on disk works: stay on it, as for an update this SDK is too old for.
        this._chromiumVersion = this.effectiveInstalledVersion(local) ?? CHROMIUM_VERSION;
        this._greaseBrand = local.installed_grease_brand;
        this._greaseVersion = local.installed_grease_version;
        this._androidVersion = local.installed_android_chromium_version;
        this._tls = local.installed_tls;
        console.warn(
          `[shardx] the engine update was not installed: ${(e as Error).message}. Staying on the `
          + `installed engine ${this._chromiumVersion}; the update is tried again on the next run.`,
        );
      }
    }
    if (swapped) {
      if (remote.engineBuild) local.installed_engine_build = remote.engineBuild;
      // Saved at once: the engine on disk is the new one, whatever fails next.
      // The new tree has no CDM yet, so the old one's etag must not say it has.
      local.installed_chromium_version = this._chromiumVersion;
      if (remote.greaseBrand) local.installed_grease_brand = remote.greaseBrand;
      if (remote.greaseVersion) local.installed_grease_version = remote.greaseVersion;
      if (remote.androidChromiumVersion) local.installed_android_chromium_version = remote.androidChromiumVersion;
      if (remote.tls) local.installed_tls = remote.tls;
      delete local.widevine_etag;
      this.saveManifest(local);
    }
    if (this.spec.widevine && (swapped || !local.widevine_etag)) {
      // The CDM is no reason to fail a run whose engine is in: tried again next time.
      try {
        local.widevine_etag = await this.downloadAndExtract(this.spec.widevine, this.root);
        this.placeWidevine();
      } catch (e) {
        console.warn(`[shardx] Widevine CDM not installed (${(e as Error).message}); tried again on the next run.`);
      }
    }
    const remoteFp = remote.archives[FINGERPRINTS_ARCHIVE.key];
    const fpDirHasJson = readdirSync(this.fingerprintsDir).some((f) => f.endsWith(".json"));
    if (force || !fpDirHasJson || (remoteFp !== undefined && local.fingerprints_etag !== remoteFp)) {
      await this.installFingerprints();
      if (remoteFp !== undefined) local.fingerprints_etag = remoteFp;
    }
    // Authoritative: the tree was replaced whole with exactly this version.
    // Recording the known value beats re-reading it off disk. What the
    // manifest says describes the engine on disk only when that is current.
    local.installed_chromium_version = this._chromiumVersion;
    if (!needBrowser || swapped) {
      if (remote.greaseBrand) local.installed_grease_brand = remote.greaseBrand;
      if (remote.greaseVersion) local.installed_grease_version = remote.greaseVersion;
      if (remote.androidChromiumVersion) local.installed_android_chromium_version = remote.androidChromiumVersion;
      if (remote.tls) local.installed_tls = remote.tls;
    }
    this.saveManifest(local);

    // Linux/mac archives produced on Windows lose every Unix exec bit;
    // restore +x on every ELF/Mach-O file under the engine tree (not
    // just the main binary — chrome spawns chrome_crashpad_handler,
    // chrome_sandbox, etc., and they need the exec bit too).
    if (osPlatform() !== "win32") {
      fixUnixExecBits(this.root);
    }
    this._checkedInProcess = true;
  }

  // ---- helpers ----

  /** Fetch the version manifest (GitHub raw) — one request that yields every
   *  archive's current etag + the chromium version, replacing per-archive HEADs
   *  against R2/S3. Empty archives / undefined version when unreachable. */
  private async fetchManifest(): Promise<{ archives: Record<string, string>; chromiumVersion?: string; engineBuild?: string; minSdkVersion?: string; greaseBrand?: string; greaseVersion?: string; androidChromiumVersion?: string; tls?: Record<string, unknown> }> {
    try {
      const r = await fetch(MANIFEST_URL);
      if (!r.ok) return { archives: {} };
      const data = await r.json() as { archives?: Record<string, string>; chromium_version?: string; engine_build?: string | number; min_sdk_version?: string; grease_brand?: string; grease_version?: string; android_chromium_version?: string; tls?: Record<string, unknown> };
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
        androidChromiumVersion: str(data?.android_chromium_version),
        tls: (data?.tls && typeof data.tls === "object" && !Array.isArray(data.tls))
          ? data.tls as Record<string, unknown>
          : undefined,
      };
    } catch { return { archives: {} }; }
  }

  /** Downloads `arch` beside `dest` and returns where it landed, with its etag.
   *  `unpacks`: refused before a byte is written when the disk cannot also
   *  hold it unpacked beside what is installed. */
  private async download(arch: Archive, dest: string, unpacks = false): Promise<{ tmp: string; etag: string }> {
    const url = `${PUB_BASE}/${arch.key}`;
    mkdirSync(dest, { recursive: true });
    const tmp = join(dest, `.${arch.key}.tmp`);

    const r = await fetch(url);
    if (!r.ok || !r.body) throw new Error(`download ${arch.key}: HTTP ${r.status}`);
    const etag = r.headers.get("etag")?.replace(/^"|"$/g, "") ?? "";
    const total = Number(r.headers.get("content-length") ?? 0);
    if (unpacks && total > 0) {
      try { assertRoom(dest, total * (1 + UNPACK_GUESS) + SPACE_MARGIN); }
      catch (e) { await r.body.cancel().catch(() => {}); throw e; }
    }

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
    // A connection cut mid-body still leaves us with a 200 and a short file;
    // the length the server promised is the only thing that says so.
    if (total > 0 && received < total) {
      rmQuiet(tmp);
      throw new Error(`download ${arch.key}: got ${received} of ${total} bytes`);
    }
    return { tmp, etag };
  }

  /** Unpacks an archive `download` left behind, and removes it. */
  private extractArchive(tmp: string, dest: string): void {
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
  }

  /** Brings the engine to the archive the manifest offers (`wanted`: its etag,
   *  when known). A checked tree an earlier run staged is used as it is; else
   *  the archive is downloaded and staged. Returns the etag that went in. */
  private async installEngine(wanted: string | undefined, force: boolean, lock?: InstallLock): Promise<string> {
    const top = this.spec.binarySubpath[0];
    const { staging } = enginePaths(this.root, top);
    const kept = readStaged(staging, top, this.spec.binarySubpath);
    let etag: string;
    if (kept && !force && (wanted === undefined || kept.etag === wanted)) {
      etag = kept.etag;
    } else {
      // An outdated tree goes before the download: its room counts.
      rmSync(staging, { recursive: true, force: true });
      const dl = await this.download(this.spec.browser, this.root, true);
      this.stageEngine(dl.tmp, dl.etag);
      etag = dl.etag;
    }
    this.swapStaged(lock);
    return etag;
  }

  /** Unpacks the engine archive at `tmp` into the staging folder beside the
   *  live engine and checks every file the archive lists. Nothing installed is
   *  touched: the tree waits there, noted, until `swapStaged` moves it in. The
   *  archive is removed either way. */
  stageEngine(tmp: string, etag: string): void {
    const top = this.spec.binarySubpath[0];
    const { staging } = enginePaths(this.root, top);
    const key = this.spec.browser.key;
    const entry = this.spec.binarySubpath.join("/");
    // Zips produced on Windows store backslashes; compare on one spelling.
    const norm = (p: string) => p.replace(/\\/g, "/").replace(/^\.\//, "");
    try {
      // The table of contents sits at the end of the file: exactly what a
      // download cut short is missing.
      let listing: ZipListing;
      try { listing = zipListing(tmp); } catch (e) {
        throw new Error(`download ${key}: not a readable zip (${(e as Error).message}) — not touching the installed engine`);
      }
      const names = listing.names.map(norm);
      if (!names.includes(entry)) {
        throw new Error(`download ${key}: no ${entry} inside — not touching the installed engine`);
      }
      // A dirty staging dir would mix an old engine into the new one; one we
      // cannot clear stops the install while the engine still works.
      rmSync(staging, { recursive: true, force: true });
      assertRoom(this.root, listing.unpacked + SPACE_MARGIN);
      try {
        this.extractArchive(tmp, staging);
        const gone = names.find((n) => n.startsWith(`${top}/`) && !n.endsWith("/") && !lexists(join(staging, n)));
        if (gone) throw new Error(`download ${key}: unpacked without ${gone} — an antivirus may have removed it`);
        if (osPlatform() !== "win32") fixUnixExecBits(join(staging, top));
        writeFileSync(join(staging, STAGED_NOTE), JSON.stringify({ etag, files: countFiles(join(staging, top)) }));
      } catch (e) {
        rmQuiet(staging);
        throw e;
      }
      // Whatever else the archive carries (`__MACOSX` and such) replaces its
      // namesake in the cache root, as extracting over the root used to do.
      for (const name of readdirSync(staging)) {
        if (name === top || name === STAGED_NOTE) continue;
        rmSync(join(this.root, name), { recursive: true, force: true });
        renameSync(join(staging, name), join(this.root, name));
      }
    } finally {
      rmQuiet(tmp);
    }
  }

  /** Swaps the staged tree in. Refused while processes run from the engine;
   *  every refusal keeps the staged tree for the next run. */
  swapStaged(lock?: InstallLock): void {
    const top = this.spec.binarySubpath[0];
    const { staging, target } = enginePaths(this.root, top);
    if (!readStaged(staging, top, this.spec.binarySubpath)) {
      rmQuiet(staging);
      throw new Error("the unpacked engine update is no longer complete (an antivirus may have removed a file); it is downloaded again on the next run");
    }
    if (lock && !lock.held()) throw new Error("another process took the engine install over");
    const busy = engineHooks.processesUnder(target);
    if (busy > 0) {
      throw new Error(
        `${busy} process${busy === 1 ? " is" : "es are"} still running from ${target}; the update is kept `
        + "and goes in once the browsers started from it are closed",
      );
    }
    swapEngineTree(this.root, top, join(staging, top));
    rmQuiet(staging);
  }

  /** Downloads and unpacks in one step, for everything that has nothing to lose. */
  private async downloadAndExtract(arch: Archive, dest: string): Promise<string> {
    const { tmp, etag } = await this.download(arch, dest);
    this.extractArchive(tmp, dest);
    return etag;
  }

  private placeWidevine(): void {
    if (!this.spec.widevine) return;
    const wrapper = this.spec.widevine.key.replace(/\.zip$/, "");
    const src = join(this.root, wrapper, "WidevineCdm");
    if (!existsSync(src)) return;
    // On macOS the CDM sits inside the framework's versioned folder. That is
    // the version of the engine on disk, not the one this SDK was published
    // with: the constant put it in a folder the engine never reads.
    const onDisk = this.installedEngineVersion() ?? CHROMIUM_VERSION;
    const dst = join(this.root,
      ...this.spec.widevineSubpath.map((seg) => (seg === CHROMIUM_VERSION ? onDisk : seg)));
    if (existsSync(dst)) rmSync(dst, { recursive: true, force: true });
    mkdirSync(dirname(dst), { recursive: true });
    renameSync(src, dst);
    rmSync(join(this.root, wrapper), { recursive: true, force: true });
  }

  private async installFingerprints(): Promise<void> {
    const url = `${PUB_BASE}/${FINGERPRINTS_ARCHIVE.key}`;
    const staging = join(this.fingerprintsDir, ".staging");
    if (existsSync(staging)) rmSync(staging, { recursive: true, force: true });
    mkdirSync(staging, { recursive: true });
    const tmp = join(staging, "bundle.zip");

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

    // Fingerprints bundle is plain JSON files — adm-zip is fine here
    // (no symlinks / exec bits to preserve).
    new AdmZip(tmp).extractAllTo(staging, true);

    const srcDir = join(staging, FINGERPRINTS_TOP_DIR);
    const walk = existsSync(srcDir) ? srcDir : staging;
    for (const name of readdirSync(walk)) {
      if (!name.endsWith(".json")) continue;
      const dst = join(this.fingerprintsDir, name);
      // Always overwrite bundled templates so engine-version bumps reach
      // existing libraries; user-added files (other names) are never iterated.
      copyFileSync(join(walk, name), dst);
    }
    rmSync(staging, { recursive: true, force: true });
  }
}

// ---- engine swap (#122) ----
// Shared with the Python and Rust SDKs, which use the same cache dir: in
// `<cache>`, `.install.lock`, `.<top>.staging/` (+ `staged.json`),
// `.<top>.previous` and the journal `.<top>.swapping` mean the same in all three.

/** Room kept free beyond what the unpacked engine takes. */
const SPACE_MARGIN = 256 * 1024 * 1024;
/** Unpacked bytes per archive byte, guessed before the archive is in. */
const UNPACK_GUESS = 3;
/** A lock this old is taken whoever holds it (a pid the OS gave to another). */
const LOCK_MAX_AGE_MS = 6 * 60 * 60 * 1000;
const STAGED_NOTE = "staged.json";

function enginePaths(root: string, top: string) {
  return {
    target: join(root, top),
    previous: join(root, `.${top}.previous`),
    journal: join(root, `.${top}.swapping`),
    staging: join(root, `.${top}.staging`),
  };
}

/** What the install asks the OS; tests swap in their own. */
export const engineHooks = { freeBytes, processesUnder };

/** The file operations an engine swap makes; tests hand in failing ones.
 *  `tries`: how often a rename Windows refuses for a moment is tried. */
export interface SwapFs {
  rename(from: string, to: string): void;
  rm(path: string): void;
  tries?: number;
}

const realFs: SwapFs = {
  rename: (from, to) => renameSync(from, to),
  rm: (path) => rmSync(path, { recursive: true, force: true }),
  tries: 6,
};

/** `f` again while a file is held for now (an antivirus reading a new one);
 *  any other error at once. */
function retrySync(f: () => void, tries: number): void {
  let pause = 100;
  for (let i = 1; ; i++) {
    try { f(); return; } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      const held = code === "EBUSY" || (osPlatform() === "win32" && (code === "EPERM" || code === "EACCES"));
      if (i >= tries || !held) throw e;
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, pause);
    pause = Math.min(pause * 2, 2000);
  }
}

/** Files and links under `dir`, links not followed. */
export function countFiles(dir: string): number {
  let entries;
  try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return 0; }
  let n = 0;
  for (const e of entries) n += e.isDirectory() ? countFiles(join(dir, e.name)) : 1;
  return n;
}

/** On disk before the rename it announces, whole or not at all. */
function writeJournal(path: string, files: number): void {
  const tmp = `${path}.tmp`;
  const fd = openSync(tmp, "w");
  try { writeSync(fd, JSON.stringify({ files })); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(tmp, path);
}

function readJournal(path: string): number | undefined {
  try {
    const v = JSON.parse(readFileSync(path, "utf8"));
    return typeof v?.files === "number" ? v.files : undefined;
  } catch { return undefined; }
}

/** Puts `<root>/.<top>.previous` back when a run died between the two renames
 *  of an update (or could not undo the first) — only as its journal left it,
 *  and only whole. Any other copy is a leftover and goes: one a cleanup left
 *  half removed never stands in for a missing engine. True when put back. */
export function recoverEngine(root: string, top: string, fs: SwapFs = realFs): boolean {
  const { target, previous, journal } = enginePaths(root, top);
  if (existsSync(previous)) {
    const moved = readJournal(journal);
    if (!existsSync(target) && moved !== undefined && countFiles(previous) === moved) {
      // Still held: kept, with its journal, for the next try.
      try { retrySync(() => fs.rename(previous, target), fs.tries ?? 1); } catch { return false; }
      rmQuiet(journal);
      return true;
    }
    try { fs.rm(previous); } catch { /* the next install() tries again */ }
  }
  if (!existsSync(previous)) rmQuiet(journal);
  return false;
}

/** Swaps the complete tree at `staged` in for `<root>/<top>`: live → previous,
 *  staged → live, the first undone if the second fails. Any failure leaves the
 *  engine that was there (or `.previous` with its journal, put back by the
 *  next install()). */
export function swapEngineTree(root: string, top: string, staged: string, fs: SwapFs = realFs): void {
  const { target, previous, journal } = enginePaths(root, top);
  const again = (f: () => void) => retrySync(f, fs.tries ?? 1);
  recoverEngine(root, top, fs);
  if (existsSync(previous)) {
    throw new Error(existsSync(target)
      ? `a copy of an earlier engine at ${previous} could not be removed; delete it and try again`
      : `the previous engine at ${previous} could not be put back yet; close the browsers started from it and try again`);
  }
  const hadLive = existsSync(target);
  if (hadLive) {
    writeJournal(journal, countFiles(target));
    try { again(() => fs.rename(target, previous)); } catch (e) {
      rmQuiet(journal);
      throw new Error(
        `the engine at ${target} is in use and was not replaced (${(e as Error).message}); `
        + "close the browsers started from it and try again",
      );
    }
  }
  try {
    again(() => fs.rename(staged, target));
  } catch (e) {
    if (hadLive) {
      try { again(() => fs.rename(previous, target)); } catch {
        // The journal stays: it is what puts the engine back.
        throw new Error(
          `replacing the engine failed (${(e as Error).message}) and the previous one could not be `
          + `put back yet; it is kept at ${previous} and restored by the next install()`,
        );
      }
      rmQuiet(journal);
    }
    throw e;
  }
  rmQuiet(journal);
  // A tree Windows still holds is clutter: the next install() removes it.
  try { fs.rm(previous); } catch { /* clutter */ }
}

/** A tree an earlier run unpacked, checked and noted, still whole. */
function readStaged(staging: string, top: string, binarySubpath: string[]): { etag: string; files: number } | undefined {
  try {
    const v = JSON.parse(readFileSync(join(staging, STAGED_NOTE), "utf8"));
    if (typeof v?.etag !== "string" || typeof v?.files !== "number") return undefined;
    const tree = join(staging, top);
    if (!existsSync(join(staging, ...binarySubpath)) || countFiles(tree) !== v.files) return undefined;
    return { etag: v.etag, files: v.files };
  } catch { return undefined; }
}

/** What an install that died left: an unfinished staging folder, a partial
 *  download. A checked staged tree stays for the next swap. */
function sweepLeftovers(root: string, top: string, key: string, binarySubpath: string[]): void {
  const { staging } = enginePaths(root, top);
  if (existsSync(staging) && !readStaged(staging, top, binarySubpath)) rmQuiet(staging);
  rmQuiet(join(root, `.${key}.tmp`));
}

/** Bytes the current user may still write on the disk holding `dir`. */
export function freeBytes(dir: string): number | undefined {
  // Node 18.15 and later.
  const statfs = (nodeFs as { statfsSync?: (p: string) => { bavail: number; bsize: number } }).statfsSync;
  if (!statfs) return undefined;
  try { const s = statfs(dir); return s.bavail * s.bsize; } catch { return undefined; }
}

function assertRoom(dir: string, need: number): void {
  const free = engineHooks.freeBytes(dir);
  if (free !== undefined && free < need) {
    const mb = (n: number) => Math.ceil(n / (1 << 20));
    throw new Error(
      `not enough free disk space for the engine update: it needs about ${mb(need)} MB and `
      + `${mb(free)} MB is free in ${dir}; the engine you have was not touched`,
    );
  }
}

/** How many processes run from `dir`. On macOS and Linux a swap would pull
 *  the files from under them; Windows refuses the rename itself. */
export function processesUnder(dir: string): number {
  // As given and as resolved: a process names the path it was started by.
  let roots: string[];
  try { roots = [...new Set([resolve(dir), realpathSync(dir)])].map((r) => r.replace(/[\\/]+$/, "") + sep); } catch { return 0; }
  const under = (p: string) => roots.some((r) => p.startsWith(r));
  if (osPlatform() === "linux") {
    let pids: string[];
    try { pids = readdirSync("/proc"); } catch { return 0; }
    let n = 0;
    for (const pid of pids) {
      if (!/^\d+$/.test(pid)) continue;
      // A replaced binary reads "<path> (deleted)": still that folder.
      try { if (under(readlinkSync(`/proc/${pid}/exe`).replace(/ \(deleted\)$/, ""))) n++; } catch { /* gone, or not ours */ }
    }
    return n;
  }
  if (osPlatform() === "darwin") {
    const r = spawnSync("ps", ["-axwwo", "comm="], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    return (r.stdout ?? "").split("\n").filter((l) => under(l.trim())).length;
  }
  return 0;
}

interface LockInfo { pid?: number; host?: string; token?: string; at?: number }

/** One install at a time over a cache dir, across processes and the three
 *  SDKs (same file, same rules). A holder that died is taken over. */
export class InstallLock {
  private constructor(readonly path: string, readonly token: string) {}

  static async acquire(root: string, opts: { pollMs?: number } = {}): Promise<InstallLock> {
    const path = join(root, ".install.lock");
    const token = randomBytes(8).toString("hex");
    let told = false;
    for (;;) {
      if (createLock(path, token)) return new InstallLock(path, token);
      const seen = readLock(path);
      if (seen === null) continue; // released meanwhile
      if (lockIsStale(path, seen)) { stealLock(path, seen, token); continue; }
      if (!told && seen.pid !== process.pid) {
        console.warn(
          `[shardx] waiting for another process (pid ${seen.pid ?? "?"}) to finish installing the engine… `
          + `(if none is running, delete ${path})`,
        );
        told = true;
      }
      await new Promise((r) => setTimeout(r, (opts.pollMs ?? 500) + Math.random() * 100));
    }
  }

  /** Still ours: nobody took it over as stale. */
  held(): boolean { return readLock(this.path)?.token === this.token; }

  release(): void {
    if (this.held()) { try { unlinkSync(this.path); } catch { /* gone */ } }
  }
}

function createLock(path: string, token: string): boolean {
  let fd: number;
  try { fd = openSync(path, "wx"); } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "EEXIST") return false;
    throw e;
  }
  try {
    writeSync(fd, JSON.stringify({ pid: process.pid, host: hostname().toLowerCase(), token, at: Date.now() }));
    fsyncSync(fd);
  } finally { closeSync(fd); }
  return true;
}

/** null when there is no lock; {} when it cannot be read as one. */
function readLock(path: string): LockInfo | null {
  let text: string;
  try { text = readFileSync(path, "utf8"); } catch { return null; }
  try { const v = JSON.parse(text); return v && typeof v === "object" ? v : {}; } catch { return {}; }
}

function lockIsStale(path: string, l: LockInfo): boolean {
  let mtime: number;
  try { mtime = statSync(path).mtimeMs; } catch { return false; }
  // Unreadable: being written this instant, or by a process that died doing so.
  if (typeof l.token !== "string") return Date.now() - mtime > 10_000;
  if (Date.now() - (typeof l.at === "number" ? l.at : mtime) > LOCK_MAX_AGE_MS) return true;
  // Another computer's process cannot be asked about.
  if (l.host !== hostname().toLowerCase()) return false;
  return !pidAlive(l.pid);
}

export function pidAlive(pid: unknown): boolean {
  if (typeof pid !== "number" || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** Moves the stale lock aside and drops it, unless what moved was a fresh one
 *  another process took in the meantime: that one is put back. */
function stealLock(path: string, seen: LockInfo, token: string): void {
  const aside = `${path}.stale-${token}`;
  try { renameSync(path, aside); } catch { return; }
  const got = readLock(aside);
  if (got && got.token !== seen.token) {
    try { linkSync(aside, path); } catch { /* a third one has it */ }
  }
  try { unlinkSync(aside); } catch { /* gone */ }
}

interface ZipListing { names: string[]; unpacked: number }

/** The archive's own table of contents, read from its end without loading the
 *  rest: entry names and the bytes they unpack to. Throws when there is none. */
export function zipListing(path: string): ZipListing {
  const fd = openSync(path, "r");
  try {
    const size = fstatSync(fd).size;
    const tailLen = Math.min(size, 22 + 0xffff);
    const tail = Buffer.alloc(tailLen);
    readSync(fd, tail, 0, tailLen, size - tailLen);
    let end = -1;
    for (let i = tailLen - 22; i >= 0; i--) {
      if (tail.readUInt32LE(i) === 0x06054b50) { end = i; break; }
    }
    if (end < 0) throw new Error("no end of central directory");
    let count = tail.readUInt16LE(end + 10);
    let cdSize = tail.readUInt32LE(end + 12);
    let cdOff = tail.readUInt32LE(end + 16);
    if (count === 0xffff || cdSize === 0xffffffff || cdOff === 0xffffffff) {
      // ZIP64: its locator sits just before the end record.
      const loc = end - 20;
      if (loc < 0 || tail.readUInt32LE(loc) !== 0x07064b50) throw new Error("no zip64 locator");
      const rec = Buffer.alloc(56);
      readSync(fd, rec, 0, 56, Number(tail.readBigUInt64LE(loc + 8)));
      if (rec.readUInt32LE(0) !== 0x06064b50) throw new Error("no zip64 end record");
      count = Number(rec.readBigUInt64LE(32));
      cdSize = Number(rec.readBigUInt64LE(40));
      cdOff = Number(rec.readBigUInt64LE(48));
    }
    if (cdOff + cdSize > size) throw new Error("central directory past the end of the file");
    const cd = Buffer.alloc(cdSize);
    readSync(fd, cd, 0, cdSize, cdOff);
    const names: string[] = [];
    let unpacked = 0;
    let p = 0;
    for (let n = 0; n < count; n++) {
      if (p + 46 > cd.length || cd.readUInt32LE(p) !== 0x02014b50) throw new Error("damaged central directory");
      let usize = cd.readUInt32LE(p + 24);
      const nameLen = cd.readUInt16LE(p + 28);
      const extraLen = cd.readUInt16LE(p + 30);
      const commentLen = cd.readUInt16LE(p + 32);
      names.push(cd.toString("utf8", p + 46, p + 46 + nameLen));
      if (usize === 0xffffffff) {
        // The zip64 extra field leads with the real size.
        for (let e = p + 46 + nameLen; e + 4 <= p + 46 + nameLen + extraLen;) {
          const id = cd.readUInt16LE(e);
          const len = cd.readUInt16LE(e + 2);
          if (id === 0x0001 && len >= 8) { usize = Number(cd.readBigUInt64LE(e + 4)); break; }
          e += 4 + len;
        }
      }
      unpacked += usize;
      p += 46 + nameLen + extraLen + commentLen;
    }
    return { names, unpacked };
  } finally {
    closeSync(fd);
  }
}

function lexists(p: string): boolean {
  try { lstatSync(p); return true; } catch { return false; }
}

/** Delete that is allowed to fail: a staging or backup dir we cannot remove
 *  (Windows holds files of a running engine open) is clutter, not a reason to
 *  report an install that already succeeded as failed. */
function rmQuiet(p: string): void {
  try { rmSync(p, { recursive: true, force: true }); } catch { /* clutter */ }
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
