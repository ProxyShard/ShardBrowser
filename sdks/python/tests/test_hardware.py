"""A desktop profile never claims fewer than four logical cores, as in the launcher.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import importlib.util
import sys
import types
import unittest
from pathlib import Path

# randomize.py imports `.host`; a stand-in reports the host cores each case asks for.
_PKG = "shardx_hw_test"
_pkg = types.ModuleType(_PKG)
_pkg.__path__ = []
_host = types.ModuleType(f"{_PKG}.host")
_host.cores = 8
_host.host_logical_cores = lambda: _host.cores
_host.host_ram_bucket_gb = lambda: 16
sys.modules[_PKG] = _pkg
sys.modules[f"{_PKG}.host"] = _host
_spec = importlib.util.spec_from_file_location(f"{_PKG}.randomize", Path(__file__).resolve().parents[1] / "shardx" / "randomize.py")
rz = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = rz
_spec.loader.exec_module(rz)


class HardwareTest(unittest.TestCase):
    def test_the_randomizer_never_picks_under_four_cores_for_a_desktop(self):
        self.assertGreaterEqual(min(rz.X86_CORES), 4)
        for pairs in rz.MAC_HW_CONFIGS.values():
            for cores, _ in pairs:
                self.assertGreaterEqual(cores, 4)
        for host in (1, 2, 4, 6, 16):
            _host.cores = host
            for _ in range(100):
                for platform, pid in (("Windows", "win-x"), ("Linux", "linux-x"), ("macOS", "mac-m1-air13")):
                    cfg = {"navigator": {"platform": platform, "hardware_concurrency": 2}}
                    rz.randomize_hardware(cfg, pid)
                    self.assertGreaterEqual(cfg["navigator"]["hardware_concurrency"], 4, (host, platform))


if __name__ == "__main__":
    unittest.main()
