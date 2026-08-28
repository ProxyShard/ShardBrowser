// Runtime cache: download ShardX engine + Widevine CDM + fingerprint
// library from the ProxyShard CDN, extract into a per-user cache dir,
// place Widevine inside the engine bundle, remember etags so subsequent
// runs are zero-network. Mirrors src-tauri/src/runtime.rs in the launcher.
import { closeSync, createWriteStream, existsSync, mkdirSync, openSync, readdirSync, readFileSync, readSync, renameSync, rmSync, statSync, writeFileSync, chmodSync, copyFileSync, lstatSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { homedir, platform as osPlatform, arch as osArch } from "node:os";
import { join, dirname, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { spawnSync } from "node:child_process";
import AdmZip from "adm-zip";

export const PUB_BASE = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
export const CHROMIUM_VERSION = "149.0.7827.103";
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

  // ---- manifest ----

  private loadManifest(): Manifest {
    try { return JSON.parse(readFileSync(this.manifestPath, "utf8")); }
    catch { return {}; }
  }
  private saveManifest(m: Manifest): void {
    writeFileSync(this.manifestPath, JSON.stringify(m, null, 2));
  }

  // ---- install ----

  async install(opts: { force?: boolean } = {}): Promise<void> {
    const force = !!opts.force;
    if (this._checkedInProcess && !force) return;
    const local = this.loadManifest();
    const remote = await this.fetchManifest();
    // Remember the engine version + grease so launch can normalise profiles.
    this._chromiumVersion = remote.chromiumVersion ?? CHROMIUM_VERSION;
    this._greaseBrand = remote.greaseBrand;
    this._greaseVersion = remote.greaseVersion;

    // Re-download the engine when its on-disk version differs from the
    // manifest's chromium version — VERSION-based, not etag, so it fires for
    // users who updated the SDK but whose stored etag already matched. Manifest
    // unreachable (undefined) → don't force a re-download when installed.
    let needBrowser = force || !this.installed;
    if (!needBrowser && remote.chromiumVersion !== undefined) {
      needBrowser = this.effectiveInstalledVersion(local) !== remote.chromiumVersion;
    }
    if (needBrowser) {
      // Wipe the old engine tree first so a leftover `<old>.manifest` / stale
      // libs can't linger beside the new ones (that pinned the detected version
      // → endless re-download). binarySubpath[0] is the engine root dir.
      rmSync(join(this.root, this.spec.binarySubpath[0]), { recursive: true, force: true });
      local.browser_etag = await this.downloadAndExtract(this.spec.browser, this.root);
    }
    if (this.spec.widevine && (needBrowser || !local.widevine_etag)) {
      local.widevine_etag = await this.downloadAndExtract(this.spec.widevine, this.root);
      this.placeWidevine();
    }
    const remoteFp = remote.archives[FINGERPRINTS_ARCHIVE.key];
    const fpDirHasJson = readdirSync(this.fingerprintsDir).some((f) => f.endsWith(".json"));
    if (force || !fpDirHasJson || (remoteFp !== undefined && local.fingerprints_etag !== remoteFp)) {
      await this.installFingerprints();
      if (remoteFp !== undefined) local.fingerprints_etag = remoteFp;
    }
    // Authoritative: we just extracted exactly this version (old tree wiped
    // first). Recording the known value beats re-reading it off disk.
    local.installed_chromium_version = this._chromiumVersion;
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
  private async fetchManifest(): Promise<{ archives: Record<string, string>; chromiumVersion?: string; greaseBrand?: string; greaseVersion?: string }> {
    try {
      const r = await fetch(MANIFEST_URL);
      if (!r.ok) return { archives: {} };
      const data = await r.json() as { archives?: Record<string, string>; chromium_version?: string; grease_brand?: string; grease_version?: string };
      const str = (v: unknown) => (typeof v === "string" ? v : undefined);
      return {
        archives: (data && typeof data.archives === "object" && data.archives) || {},
        chromiumVersion: str(data?.chromium_version),
        greaseBrand: str(data?.grease_brand),
        greaseVersion: str(data?.grease_version),
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

  private placeWidevine(): void {
    if (!this.spec.widevine) return;
    const wrapper = this.spec.widevine.key.replace(/\.zip$/, "");
    const src = join(this.root, wrapper, "WidevineCdm");
    if (!existsSync(src)) return;
    const dst = join(this.root, ...this.spec.widevineSubpath);
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
