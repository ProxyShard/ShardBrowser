"""Phone profiles report the Chrome for Android build of the engine's major.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import importlib.util
import sys
import types
import unittest
from pathlib import Path

if "httpx" not in sys.modules:
    try:
        import httpx  # noqa: F401
    except ImportError:
        sys.modules["httpx"] = types.ModuleType("httpx")
RUNTIME_PY = Path(__file__).resolve().parents[1] / "shardx" / "runtime.py"
_spec = importlib.util.spec_from_file_location("shardx_runtime_android_test", RUNTIME_PY)
rt = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = rt
_spec.loader.exec_module(rt)


def phone():
    return {"navigator": {"user_agent": "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 "
                                        "(KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36"},
            "client_hints": {"mobile": True, "brand_full_version": "156.0.8078.48"}}


class AndroidVersionTest(unittest.TestCase):
    def test_a_phone_gets_the_android_build(self):
        cfg = phone()
        rt.apply_engine_version(cfg, "157.0.8103.39", android_version="157.0.8103.38")
        ch = cfg["client_hints"]
        self.assertEqual((ch["brand_version"], ch["brand_full_version"], ch["chrome_patch"]),
                         ("157", "157.0.8103.38", 38))
        self.assertIn("Chrome/157.0.0.0 Mobile", cfg["navigator"]["user_agent"])

    def test_no_android_build_or_another_major_keeps_the_engines(self):
        for android in (None, "156.0.8078.81", "157.0.8103", ""):
            cfg = phone()
            rt.apply_engine_version(cfg, "157.0.8103.39", android_version=android)
            self.assertEqual(cfg["client_hints"]["brand_full_version"], "157.0.8103.39", android)


if __name__ == "__main__":
    unittest.main()
