"""Runtime cache: download ShardX engine + Widevine CDM + fingerprint library
from the ProxyShard CDN, extract into a per-user cache dir, place Widevine
inside the engine bundle, and remember etags so subsequent runs are
zero-network. Mirrors src-tauri/src/runtime.rs in the launcher."""
from __future__ import annotations

import errno
import json
import os
import platform
import random
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tarfile
import time
import warnings
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import httpx

PUB_BASE = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev"
CHROMIUM_VERSION = "157.0.8103.39"
# This SDK's own version, compared against the manifest's `min_sdk_version`.
SDK_VERSION = "2.1.4"
# Version manifest (GitHub raw) — one tiny GET tells us every archive's current
# etag, so we never poll R2/S3 (no per-archive HEAD). Updated archives are then
# pulled from PUB_BASE only when their etag changed.
MANIFEST_URL = "https://raw.githubusercontent.com/ProxyShard/ShardBrowser/main/runtime.json"

# Default cache: ~/Library/Application Support/shardx-sdk (mac),
# %LOCALAPPDATA%\shardx-sdk (win), ~/.cache/shardx-sdk (linux).
def _default_cache_dir() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "shardx-sdk"
    if sys.platform == "win32":
        return Path(os.environ.get("LOCALAPPDATA", Path.home())) / "shardx-sdk"
    return Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "shardx-sdk"

RUNTIME_DIR = _default_cache_dir()


@dataclass(frozen=True)
class Archive:
    key: str           # filename in R2 bucket
    label: str         # human-readable for progress callbacks


@dataclass(frozen=True)
class HostSpec:
    browser: Archive
    widevine: Optional[Archive]
    binary_subpath: tuple[str, ...]   # path under runtime/ to the executable
    widevine_subpath: tuple[str, ...] # destination for the WidevineCdm dir


def host_spec() -> HostSpec:
    sysname = sys.platform
    arch = platform.machine().lower()
    if sysname == "darwin" and arch in ("arm64", "aarch64"):
        return HostSpec(
            browser=Archive("ShardX-Mac-arm64.zip", "ShardX browser (macOS arm64)"),
            widevine=Archive("ShardX-Widevine-Mac-arm64.zip", "Widevine CDM"),
            binary_subpath=("ShardX-Mac-arm64", "ShardX.app", "Contents", "MacOS", "ShardX"),
            widevine_subpath=("ShardX-Mac-arm64", "ShardX.app", "Contents", "Frameworks",
                              "ShardX Framework.framework", "Versions", CHROMIUM_VERSION,
                              "Libraries", "WidevineCdm"),
        )
    if sysname == "win32" and arch in ("amd64", "x86_64"):
        return HostSpec(
            browser=Archive("ShardX-Windows.zip", "ShardX browser (Windows x64)"),
            widevine=Archive("ShardX-Widevine-Win.zip", "Widevine CDM"),
            binary_subpath=("ShardX-Windows", "chrome.exe"),
            widevine_subpath=("ShardX-Windows", "WidevineCdm"),
        )
    if sysname.startswith("linux") and arch in ("x86_64", "amd64"):
        return HostSpec(
            browser=Archive("ShardX-Linux.zip", "ShardX browser (Linux x64)"),
            widevine=Archive("ShardX-Widevine-Linux.zip", "Widevine CDM"),
            binary_subpath=("ShardX-Linux", "chrome"),
            widevine_subpath=("ShardX-Linux", "WidevineCdm"),
        )
    raise RuntimeError(
        f"Unsupported host: {sysname}/{arch}. ShardX ships mac-arm64, win-x64, linux-x64."
    )


FINGERPRINTS_ARCHIVE = Archive("ShardX-Fingerprints.zip", "Fingerprint library")
FINGERPRINTS_TOP_DIR = "shardx-fingerprints"


ProgressCb = Callable[[str, int, int], None]   # (label, received, total)


def apply_engine_version(
    config: dict,
    chromium_version: str,
    grease_brand: Optional[str] = None,
    grease_version: Optional[str] = None,
    tls: Optional[dict] = None,
) -> None:
    """Normalise a profile config's spoofed Chrome version to `chromium_version`
    (e.g. "152.0.7977.65") so it always matches the running engine — bumps
    `navigator.user_agent` (Chrome/<major>.0.0.0) and the version fields in
    `client_hints`: brand_version / brand_full_version / chrome_build /
    chrome_patch (derived from the version) plus, when supplied, grease_brand /
    grease_version / grease_full_version (GREASE rotates per release, so it
    can't be derived — it comes from the manifest). Leaves platform_version,
    architecture, etc. intact. Mutates `config` in place. SDK equivalent of the
    launcher's post-update profile migration.

    `tls` carries the manifest's TLS block. Only the keys it actually contains
    are overwritten, so a profile's own `shuffle_extensions` (or any field the
    manifest does not ship) survives, and a profile with no `tls` block is left
    alone — absent means "use the engine default", not "stale". Without this the
    profile keeps the previous release's signature_algorithms while the engine
    advertises the new ones, and the JA4 hash no longer matches the Chrome
    version the profile claims: 152 added the ML-DSA schemes 0x0904-0x0906, so
    a 149-era profile is eleven entries short and reads as a mismatch."""
    parts = chromium_version.split(".")
    if len(parts) != 4:
        return
    major = parts[0]
    try:
        build = int(parts[2])
        patch = int(parts[3])
    except ValueError:
        build = patch = None

    nav = config.get("navigator")
    if isinstance(nav, dict) and isinstance(nav.get("user_agent"), str):
        ua = nav["user_agent"]
        idx = ua.find("Chrome/")
        if idx >= 0:
            rest = ua[idx + 7:]
            end = rest.find(" ")
            tail = rest[end:] if end >= 0 else ""
            nav["user_agent"] = f"{ua[:idx]}Chrome/{major}.0.0.0{tail}"

    ch = config.get("client_hints")
    if isinstance(ch, dict):
        ch["brand_version"] = major
        ch["brand_full_version"] = chromium_version
        if build is not None:
            ch["chrome_build"] = build
        if patch is not None:
            ch["chrome_patch"] = patch
        if grease_brand:
            ch["grease_brand"] = grease_brand
        if grease_version:
            ch["grease_version"] = grease_version
            ch["grease_full_version"] = f"{grease_version}.0.0.0"

    cfg_tls = config.get("tls")
    if isinstance(tls, dict) and isinstance(cfg_tls, dict):
        cfg_tls.update(tls)


