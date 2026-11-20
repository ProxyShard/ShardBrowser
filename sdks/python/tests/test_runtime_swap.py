"""Engine updates that fail at each step must leave a working engine (#122).

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import errno
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
import warnings
import zipfile
from pathlib import Path

# The swap needs neither the network nor the browser driver: runtime.py is
# loaded on its own, with httpx stood in for when it is not installed.
if "httpx" not in sys.modules:
    try:
        import httpx  # noqa: F401
    except ImportError:
        sys.modules["httpx"] = types.ModuleType("httpx")
RUNTIME_PY = Path(__file__).resolve().parents[1] / "shardx" / "runtime.py"
_spec = importlib.util.spec_from_file_location("shardx_runtime_under_test", RUNTIME_PY)
rt = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = rt  # its dataclasses look themselves up there
_spec.loader.exec_module(rt)

TOP = "ShardX-Test"


def tree(d: Path, version: str) -> None:
    d.mkdir(parents=True, exist_ok=True)
    (d / "chrome").write_text("engine")
    (d / "version.txt").write_text(version)
    (d / f"only-in-{version}").write_text("x")


def version(d: Path) -> str:
    return (d / "version.txt").read_text()


def refusing(*bad: Path):
    def rename(a, b):
        if Path(a) in bad:
            raise PermissionError(32, "The process cannot access the file because it is being used by another process")
        os.rename(a, b)
    return rename


class SwapTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="shardx-swap-"))
        self.live = self.root / TOP
        self.previous = self.root / f".{TOP}.previous"
        self.journal = self.root / f".{TOP}.swapping"
        self.staged = self.root / f".{TOP}.staging" / TOP
        tree(self.live, "156")
        tree(self.staged, "157")

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def test_an_update_replaces_the_engine_whole(self):
        rt._swap_engine_tree(self.root, TOP, self.staged)
        self.assertEqual(version(self.live), "157")
        self.assertFalse((self.live / "only-in-156").exists())
        self.assertFalse(self.previous.exists() or self.journal.exists())

    def test_a_first_rename_refused_leaves_everything(self):
        with self.assertRaisesRegex(RuntimeError, "in use"):
            rt._swap_engine_tree(self.root, TOP, self.staged, rename=refusing(self.live))
        self.assertEqual(version(self.live), "156")
        self.assertTrue(self.staged.exists())
        self.assertFalse(self.journal.exists())

    def test_a_second_rename_that_fails_puts_the_engine_back(self):
        with self.assertRaises(PermissionError):
            rt._swap_engine_tree(self.root, TOP, self.staged, rename=refusing(self.staged))
        self.assertEqual(version(self.live), "156")
        self.assertFalse(self.previous.exists() or self.journal.exists())

    def test_a_rename_refused_for_a_moment_is_tried_again(self):
        refusals = []

        def rename(a, b):
            if Path(a) == self.staged and len(refusals) < 2:
                refusals.append(a)
                raise OSError(errno.EBUSY, "Resource busy")
            os.rename(a, b)
        rt._swap_engine_tree(self.root, TOP, self.staged, rename=rename, tries=4)
        self.assertEqual(len(refusals), 2)
        self.assertEqual(version(self.live), "157")

    def test_a_failed_put_back_is_restored_by_the_next_install(self):
        with self.assertRaisesRegex(RuntimeError, "restored by the next install"):
            rt._swap_engine_tree(self.root, TOP, self.staged, rename=refusing(self.staged, self.previous))
        self.assertFalse(self.live.exists())
        self.assertTrue(self.journal.exists(), "the journal is what lets it back")
        self.assertTrue(rt._recover_engine(self.root, TOP))
        self.assertEqual(version(self.live), "156")
        self.assertFalse(self.journal.exists())

    def test_a_run_killed_between_the_renames_comes_back_to_the_old_engine(self):
        # A real exit: the second rename never returns and nothing after it runs.
        code = f"""
