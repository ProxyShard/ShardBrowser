"""#147: a profile reports only an OS version Chrome itself can report.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import importlib.util
import sys
import types
import unittest
from pathlib import Path

# randomize.py imports `.host`; give it a package with a stand-in for that.
_PKG = "shardx_pv_test"
_pkg = types.ModuleType(_PKG)
_pkg.__path__ = []
_host = types.ModuleType(f"{_PKG}.host")
_host.host_logical_cores = lambda: 8
_host.host_ram_bucket_gb = lambda: 16
sys.modules[_PKG] = _pkg
sys.modules[f"{_PKG}.host"] = _host
RANDOMIZE_PY = Path(__file__).resolve().parents[1] / "shardx" / "randomize.py"
_spec = importlib.util.spec_from_file_location(f"{_PKG}.randomize", RANDOMIZE_PY)
rz = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = rz
_spec.loader.exec_module(rz)

M1 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)"
M4 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version)"
M5 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)"


def profile(platform, version, renderer=""):
    return {"navigator": {"platform": platform, "platform_version": version},
            "client_hints": {"platform_version": version},
            "webgl": {"renderer": renderer}}


def pv(cfg):
    return cfg["navigator"]["platform_version"], cfg["client_hints"]["platform_version"]


class PlatformVersionTest(unittest.TestCase):
    def test_windows_keeps_what_chrome_reports_and_moves_the_rest(self):
        for old, new in (("10.0.0", "10.0.0"), ("14.0.0", "14.0.0"), ("15.0.0", "15.0.0"),
                         ("19.0.0", "19.0.0"), ("13.0.0", "19.0.0"), ("16.0.0", "19.0.0"),
                         ("17.0.0", "19.0.0"), ("20.0.0", "19.0.0"), ("5.0.0", "10.0.0"),
                         ("12.0.0", "10.0.0"), ("", "19.0.0"), ("x", "19.0.0")):
            cfg = profile("Windows", old)
            self.assertEqual(rz.settle_platform_version(cfg), old != new, old)
            self.assertEqual(pv(cfg), (new, new), old)

    def test_mac_gets_three_parts_and_a_version_its_chip_runs(self):
        for old, gpu, new in (("15.6.1", M1, "15.6.1"), ("15.4", M1, "15.4.0"), ("26", M1, "26.0.0"),
                              ("14.7", M1, "14.7.0"), ("14.7", M4, "15.8.2"), ("15.6.1", M4, "15.6.1"),
                              ("15.6.1", M5, "26.7.2"), ("26.1", M5, "26.1.0"), ("10.15.7", M1, "26.7.2"),
                              ("", M1, "26.7.2"), ("15.6.1.2", M1, "26.7.2")):
            cfg = profile("macOS", old, gpu)
            rz.settle_platform_version(cfg)
            self.assertEqual(pv(cfg), (new, new), (old, gpu))
            self.assertFalse(rz.settle_platform_version(cfg), (old, "once"))

    def test_client_hints_win_and_navigator_is_brought_along(self):
        cfg = profile("Windows", "15.0.0")
        cfg["navigator"]["platform_version"] = "17.0.0"
        self.assertTrue(rz.settle_platform_version(cfg))
        self.assertEqual(pv(cfg), ("15.0.0", "15.0.0"))

    def test_linux_and_phones_are_left_alone(self):
        self.assertFalse(rz.settle_platform_version(profile("Linux", "6.8.0")))
        self.assertFalse(rz.settle_platform_version(profile("Android", "16.0.0")))

    def test_every_pool_value_is_already_settled(self):
        for v in rz.WINDOWS_PLATFORM_VERSIONS:
            self.assertFalse(rz.settle_platform_version(profile("Windows", v)), v)
        for v in rz.MACOS_PLATFORM_VERSIONS:
            self.assertFalse(rz.settle_platform_version(profile("macOS", v, M1)), v)
        for _ in range(200):
            cfg = profile("macOS", "", M5)
            rz.randomize_platform_version(cfg)
            self.assertGreaterEqual(int(pv(cfg)[0].split(".")[0]), 26, pv(cfg))
            self.assertFalse(rz.settle_platform_version(cfg))


if __name__ == "__main__":
    unittest.main()