# The brand Chrome lists in `userAgentData.brands` and `Sec-CH-UA`, Android included.
_CHROME_BRAND = "Google Chrome"


def apply_chrome_brand(config: dict) -> None:
    """Name the brand on a profile that names none, so the brand list is
    Chrome's and not only "Chromium" and GREASE; a brand the profile names
    stays. Launch calls it for phone profiles, whose library templates lacked
    the field. Mutates `config` in place. SDK equivalent of the launcher's
    profile migration."""
    ch = config.get("client_hints")
    if not isinstance(ch, dict):
        return
    if isinstance(ch.get("brand"), str) and ch["brand"]:
        return
    ch["brand"] = _CHROME_BRAND


def adopt_library_fields(config: dict, fingerprints_dir: Path) -> None:
    """Copy into a saved profile what its library template gained after it was
    frozen: fonts, fonts_local and webgpu.features where the profile has none,
    and the adapter's subgroup sizes for the same claimed GPU (the template's win). The template is
    found by `_meta.gpu_preset_id`, else by `name`; a profile with no template
    is left as it is. Mutates `config` in place. SDK equivalent of the
    launcher's `adopt_value`."""
    meta = config.get("_meta") if isinstance(config.get("_meta"), dict) else {}
    key = next((k for k in (meta.get("gpu_preset_id"), config.get("name")) if isinstance(k, str) and k), None)
    if not key or not re.fullmatch(r"[A-Za-z0-9._-]+", key):
        return
    path = Path(fingerprints_dir) / f"{key}.json"
    try:
        src = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return
    for k in ("fonts", "fonts_local"):
        if k not in config and k in src:
            config[k] = src[k]
    w, sw = config.get("webgpu"), src.get("webgpu")
    if not isinstance(w, dict) or not isinstance(sw, dict):
        return
    if "features" not in w and "features" in sw:
        w["features"] = sw["features"]
    # The template's subgroup sizes win for the same claimed GPU, over a stored None or a
    # pre-157 capture (D3D12 max was 128 then); a None in the template changes nothing.
    same_gpu = all(k in w and w[k] == sw.get(k) for k in ("vendor", "architecture"))
    for k in ("subgroupMinSize", "subgroupMaxSize"):
        v = sw.get(k)
        if same_gpu and isinstance(v, int) and not isinstance(v, bool):
            w[k] = v


# WebGPU features Chrome 156 reports on Apple M-series that older templates lack.
_APPLE_WEBGPU_ADDED = ("texture-compression-unaligned",)
# Chrome 157 adds these from M2 on (Dawn: Apple8 with Mac2, or Apple9); M1 has none.
_APPLE_M2_WEBGPU_ADDED = ("atomic-vec2u-min-max",)


def apply_apple_webgpu_features(config: dict) -> None:
    """Add `_APPLE_WEBGPU_ADDED` to an Apple M-series profile's
    `webgpu.features`, in sorted place; touches nothing else. An absent or
    empty list means "the host's own" and stays as it is. Mutates `config` in
    place. SDK equivalent of the launcher's profile migration of the same name."""
    webgl = config.get("webgl")
    webgpu = config.get("webgpu")
    renderer = webgl.get("renderer") if isinstance(webgl, dict) else None
    if not isinstance(renderer, str) or not isinstance(webgpu, dict) or webgpu.get("vendor") != "apple":
        return
    gen = re.search(r"Apple M(\d+)", renderer)
    if not gen:
        return
    features = webgpu.get("features")
    if not isinstance(features, list) or not features:
        return
    m2 = _APPLE_M2_WEBGPU_ADDED if int(gen.group(1)) >= 2 else ()
    for feat in _APPLE_WEBGPU_ADDED + m2:
        if feat in features:
            continue
        i = next((k for k, v in enumerate(features) if isinstance(v, str) and v > feat), len(features))
        features.insert(i, feat)