import importlib.util, os, sys, types
from pathlib import Path
sys.modules.setdefault("httpx", types.ModuleType("httpx"))
spec = importlib.util.spec_from_file_location("rt", {str(RUNTIME_PY)!r})
rt = importlib.util.module_from_spec(spec); sys.modules["rt"] = rt; spec.loader.exec_module(rt)
staged = Path({str(self.staged)!r})
def rename(a, b):
    if Path(a) == staged:
        os._exit(9)
    os.rename(a, b)
rt._swap_engine_tree(Path({str(self.root)!r}), {TOP!r}, staged, rename=rename)
"""
        r = subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, text=True)
        self.assertEqual(r.returncode, 9, r.stderr)
        self.assertTrue(not self.live.exists() and self.previous.exists() and self.journal.exists())

        self.assertTrue(rt._recover_engine(self.root, TOP))
        self.assertEqual(version(self.live), "156")
        self.assertFalse(self.previous.exists() or self.journal.exists())
        rt._swap_engine_tree(self.root, TOP, self.staged)
        self.assertEqual(version(self.live), "157")

    def test_a_leftover_previous_copy_is_removed_never_swapped_back(self):
        tree(self.previous, "half-of-155")
        self.assertFalse(rt._recover_engine(self.root, TOP))
        self.assertFalse(self.previous.exists())
        tree(self.previous, "half-of-155")
        with self.assertRaises(PermissionError):
            rt._swap_engine_tree(self.root, TOP, self.staged, rename=refusing(self.staged))
        self.assertEqual(version(self.live), "156")

    def test_a_copy_a_cleanup_left_half_removed_is_never_put_back(self):
        # A cleanup cut short after a finished swap: no journal, part of the files.
        tree(self.previous, "155")
        (self.previous / "version.txt").unlink()
        shutil.rmtree(self.live)
        self.assertFalse(rt._recover_engine(self.root, TOP))
        self.assertFalse(self.live.exists() or self.previous.exists())

    def test_an_unfinished_swap_whose_previous_tree_lost_a_file_is_not_trusted(self):
        with self.assertRaises(RuntimeError):
            rt._swap_engine_tree(self.root, TOP, self.staged, rename=refusing(self.staged, self.previous))
        (self.previous / "version.txt").unlink()
        self.assertFalse(rt._recover_engine(self.root, TOP))
        self.assertFalse(self.live.exists())

    def test_a_cleanup_windows_refuses_does_not_fail_the_update(self):
        def locked(p):
            if Path(p) == self.previous:
                raise PermissionError(5, "Access is denied")
            shutil.rmtree(p)
        rt._swap_engine_tree(self.root, TOP, self.staged, rmtree=locked)
        self.assertEqual(version(self.live), "157")
        self.assertTrue(self.previous.exists())
        self.assertFalse(rt._recover_engine(self.root, TOP))
        self.assertFalse(self.previous.exists(), "removed by the next install()")

    def test_a_first_install_has_nothing_to_move_aside(self):
        shutil.rmtree(self.live)
        rt._swap_engine_tree(self.root, TOP, self.staged)
        self.assertEqual(version(self.live), "157")

    @unittest.skipIf(sys.platform == "win32", "on Windows the browser's open files refuse the rename")
    def test_processes_running_from_the_engine_folder_are_counted(self):
        d = self.root / "engine"
        d.mkdir()
        # macOS may kill a copied system binary at exec; a copy of node runs there.
        # Each under its own name: macOS also kills a binary rewritten in place.
        for i, (source, args) in enumerate((("/bin/sleep", ["30"]),
                                            (shutil.which("node"), ["-e", "setTimeout(() => {}, 30000)"]))):
            if not source:
                continue
            binary = d / f"browser-{i}"
            shutil.copy(source, binary)
            binary.chmod(0o755)
            self.assertEqual(rt._processes_under(d), 0)
            p = subprocess.Popen([str(binary), *args])
            try:
                n = 0
                for _ in range(60):
                    n = rt._processes_under(d)
                    if n or p.poll() is not None:
                        break
                    time.sleep(0.05)
                if p.poll() is not None:
                    continue
                self.assertEqual(n, 1)
                self.assertEqual(rt._processes_under(self.root / "elsewhere"), 0)
                return
            finally:
                p.kill()
                p.wait()
        self.skipTest("no binary that runs from a copy here")


class LockTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="shardx-lock-"))
        self.path = self.root / ".install.lock"

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def write(self, **body):
        self.path.write_text(json.dumps(body))

    def acquire_in_background(self):
        got = []
        t = threading.Thread(target=lambda: got.append(rt._InstallLock.acquire(self.root, poll=0.02)), daemon=True)
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            t.start()
            t.join(0.3)
        return t, got

    def test_a_lock_another_live_process_holds_is_waited_for(self):
        self.write(pid=os.getpid(), host=socket.gethostname().lower(), token="other", at=int(time.time() * 1000))
        t, got = self.acquire_in_background()
        self.assertEqual(got, [], "taken while held")
        self.path.unlink()
        t.join(5)
        self.assertTrue(got and got[0].held())
        got[0].release()
        self.assertFalse(self.path.exists())

    def test_the_lock_of_a_process_that_died_is_taken_over(self):
        gone = subprocess.run([sys.executable, "-c", "import os; print(os.getpid())"], capture_output=True, text=True)
        self.write(pid=int(gone.stdout), host=socket.gethostname().lower(), token="dead", at=int(time.time() * 1000))
        lock = rt._InstallLock.acquire(self.root, poll=0.02)
        self.assertTrue(lock.held())
        self.assertEqual(json.loads(self.path.read_text())["pid"], os.getpid())
        lock.release()

    def test_an_unreadable_lock_is_taken_only_once_it_is_old(self):
        self.path.write_text("")
        t, got = self.acquire_in_background()
        self.assertEqual(got, [])
        old = time.time() - 60
        os.utime(self.path, (old, old))
        t.join(5)
        self.assertTrue(got and got[0].held())
        got[0].release()

    def test_a_holder_whose_lock_was_taken_over_knows_it(self):
        lock = rt._InstallLock.acquire(self.root)
        self.write(pid=1, host="elsewhere", token="new", at=int(time.time() * 1000))
        self.assertFalse(lock.held())
        lock.release()
        self.assertTrue(self.path.exists(), "not ours to remove")

    def test_the_lock_reads_as_the_node_and_rust_sdks_write_it(self):
        lock = rt._InstallLock.acquire(self.root)
        body = json.loads(self.path.read_text())
        self.assertEqual(set(body), {"pid", "host", "token", "at"})
        self.assertEqual(body["host"], socket.gethostname().lower())
        self.assertIsInstance(body["at"], int)
        lock.release()


class InstallTest(unittest.TestCase):
    """install() end to end on fake archives, the network stood in for."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="shardx-install-"))
        try:
            self.spec = rt.host_spec()
        except RuntimeError as e:  # a host the SDK ships no engine for
            self.skipTest(str(e))
        self.sub = self.spec.binary_subpath
        self.top = self.sub[0]
        self.key = self.spec.browser.key
        self.live = self.root / self.top
        self.staging = self.root / f".{self.top}.staging"
        self.saved = (rt._free_bytes, rt._processes_under)

    def tearDown(self):
        rt._free_bytes, rt._processes_under = self.saved
        shutil.rmtree(self.root, ignore_errors=True)

    def entries(self, v: str, extra: int = 0) -> dict:
        e = {
            "/".join(self.sub): f"engine {v}",
            f"{self.top}/version.txt": v,
            f"{self.top}/resources.pak": "R" * 256,
        }
        if sys.platform == "darwin":
            e[f"{self.top}/ShardX.app/Contents/Frameworks/ShardX Framework.framework/Versions/{v}/ShardX Framework"] = "fw"
        if sys.platform == "win32":
            e[f"{self.top}/{v}.manifest"] = "<assembly/>"
        for i in range(extra):
            e[f"{self.top}/f/{i}.bin"] = f"file {i}"
        return e

    @staticmethod
    def zip_bytes(entries: dict) -> bytes:
        tmp = Path(tempfile.mkstemp(suffix=".zip")[1])
        try:
            with zipfile.ZipFile(tmp, "w", zipfile.ZIP_STORED) as z:
                for name, body in entries.items():
                    z.writestr(name, body)
            return tmp.read_bytes()
        finally:
            tmp.unlink()

    def installed(self, v: str) -> None:
        for name, body in self.entries(v).items():
            p = self.root / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(body)
        (self.live / f"only-in-{v}").write_text("x")
        (self.root / "fingerprints").mkdir()
        (self.root / "fingerprints" / "a.json").write_text("{}")
        (self.root / "manifest.json").write_text(json.dumps({
            "browser_etag": f"e{v}", "widevine_etag": "w1", "fingerprints_etag": "f1",
            "installed_chromium_version": v,
        }))

    def remote(self, v: str, **more) -> dict:
        return {"chromium_version": v, "archives": {self.key: f"e{v}", "ShardX-Fingerprints.zip": "f1"}, **more}

    def runtime(self, manifest: dict, body: bytes = None, calls: list = None) -> "rt.Runtime":
        r = rt.Runtime(cache_dir=self.root)
        r._fetch_manifest = lambda: manifest

        def download(arch, dest, unpacks=False):
            if calls is not None:
                calls.append(arch.key)
            if arch.key != self.key or body is None:
                raise RuntimeError(f"download {arch.key}: HTTP 404")
            tmp = dest / f".{arch.key}.tmp"
            tmp.write_bytes(body)
            return tmp, f"e{manifest['chromium_version']}"
        r._download = download
        return r

    def install(self, r) -> list:
        with warnings.catch_warnings(record=True) as seen:
            warnings.simplefilter("always")
            r.install()
        return [str(w.message) for w in seen]

    def record(self) -> dict:
        return json.loads((self.root / "manifest.json").read_text())

    def test_an_update_swaps_the_engine_in_and_records_it_at_once(self):
        self.installed("156.0.0.1")
        calls = []
        r = self.runtime(self.remote("157.0.0.1"), self.zip_bytes(self.entries("157.0.0.1")), calls)
        seen = self.install(r)
        self.assertEqual(version(self.live), "157.0.0.1")
        self.assertFalse((self.live / "only-in-156.0.0.1").exists())
        self.assertEqual(r.chromium_version, "157.0.0.1")
        m = self.record()
        self.assertEqual(m["installed_chromium_version"], "157.0.0.1")
        self.assertEqual(m["browser_etag"], "e157.0.0.1")
        # The CDM comes from Google at launch, never from the CDN; the old record stays readable.
        self.assertFalse(any("Widevine" in c for c in calls), calls)
        self.assertEqual(m["widevine_etag"], "w1")
        self.assertEqual(seen, [])
        self.assertFalse(self.staging.exists() or (self.root / ".install.lock").exists())

    def check_stays(self, body: bytes) -> None:
        self.installed("156.0.0.1")
        r = self.runtime(self.remote("157.0.0.1"), body)
        seen = self.install(r)
        self.assertEqual(version(self.live), "156.0.0.1")
        self.assertEqual(r.chromium_version, "156.0.0.1", "profiles keep claiming the engine they run on")
        self.assertEqual(self.record()["installed_chromium_version"], "156.0.0.1")
        self.assertTrue(any("Staying on the installed engine" in w for w in seen), seen)
        self.assertFalse(self.staging.exists())
        self.assertFalse((self.root / f".{self.key}.tmp").exists())

    def test_a_damaged_archive_leaves_the_engine_and_the_run_goes_on(self):
        self.check_stays(b"PK\x03\x04 cut short")

    def test_an_archive_without_the_engine_leaves_the_engine(self):
        self.check_stays(self.zip_bytes({f"{self.top}/readme.txt": "hi"}))

    def test_an_unpack_that_fails_half_way_leaves_the_engine(self):
        body = bytearray(self.zip_bytes(self.entries("157.0.0.1")))
        body[body.index(b"RRRRRRRR")] = ord("X")  # its CRC no longer matches
        self.check_stays(bytes(body))

    def test_with_nothing_installed_a_failed_install_is_an_error(self):
        r = self.runtime(self.remote("157.0.0.1"), b"not a zip")
        with self.assertRaisesRegex(RuntimeError, "not a readable zip"):
            self.install(r)
        self.assertFalse(self.live.exists() or (self.root / ".install.lock").exists())

    def test_not_enough_room_refuses_before_unpacking(self):
        self.installed("156.0.0.1")
        tmp = self.root / f".{self.key}.tmp"
        tmp.write_bytes(self.zip_bytes(self.entries("157.0.0.1", 50)))
        # Enough for the margin alone, not for the files.
        rt._free_bytes = lambda d: rt.SPACE_MARGIN + 10
        r = rt.Runtime(cache_dir=self.root)
        with self.assertRaisesRegex(RuntimeError, "not enough free disk space"):
            r._stage_engine(tmp, "e157.0.0.1")
        self.assertFalse((self.staging / self.top).exists())
        self.assertFalse(tmp.exists())
        self.assertEqual(version(self.live), "156.0.0.1")

    def test_running_browsers_refuse_the_swap_and_the_next_run_swaps_without_downloading(self):
        self.installed("156.0.0.1")
        body = self.zip_bytes(self.entries("157.0.0.1"))
        rt._processes_under = lambda d: 2
        busy = self.runtime(self.remote("157.0.0.1"), body)
        seen = self.install(busy)
        self.assertEqual(version(self.live), "156.0.0.1")
        self.assertEqual(busy.chromium_version, "156.0.0.1")
        self.assertTrue(any("2 processes are still running" in w for w in seen), seen)
        self.assertTrue((self.staging / "staged.json").exists(), "the unpacked update is kept")

        rt._processes_under = lambda d: 0
        calls = []
        self.install(self.runtime(self.remote("157.0.0.1"), body, calls))
        self.assertEqual(version(self.live), "157.0.0.1")
        self.assertNotIn(self.key, calls, "not downloaded again")
        self.assertFalse(self.staging.exists())

    def test_a_process_that_checked_earlier_follows_an_engine_another_swapped_in(self):
        self.installed("156.0.0.1")
        early = self.runtime(self.remote("156.0.0.1"))
        self.install(early)
        self.assertEqual(early.chromium_version, "156.0.0.1")
        later = self.runtime(self.remote("157.0.0.1", grease_brand="Not.A/Brand", grease_version="99"),
                             self.zip_bytes(self.entries("157.0.0.1")))
        self.install(later)
        early._fetch_manifest = lambda: self.fail("no network needed")
        self.install(early)
        self.assertEqual(early.chromium_version, "157.0.0.1")
        self.assertEqual(early.grease_brand, "Not.A/Brand")

    def test_two_processes_updating_one_cache_at_once_take_turns(self):
        self.installed("156.0.0.1")
        entries = self.entries("157.0.0.1", 3000)
        archive = self.root.parent / f"{self.root.name}-engine.zip"
        archive.write_bytes(self.zip_bytes(entries))
        try:
            worker = Path(__file__).with_name("install_worker.py")
            procs = [subprocess.Popen([sys.executable, "-I", str(worker), str(RUNTIME_PY), str(self.root), str(archive)],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(2)]
            outs = [p.communicate(timeout=120) for p in procs]
        finally:
            archive.unlink()
        for p, (out, err) in zip(procs, outs):
            self.assertEqual(p.returncode, 0, err)
        results = [json.loads(out.strip().splitlines()[-1]) for out, _ in outs]
        self.assertTrue(all(r["ok"] and r["version"] == "157.0.0.1" for r in results), results)
        self.assertEqual(sum(r["downloaded"] for r in results), 1, "the second found it done")
        self.assertEqual(version(self.live), "157.0.0.1")
        self.assertEqual(rt._count_files(self.live), len(entries))
        self.assertFalse((self.root / ".install.lock").exists() or self.staging.exists())


if __name__ == "__main__":
    unittest.main()