class Runtime:
    """Owns the cache dir and the install/update lifecycle."""

    def __init__(
        self,
        cache_dir: Optional[str | Path] = None,
        progress: Optional[ProgressCb] = None,
        profiles_dir: Optional[str | Path] = None,
    ):
        self.root = Path(cache_dir) if cache_dir else RUNTIME_DIR
        self.root.mkdir(parents=True, exist_ok=True)
        # Per-profile user-data-dir tree.  Defaults to `./shardx-profiles/`
        # next to the running script so the user can find cookies / cache
        # easily; override with `profiles_dir=...`.  Engine assets stay
        # in `cache_dir`.
        self._profiles_root = Path(profiles_dir).resolve() if profiles_dir else None
        self._progress = progress
        self._spec = host_spec()
        # Engine chromium version (manifest-driven; set on install()). Used by
        # launch to normalise profile UA + client_hints to the running engine.
        self._chromium_version = CHROMIUM_VERSION
        # GREASE brand/version from the manifest (rotates per release; can't be
        # derived from the version number). Applied to profiles on launch.
        self._grease_brand: Optional[str] = None
        self._grease_version: Optional[str] = None
        # TLS block from the manifest (signature_algorithms / cipher_suites).
        # Applied to profiles on launch so the JA4 hash keeps matching the
        # Chrome version the profile claims.
        self._tls: Optional[dict] = None
        # Set to True after a successful in-process install() so subsequent
        # launches in the same process skip the R2 HEAD round-trip (~1 s
        # over a clean connection).  Cleared by `install(force=True)`.
        self._checked_in_process = False

    @property
    def profiles_root(self) -> Path:
        d = self._profiles_root if self._profiles_root else self.root / "profiles"
        d.mkdir(parents=True, exist_ok=True)
        return d

    # ---- paths ----

    @property
    def manifest_path(self) -> Path:
        return self.root / "manifest.json"

    @property
    def binary_path(self) -> Path:
        return self.root.joinpath(*self._spec.binary_subpath)

    @property
    def fingerprints_dir(self) -> Path:
        d = self.root / "fingerprints"
        d.mkdir(parents=True, exist_ok=True)
        return d

    @property
    def installed(self) -> bool:
        return self.binary_path.exists()

    @property
    def chromium_version(self) -> str:
        """Engine chromium version (manifest-driven; set on install())."""
        return self._chromium_version

    @property
    def grease_brand(self) -> Optional[str]:
        """GREASE brand from the manifest (e.g. "Not)A;Brand"); set on install()."""
        return self._grease_brand

    @property
    def grease_version(self) -> Optional[str]:
        """GREASE version from the manifest (e.g. "24"); set on install()."""
        return self._grease_version

    @property
    def tls(self) -> Optional[dict]:
        """TLS overrides from the manifest; set on install()."""
        return self._tls

    def _installed_engine_version(self) -> Optional[str]:
        """Chromium version of the engine actually on disk — read from the
        mac Framework `Versions/<ver>/` dir or the win `<ver>.manifest` file.
        Returns None on Linux (no on-disk version marker) or when unreadable."""
        try:
            if sys.platform == "darwin":
                versions = self.root / "ShardX-Mac-arm64" / "ShardX.app" / "Contents" / \
                    "Frameworks" / "ShardX Framework.framework" / "Versions"
                for entry in versions.iterdir():
                    if entry.name != "Current" and entry.name[:1].isdigit():
                        return entry.name
                return None
            if sys.platform == "win32":
                # Only accept a `<version>.manifest` whose stem parses as a
                # version, so a stray/leftover manifest can't pin a bogus version.
                for entry in (self.root / "ShardX-Windows").iterdir():
                    if entry.suffix == ".manifest" and entry.stem[:1].isdigit() and "." in entry.stem:
                        return entry.stem
                return None
            return None
        except OSError:
            return None

    def _installed_engine_build(self, local: dict) -> Optional[str]:
        """Build stamp on disk: `<engine>/shardx-build` when the archive ships
        one, else what was recorded at install time."""
        try:
            stamp = (self.root / self._spec.binary_subpath[0] / "shardx-build").read_text(encoding="utf-8").strip()
            if stamp:
                return stamp
        except OSError:
            pass
        return local.get("installed_engine_build") or None

    def _effective_installed_version(self, local: dict) -> Optional[str]:
        """Effective installed version. Trusts the version recorded at install
        time (authoritative — written only after a successful extract) over
        re-reading it off disk, which can carry stale files from a previous
        version. On-disk detection is the fallback for legacy installs."""
        return local.get("installed_chromium_version") or self._installed_engine_version()

    # ---- manifest ----

    def _load_manifest(self) -> dict:
        try:
            return json.loads(self.manifest_path.read_text(encoding="utf-8"))
        except Exception:
            return {}

    def _save_manifest(self, m: dict) -> None:
        # Whole or not at all: other processes read it (_adopt_installed).
        tmp = self.manifest_path.with_name(self.manifest_path.name + ".tmp")
        tmp.write_text(json.dumps(m, indent=2), encoding="utf-8")
        os.replace(tmp, self.manifest_path)

    # ---- install ----

    @staticmethod
    def _version_lt(a: str, b: str) -> bool:
        """Dotted-numeric compare; non-numeric parts count as 0."""
        def parts(v: str) -> list:
            out = []
            for chunk in v.replace("-", ".").replace("+", ".").split("."):
                out.append(int(chunk) if chunk.isdigit() else 0)
            return out
        pa, pb = parts(a), parts(b)
        for i in range(max(len(pa), len(pb))):
            x = pa[i] if i < len(pa) else 0
            y = pb[i] if i < len(pb) else 0
            if x != y:
                return x < y
        return False

    def _engine_outdated(self, local: dict, manifest: dict) -> bool:
        """Chromium version, `engine_build`, or the archive hash — any one of
        them means the engine moved. An unknown local value is not a mismatch."""
        remote_ver = manifest.get("chromium_version")
        if remote_ver and self._effective_installed_version(local) != remote_ver:
            return True

        def differs(have, want) -> bool:
            return bool(have) and bool(want) and have != want

        if differs(self._installed_engine_build(local), manifest.get("engine_build")):
            return True
        archives = manifest.get("archives") if isinstance(manifest.get("archives"), dict) else {}
        return differs(local.get("browser_etag"), archives.get(self._spec.browser.key))

    def install(self, force: bool = False) -> None:
        """Idempotent — re-checks remote etag, skips when nothing changed.
        Within a single process, subsequent calls are no-ops unless `force=True`.
        """
        if self._checked_in_process and not force:
            self._adopt_installed()
            return
        # One install at a time over this cache, across processes and the SDKs.
        lock = _InstallLock.acquire(self.root)
        try:
            # Another call of this process did it while this one waited.
            if self._checked_in_process and not force:
                self._adopt_installed()
                return
            self._install_locked(force, lock)
        finally:
            lock.release()

    def _adopt_installed(self) -> None:
        """Another process may have swapped the engine since this one checked:
        the version profiles claim follows the engine on disk, from its record."""
        local = self._load_manifest()
        v = local.get("installed_chromium_version")
        if not v or v == self._chromium_version:
            return
        self._chromium_version = v
        self._grease_brand = local.get("installed_grease_brand") or None
        self._grease_version = local.get("installed_grease_version") or None
        tls = local.get("installed_tls")
        self._tls = tls if isinstance(tls, dict) else None

    def _stay_on_installed(self, local: dict) -> None:
        self._chromium_version = self._effective_installed_version(local) or CHROMIUM_VERSION
        self._grease_brand = local.get("installed_grease_brand") or None
        self._grease_version = local.get("installed_grease_version") or None
        tls = local.get("installed_tls")
        self._tls = tls if isinstance(tls, dict) else None

    def _install_locked(self, force: bool, lock: "_InstallLock") -> None:
        top = self._spec.binary_subpath[0]
        # A run that died between the two renames of an update left the engine
        # as `.previous`: put back before anything asks whether it is installed.
        _recover_engine(self.root, top)
        _sweep_leftovers(self.root, top, self._spec.browser.key, self._spec.binary_subpath)
        local = self._load_manifest()
        manifest = self._fetch_manifest()
        remote = manifest.get("archives") if isinstance(manifest.get("archives"), dict) else {}
        # An engine this SDK is too old to drive is not installed, but the one
        # already on disk keeps running. Refusing outright stopped every run on
        # an engine that worked yesterday, for an update nobody had asked for.
        # With nothing installed, or a reinstall asked for, there is nothing to
        # keep.
        min_sdk = manifest.get("min_sdk_version")
        too_old = bool(min_sdk) and self._version_lt(SDK_VERSION, str(min_sdk))
        if too_old and (force or not self.installed):
            raise RuntimeError(
                f"this engine build needs shardx {min_sdk} or newer — "
                f"you are on {SDK_VERSION}; upgrade the SDK first"
            )
        # Remember the engine version so launch can normalise profiles to it.
        # Without the manifest, what is ALREADY installed — not the version
        # this SDK was published with. Falling back to the constant made an
        # offline run record 152 over an engine that is 156, and profiles then
        # declared a version the engine does not have. Skipping an update, the
        # manifest describes an engine that is not the one on disk, so it is
        # not read.
        offered = {} if too_old else manifest
        self._chromium_version = (
            offered.get("chromium_version")
            or self._effective_installed_version(local)
            or CHROMIUM_VERSION
        )
        self._grease_brand = offered.get("grease_brand") or local.get("installed_grease_brand") or None
        self._grease_version = offered.get("grease_version") or local.get("installed_grease_version") or None
        remote_tls = offered.get("tls")
        if not isinstance(remote_tls, dict):
            remote_tls = local.get("installed_tls")
        self._tls = remote_tls if isinstance(remote_tls, dict) else None
        if too_old:
            warnings.warn(
                f"[shardx] engine {manifest.get('chromium_version') or 'update'} needs shardx "
                f"{min_sdk} or newer (this is {SDK_VERSION}); staying on the installed engine "
                f"{self._chromium_version}. Upgrade the SDK to get it.",
                RuntimeWarning,
                stacklevel=3,
            )
            self._checked_in_process = True
            return
        # Unknown is never a mismatch, so adopt a stamp once or the first bump
        # never lands.
        if self.installed and manifest.get("engine_build") \
                and not self._installed_engine_build(local):
            local["installed_engine_build"] = manifest["engine_build"]
        # A None manifest (unreachable) must NOT force a re-download.
        need_browser = force or not self.installed or self._engine_outdated(local, manifest)
        swapped = False
        if need_browser:
            # Nothing installed is touched until a complete new tree is unpacked
            # beside it (see _install_engine). The old tree still goes whole — a
            # leftover `<old>.manifest` or stale lib pinned the detected version
            # and re-downloaded for ever — but a download, an archive or an
            # unpack that fails must never be what removes it (#122).
            try:
                local["browser_etag"] = self._install_engine(remote.get(self._spec.browser.key), force, lock)
                swapped = True
            except Exception as e:
                # With nothing installed, or a reinstall asked for, there is nothing to stay on.
                if force or not self.installed:
                    raise
                # The engine on disk works: stay on it, as for an update this SDK is too old for.
                self._stay_on_installed(local)
                warnings.warn(
                    f"[shardx] the engine update was not installed: {e}. Staying on the installed "
                    f"engine {self._chromium_version}; the update is tried again on the next run.",
                    RuntimeWarning,
                    stacklevel=3,
                )
        if swapped:
            if manifest.get("engine_build"):
                local["installed_engine_build"] = manifest["engine_build"]
            # Saved at once: the engine on disk is the new one, whatever fails next.
            # The new tree has no CDM yet, so the old one's etag must not say it has.
            local["installed_chromium_version"] = self._chromium_version
            self._record_descriptor(local, manifest)
            local.pop("widevine_etag", None)
            self._save_manifest(local)
        if self._spec.widevine and (swapped or not local.get("widevine_etag")):
            # The CDM is no reason to fail a run whose engine is in: tried again next time.
            try:
                local["widevine_etag"] = self._download_and_extract(self._spec.widevine, self.root)
                self._place_widevine()
            except Exception as e:
                warnings.warn(
                    f"[shardx] Widevine CDM not installed ({e}); tried again on the next run.",
                    RuntimeWarning,
                    stacklevel=3,
                )
        # Fingerprints — additive seed (etag changed → re-extract, never
        # overwrites user-renamed files).
        fp_remote = remote.get(FINGERPRINTS_ARCHIVE.key)
        need_fp = force or not any(self.fingerprints_dir.glob("*.json")) or \
            (fp_remote is not None and local.get("fingerprints_etag") != fp_remote)
        if need_fp:
            self._install_fingerprints()
            if fp_remote is not None:
                local["fingerprints_etag"] = fp_remote
        # Authoritative: the tree was replaced whole with exactly this version.
        # Recording the known value beats re-reading it off disk. What the
        # manifest says describes the engine on disk only when that is current.
        local["installed_chromium_version"] = self._chromium_version
        if not need_browser or swapped:
            self._record_descriptor(local, manifest)
        self._save_manifest(local)
        # Linux/mac archives produced on Windows lose every Unix exec bit;
        # restore +x on every ELF/Mach-O file under the engine tree (not
        # just the main binary — chrome spawns chrome_crashpad_handler,
        # chrome_sandbox, etc., and they need the exec bit too).
        if sys.platform != "win32":
            _fix_unix_exec_bits(self.root)
        self._checked_in_process = True

    @staticmethod
    def _record_descriptor(local: dict, manifest: dict) -> None:
        """GREASE and TLS of the engine on disk, for runs without the manifest."""
        for src, dst in (("grease_brand", "installed_grease_brand"),
                         ("grease_version", "installed_grease_version")):
            if manifest.get(src):
                local[dst] = manifest[src]
        if isinstance(manifest.get("tls"), dict):
            local["installed_tls"] = manifest["tls"]

    def _fetch_manifest(self) -> dict:
        """Fetch the version manifest (GitHub raw) — one request that yields
        every archive's current etag + the chromium version, replacing
        per-archive HEADs against R2/S3. Returns the parsed manifest, or {}
        when unreachable."""
        try:
            with httpx.Client(timeout=8.0, follow_redirects=True) as c:
                r = c.get(MANIFEST_URL)
                if r.status_code != 200:
                    return {}
                data = r.json()
                return data if isinstance(data, dict) else {}
        except Exception:
            return {}

    def _download_and_extract(self, arch: Archive, dest: Path) -> str:
        tmp, etag = self._download(arch, dest)
        self._extract_archive(tmp, dest)
        return etag

    def _download(self, arch: Archive, dest: Path, unpacks: bool = False) -> tuple[Path, str]:
        """Fetch `arch` beside `dest` without touching what is installed.
        `unpacks`: refused before a byte is written when the disk cannot also
        hold it unpacked beside what is installed."""
        url = f"{PUB_BASE}/{arch.key}"
        tmp = dest / f".{arch.key}.tmp"
        tmp.parent.mkdir(parents=True, exist_ok=True)
        etag = ""
        with httpx.stream("GET", url, timeout=None, follow_redirects=True) as r:
            r.raise_for_status()
            etag = r.headers.get("etag", "").strip('"')
            total = int(r.headers.get("content-length", 0))
            if unpacks and total > 0:
                _assert_room(dest, total * (1 + UNPACK_GUESS) + SPACE_MARGIN)
            received = 0
            try:
                with tmp.open("wb") as f:
                    for chunk in r.iter_bytes(chunk_size=1 << 16):
                        f.write(chunk)
                        received += len(chunk)
                        if self._progress:
                            self._progress(arch.label, received, total)
            except BaseException:
                tmp.unlink(missing_ok=True)
                raise
        # A connection cut mid-body can still end as a 200 and a short file.
        if total > 0 and received < total:
            tmp.unlink(missing_ok=True)
            raise RuntimeError(f"download {arch.key}: got {received} of {total} bytes")
        return tmp, etag

    def _extract_archive(self, tmp: Path, dest: Path) -> None:
        """Unpack what `_download` left behind, and remove it."""
        # Extract.  IMPORTANT: on macOS/Linux we shell out to the system
        # `unzip` instead of Python's `zipfile` because zipfile cannot
        # restore symlinks (every `Versions/Current/...` link in a `.app`
        # framework gets written as a 24-byte text file) and drops the
        # +x permission bits on every helper executable.  The result
        # extracts cleanly but fails to launch — GPU helper can't find
        # the framework dylib and the engine FATALs on first child.
        if sys.platform == "win32":
            with zipfile.ZipFile(tmp) as z:
                z.extractall(dest)
        else:
            _system_unzip(tmp, dest)
        tmp.unlink(missing_ok=True)

    def _install_engine(self, wanted: Optional[str], force: bool = False,
                        lock: Optional["_InstallLock"] = None) -> str:
        """Brings the engine to the archive the manifest offers (`wanted`: its
        etag, when known). A checked tree an earlier run staged is used as it
        is; else the archive is downloaded and staged. Returns the etag."""
        top = self._spec.binary_subpath[0]
        staging = self.root / f".{top}.staging"
        kept = _read_staged(staging, top, self._spec.binary_subpath)
        if kept and not force and (wanted is None or kept["etag"] == wanted):
            etag = kept["etag"]
        else:
            # An outdated tree goes before the download: its room counts.
            _clear(staging)
            tmp, etag = self._download(self._spec.browser, self.root, unpacks=True)
            self._stage_engine(tmp, etag)
        self._swap_staged(lock)
        return etag

    def _stage_engine(self, tmp: Path, etag: str) -> None:
        """Unpacks the engine archive at `tmp` into the staging folder beside
        the live engine and checks every file the archive lists. Nothing
        installed is touched: the tree waits there, noted, until `_swap_staged`
        moves it in. The archive is removed either way."""
        key = self._spec.browser.key
        top = self._spec.binary_subpath[0]
        staging = self.root / f".{top}.staging"
        entry = "/".join(self._spec.binary_subpath)
        try:
            # The table of contents sits at the end of the file: exactly what a
            # download cut short is missing.
            try:
                with zipfile.ZipFile(tmp) as z:
                    infos = z.infolist()
            except (zipfile.BadZipFile, OSError) as e:
                raise RuntimeError(f"download {key}: not a readable zip ({e}) — not touching the installed engine") from e
            names = [_zip_name(i.filename) for i in infos]
            if entry not in names:
                raise RuntimeError(f"download {key}: no {entry} inside — not touching the installed engine")
            # A dirty staging dir would mix an old engine into the new one.
            _clear(staging)
            _assert_room(self.root, sum(i.file_size for i in infos) + SPACE_MARGIN)
            try:
                self._extract_archive(tmp, staging)
                gone = next((n for n in names if n.startswith(f"{top}/") and not n.endswith("/")
                             and not os.path.lexists(staging / n)), None)
                if gone:
                    raise RuntimeError(f"download {key}: unpacked without {gone} — an antivirus may have removed it")
                if sys.platform != "win32":
                    _fix_unix_exec_bits(staging / top)
                (staging / STAGED_NOTE).write_text(
                    json.dumps({"etag": etag, "files": _count_files(staging / top)}), encoding="utf-8")
            except BaseException:
                shutil.rmtree(staging, ignore_errors=True)
                raise
            # Whatever else the archive carries (`__MACOSX` and such) replaces its
            # namesake in the cache root, as extracting over the root used to do.
            for p in list(staging.iterdir()):
                if p.name in (top, STAGED_NOTE):
                    continue
                dst = self.root / p.name
                if dst.is_dir() and not dst.is_symlink():
                    shutil.rmtree(dst, ignore_errors=True)
                elif os.path.lexists(dst):
                    dst.unlink()
                os.replace(p, dst)
        finally:
            tmp.unlink(missing_ok=True)

    def _swap_staged(self, lock: Optional["_InstallLock"] = None) -> None:
        """Swaps the staged tree in. Refused while processes run from the
        engine; every refusal keeps the staged tree for the next run."""
        top = self._spec.binary_subpath[0]
        staging = self.root / f".{top}.staging"
        target = self.root / top
        if _read_staged(staging, top, self._spec.binary_subpath) is None:
            shutil.rmtree(staging, ignore_errors=True)
            raise RuntimeError(
                "the unpacked engine update is no longer complete (an antivirus may have removed a "
                "file); it is downloaded again on the next run"
            )
        if lock is not None and not lock.held():
            raise RuntimeError("another process took the engine install over")
        busy = _processes_under(target)
        if busy > 0:
            raise RuntimeError(
                f"{busy} process{' is' if busy == 1 else 'es are'} still running from {target}; the "
                "update is kept and goes in once the browsers started from it are closed"
            )
        _swap_engine_tree(self.root, top, staging / top)
        shutil.rmtree(staging, ignore_errors=True)

    def _place_widevine(self) -> None:
        if not self._spec.widevine:
            return
        # Source dir inside the extracted Widevine archive (mirrors the
        # `ShardX-Widevine-<plat>/WidevineCdm` layout from the launcher).
        wrapper_name = self._spec.widevine.key.removesuffix(".zip")
        src = self.root / wrapper_name / "WidevineCdm"
        if not src.exists():
            return
        # On macOS the CDM sits inside the framework's versioned folder. That
        # is the version of the engine on disk, not the one this SDK was
        # published with: the constant put it in a folder the engine never
        # reads.
        on_disk = self._installed_engine_version() or CHROMIUM_VERSION
        dst = self.root.joinpath(*(
            on_disk if seg == CHROMIUM_VERSION else seg for seg in self._spec.widevine_subpath
        ))
        if dst.exists():
            shutil.rmtree(dst)
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(src), str(dst))
        shutil.rmtree(self.root / wrapper_name, ignore_errors=True)

    def _install_fingerprints(self) -> None:
        url = f"{PUB_BASE}/{FINGERPRINTS_ARCHIVE.key}"
        staging = self.fingerprints_dir / ".staging"
        if staging.exists():
            shutil.rmtree(staging)
        staging.mkdir(parents=True, exist_ok=True)
        tmp = staging / "bundle.zip"
        with httpx.stream("GET", url, timeout=None, follow_redirects=True) as r:
            r.raise_for_status()
            total = int(r.headers.get("content-length", 0))
            received = 0
            with tmp.open("wb") as f:
                for chunk in r.iter_bytes(chunk_size=1 << 16):
                    f.write(chunk)
                    received += len(chunk)
                    if self._progress:
                        self._progress(FINGERPRINTS_ARCHIVE.label, received, total)
        # Fingerprints bundle is plain JSON files — `zipfile` is fine
        # everywhere (no symlinks / exec bits to preserve).
        with zipfile.ZipFile(tmp) as z:
            z.extractall(staging)
        # Move *.json from the wrapper dir into fingerprints/, additive
        # Always overwrite bundled templates so engine-version bumps reach
        # existing libraries; user-added files (other names) are never iterated.
        src_dir = staging / FINGERPRINTS_TOP_DIR
        walk = src_dir if src_dir.exists() else staging
        for p in walk.iterdir():
            if p.suffix == ".json":
                shutil.copy(p, self.fingerprints_dir / p.name)
        shutil.rmtree(staging, ignore_errors=True)


# ---- engine swap (#122) ----
# Shared with the Node and Rust SDKs, which use the same cache dir: in
# `<cache>`, `.install.lock`, `.<top>.staging/` (+ `staged.json`),
# `.<top>.previous` and the journal `.<top>.swapping` mean the same in all three.

# Room kept free beyond what the unpacked engine takes.
SPACE_MARGIN = 256 << 20
# Unpacked bytes per archive byte, guessed before the archive is in.
UNPACK_GUESS = 3
# A lock this old is taken whoever holds it (a pid the OS gave to another).
LOCK_MAX_AGE_MS = 6 * 60 * 60 * 1000
STAGED_NOTE = "staged.json"
# How often a rename Windows refuses for a moment (an antivirus) is tried.
RENAME_TRIES = 6


def _rmtree(path: Path) -> None:
    shutil.rmtree(path)


def _unlink_quiet(path: Path) -> None:
    try:
        path.unlink()
    except OSError:
        pass


def _clear(d: Path) -> None:
    """`d` gone, or an error: a staging dir we cannot clear stops the install
    while the engine still works."""
    shutil.rmtree(d, ignore_errors=True)
    if os.path.lexists(d):
        raise RuntimeError(f"could not clear {d}; close what uses it and try again")


def _zip_name(n: str) -> str:
    # Zips produced on Windows store backslashes; compare on one spelling.
    n = n.replace("\\", "/")
    return n[2:] if n.startswith("./") else n


def _transient(e: OSError) -> bool:
    """A file held for now: busy, or Windows' access denied (5) / sharing violation (32)."""
    if e.errno == errno.EBUSY:
        return True
    return sys.platform == "win32" and (
        getattr(e, "winerror", None) in (5, 32) or e.errno in (errno.EACCES, errno.EPERM))


def _retry(f, tries: int) -> None:
    """`f` again while a file is held for now; any other error at once."""
    pause = 0.1
    for i in range(1, max(tries, 1) + 1):
        try:
            f()
            return
        except OSError as e:
            if i >= tries or not _transient(e):
                raise
        time.sleep(pause)
        pause = min(pause * 2, 2.0)


def _count_files(d: Path) -> int:
    """Files and links under `d`, links not followed."""
    try:
        entries = list(os.scandir(d))
    except OSError:
        return 0
    n = 0
    for e in entries:
        try:
            n += _count_files(Path(e.path)) if e.is_dir(follow_symlinks=False) else 1
        except OSError:
            pass
    return n


def _engine_paths(root: Path, top: str):
    return root / top, root / f".{top}.previous", root / f".{top}.swapping"


def _write_journal(path: Path, files: int) -> None:
    """On disk before the rename it announces, whole or not at all."""
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(json.dumps({"files": files}))
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def _read_journal(path: Path) -> Optional[int]:
    try:
        v = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    files = v.get("files") if isinstance(v, dict) else None
    return files if isinstance(files, int) and not isinstance(files, bool) else None


def _recover_engine(root: Path, top: str, rename=os.rename, rmtree=_rmtree, tries: int = RENAME_TRIES) -> bool:
    """Puts `<root>/.<top>.previous` back when a run died between the two
    renames of an update (or could not undo the first) — only as its journal
    left it, and only whole. Any other copy is a leftover and goes: one a
    cleanup left half removed never stands in for a missing engine. True when
    the engine was put back."""
    target, previous, journal = _engine_paths(root, top)
    if os.path.lexists(previous):
        moved = _read_journal(journal)
        if not os.path.lexists(target) and moved is not None and _count_files(previous) == moved:
            try:
                _retry(lambda: rename(previous, target), tries)
            except OSError:
                return False  # still held: kept, with its journal, for the next try
            _unlink_quiet(journal)
            return True
        try:
            rmtree(previous)
        except OSError:
            pass  # the next install() tries again
    if not os.path.lexists(previous):
        _unlink_quiet(journal)
    return False


def _swap_engine_tree(root: Path, top: str, staged: Path, rename=os.rename, rmtree=_rmtree,
                      tries: int = RENAME_TRIES) -> None:
    """Swaps the complete tree at `staged` in for `<root>/<top>`: live →
    previous, staged → live, the first undone if the second fails. Any failure
    leaves the engine that was there (or `.previous` with its journal, put back
    by the next install())."""
    target, previous, journal = _engine_paths(root, top)
    _recover_engine(root, top, rename, rmtree, tries)
    if os.path.lexists(previous):
        if os.path.lexists(target):
            raise RuntimeError(f"a copy of an earlier engine at {previous} could not be removed; delete it and try again")
        raise RuntimeError(
            f"the previous engine at {previous} could not be put back yet; close the browsers "
            "started from it and try again"
        )
    had_live = os.path.lexists(target)
    if had_live:
        _write_journal(journal, _count_files(target))
        try:
            _retry(lambda: rename(target, previous), tries)
        except OSError as e:
            _unlink_quiet(journal)
            raise RuntimeError(
                f"the engine at {target} is in use and was not replaced ({e}); "
                "close the browsers started from it and try again"
            ) from e
    try:
        _retry(lambda: rename(staged, target), tries)
    except OSError as e:
        if had_live:
            try:
                _retry(lambda: rename(previous, target), tries)
            except OSError:
                # The journal stays: it is what puts the engine back.
                raise RuntimeError(
                    f"replacing the engine failed ({e}) and the previous one could not be put "
                    f"back yet; it is kept at {previous} and restored by the next install()"
                ) from e
            _unlink_quiet(journal)
        raise
    _unlink_quiet(journal)
    try:
        rmtree(previous)
    except OSError:
        pass  # a tree Windows still holds is clutter: the next install() removes it


def _read_staged(staging: Path, top: str, binary_subpath) -> Optional[dict]:
    """A tree an earlier run unpacked, checked and noted, still whole."""
    try:
        v = json.loads((staging / STAGED_NOTE).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(v, dict) or not isinstance(v.get("etag"), str) \
            or not isinstance(v.get("files"), int) or isinstance(v.get("files"), bool):
        return None
    if not staging.joinpath(*binary_subpath).exists() or _count_files(staging / top) != v["files"]:
        return None
    return v


def _sweep_leftovers(root: Path, top: str, key: str, binary_subpath) -> None:
    """What an install that died left: an unfinished staging folder, a partial
    download. A checked staged tree stays for the next swap."""
    staging = root / f".{top}.staging"
    if os.path.lexists(staging) and _read_staged(staging, top, binary_subpath) is None:
        shutil.rmtree(staging, ignore_errors=True)
    _unlink_quiet(root / f".{key}.tmp")


def _free_bytes(d: Path) -> Optional[int]:
    """Bytes the current user may still write on the disk holding `d`."""
    try:
        return shutil.disk_usage(d).free
    except OSError:
        return None


def _assert_room(d: Path, need: int) -> None:
    free = _free_bytes(d)
    if free is not None and free < need:
        mb = lambda n: -(-n // (1 << 20))  # noqa: E731
        raise RuntimeError(
            f"not enough free disk space for the engine update: it needs about {mb(need)} MB and "
            f"{mb(free)} MB is free in {d}; the engine you have was not touched"
        )


def _processes_under(d: Path) -> int:
    """How many processes run from `d`. On macOS and Linux a swap would pull
    the files from under them; Windows refuses the rename itself."""
    if not os.path.isdir(d):
        return 0
    roots = {os.path.abspath(d).rstrip(os.sep) + os.sep, os.path.realpath(d).rstrip(os.sep) + os.sep}
    if sys.platform.startswith("linux"):
        n = 0
        for pid in os.listdir("/proc"):
            if not pid.isdigit():
                continue
            try:
                exe = os.readlink(f"/proc/{pid}/exe")
            except OSError:
                continue  # gone, or not ours
            # A replaced binary reads "<path> (deleted)": still that folder.
            if exe.endswith(" (deleted)"):
                exe = exe[: -len(" (deleted)")]
            if any(exe.startswith(r) for r in roots):
                n += 1
        return n
    if sys.platform == "darwin":
        try:
            out = subprocess.run(["ps", "-axwwo", "comm="], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False).stdout
        except OSError:
            return 0
        lines = out.decode("utf-8", errors="replace").splitlines()
        return sum(1 for line in lines if any(line.strip().startswith(r) for r in roots))
    return 0


def _host() -> str:
    return socket.gethostname().lower()


def _pid_alive(pid) -> bool:
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        return False
    if sys.platform == "win32":
        # os.kill(pid, 0) would end the process here: ask for its exit code instead.
        import ctypes
        from ctypes import wintypes
        k = ctypes.WinDLL("kernel32", use_last_error=True)
        k.OpenProcess.restype = wintypes.HANDLE
        k.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
        k.GetExitCodeProcess.argtypes = (wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD))
        k.CloseHandle.argtypes = (wintypes.HANDLE,)
        h = k.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not h:
            return ctypes.get_last_error() == 5  # access denied: it is there
        try:
            code = wintypes.DWORD()
            return not k.GetExitCodeProcess(h, ctypes.byref(code)) or code.value == 259  # STILL_ACTIVE
        finally:
            k.CloseHandle(h)
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


class _InstallLock:
    """One install at a time over a cache dir, across processes and the three
    SDKs (same file, same rules). A holder that died is taken over."""

    def __init__(self, path: Path, token: str):
        self.path = path
        self.token = token

    @classmethod
    def acquire(cls, root: Path, poll: float = 0.5) -> "_InstallLock":
        path = root / ".install.lock"
        token = secrets.token_hex(8)
        told = False
        while True:
            if _create_lock(path, token):
                return cls(path, token)
            seen = _read_lock(path)
            if seen is None:
                continue  # released meanwhile
            if _lock_is_stale(path, seen):
                _steal_lock(path, seen, token)
                continue
            if not told and seen.get("pid") != os.getpid():
                warnings.warn(
                    f"[shardx] waiting for another process (pid {seen.get('pid', '?')}) to finish "
                    f"installing the engine… (if none is running, delete {path})",
                    RuntimeWarning,
                    stacklevel=3,
                )
                told = True
            time.sleep(poll + random.random() * 0.1)

    def held(self) -> bool:
        """Still ours: nobody took it over as stale."""
        seen = _read_lock(self.path)
        return seen is not None and seen.get("token") == self.token

    def release(self) -> None:
        if self.held():
            _unlink_quiet(self.path)


def _create_lock(path: Path, token: str) -> bool:
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    except FileExistsError:
        return False
    except PermissionError:
        if sys.platform == "win32":
            return False  # one being removed this instant
        raise
    try:
        body = {"pid": os.getpid(), "host": _host(), "token": token, "at": int(time.time() * 1000)}
        os.write(fd, json.dumps(body).encode("utf-8"))
        os.fsync(fd)
    finally:
        os.close(fd)
    return True


def _read_lock(path: Path) -> Optional[dict]:
    """None when there is no lock; {} when it cannot be read as one."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    try:
        v = json.loads(text)
    except ValueError:
        return {}
    return v if isinstance(v, dict) else {}


def _lock_is_stale(path: Path, seen: dict) -> bool:
    try:
        mtime = path.stat().st_mtime * 1000
    except OSError:
        return False
    now = time.time() * 1000
    # Unreadable: being written this instant, or by a process that died doing so.
    if not isinstance(seen.get("token"), str):
        return now - mtime > 10_000
    at = seen.get("at")
    if now - (at if isinstance(at, (int, float)) and not isinstance(at, bool) else mtime) > LOCK_MAX_AGE_MS:
        return True
    # Another computer's process cannot be asked about.
    if seen.get("host") != _host():
        return False
    return not _pid_alive(seen.get("pid"))


def _steal_lock(path: Path, seen: dict, token: str) -> None:
    """Moves the stale lock aside and drops it, unless what moved was a fresh
    one another process took in the meantime: that one is put back."""
    aside = path.with_name(f"{path.name}.stale-{token}")
    try:
        os.rename(path, aside)
    except OSError:
        return
    got = _read_lock(aside)
    if got is not None and got.get("token") != seen.get("token"):
        try:
            os.link(aside, path)
        except OSError:
            pass  # a third one has it
    _unlink_quiet(aside)


_NATIVE_MAGIC = (
    b"\x7fELF",                                              # Linux/BSD ELF
    b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",               # Mach-O 64-bit BE / LE
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",               # Mach-O 32-bit BE / LE
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",               # Mach-O universal
)


def _fix_unix_exec_bits(root: Path) -> None:
    """Walk `root` and add +x to every file whose first 4 bytes are an
    ELF / Mach-O magic.  Required because Windows zip producers don't
    store Unix exec bits, so chrome / chrome_crashpad_handler / chrome_sandbox
    all come out non-executable on Linux."""
    for p in root.rglob("*"):
        try:
            if not p.is_file() or p.is_symlink():
                continue
            with p.open("rb") as f:
                head = f.read(4)
            if any(head.startswith(m) for m in _NATIVE_MAGIC):
                p.chmod(p.stat().st_mode | 0o111)
        except OSError:
            pass


def _system_unzip(archive: Path, dest: Path) -> None:
    """Extract via /usr/bin/unzip — preserves symlinks and permission
    bits that Python's zipfile silently drops.  Required for any
    macOS .app bundle (Versions/Current symlinks + Helper exec bits).

    Accepts exit code 0 (clean) and 1 (warnings — e.g. "backslashes in
    path" for archives zipped on Windows; extraction still completes
    correctly).  Only 2+ are real fatal errors per unzip(1).
    """
    dest.mkdir(parents=True, exist_ok=True)
    try:
        proc = subprocess.run(
            ["unzip", "-q", "-o", str(archive), "-d", str(dest)],
            check=False,
            # On a full disk unzip asks "Continue? (y/n)" on stdin; nobody answers.
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
    except FileNotFoundError as e:
        raise RuntimeError(
            "system `unzip` not found — install with "
            "`apt install unzip` / `brew install unzip`"
        ) from e
    if proc.returncode > 1:
        raise RuntimeError(
            f"unzip failed for {archive.name} (exit {proc.returncode}): "
            f"{proc.stderr.decode(errors='replace')[:400]}"
        )
