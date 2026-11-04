"""A saved profile picks up what its library template gained after it was frozen.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import importlib.util
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

if "httpx" not in sys.modules:
    try:
        import httpx  # noqa: F401
    except ImportError:
        sys.modules["httpx"] = types.ModuleType("httpx")
RUNTIME_PY = Path(__file__).resolve().parents[1] / "shardx" / "runtime.py"
_spec = importlib.util.spec_from_file_location("shardx_runtime_adopt_test", RUNTIME_PY)
rt = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = rt
_spec.loader.exec_module(rt)
adopt_library_fields = rt.adopt_library_fields


class AdoptLibraryTest(unittest.TestCase):
    def setUp(self):
        self.lib = Path(tempfile.mkdtemp(prefix="shardx-lib-"))
        (self.lib / "win-rtx3060.json").write_text(json.dumps({"name": "win-rtx3060", "fonts_local": ["Arial"], "webgpu": {
            "vendor": "nvidia", "architecture": "ampere", "features": ["shader-f16"], "subgroupMinSize": 32, "subgroupMaxSize": 32}}))
        (self.lib / "android-x.json").write_text(json.dumps({"name": "android-x", "webgpu": {
            "vendor": "arm", "architecture": "valhall", "features": ["shader-f16"], "subgroupMinSize": None, "subgroupMaxSize": None}}))

    def test_the_same_gpu_gains_the_sizes_and_missing_fields_once(self):
        cfg = {"name": "win-rtx3060", "webgpu": {"vendor": "nvidia", "architecture": "ampere"}}
        adopt_library_fields(cfg, self.lib)
        adopt_library_fields(cfg, self.lib)
        self.assertEqual(cfg["webgpu"], {"vendor": "nvidia", "architecture": "ampere", "features": ["shader-f16"],
                                         "subgroupMinSize": 32, "subgroupMaxSize": 32})
        self.assertEqual(cfg["fonts_local"], ["Arial"])

    def test_a_pre_157_capture_and_a_stored_none_give_way_to_the_template(self):
        stale = {"name": "win-rtx3060", "webgpu": {"vendor": "nvidia", "architecture": "ampere", "features": [],
                                                   "subgroupMinSize": 32, "subgroupMaxSize": 128}}
        adopt_library_fields(stale, self.lib)
        self.assertEqual(stale["webgpu"]["subgroupMaxSize"], 32)
        stored = {"name": "win-rtx3060", "webgpu": {"vendor": "nvidia", "architecture": "ampere", "features": [],
                                                    "subgroupMinSize": None, "subgroupMaxSize": None}}
        adopt_library_fields(stored, self.lib)
        self.assertEqual((stored["webgpu"]["subgroupMinSize"], stored["webgpu"]["subgroupMaxSize"]), (32, 32))

    def test_another_gpu_a_null_or_no_template_change_no_sizes(self):
        other = {"name": "win-rtx3060", "webgpu": {"vendor": "amd", "architecture": "rdna-2", "features": []}}
        adopt_library_fields(other, self.lib)
        self.assertNotIn("subgroupMinSize", other["webgpu"])
        nulls = {"name": "android-x", "webgpu": {"vendor": "arm", "architecture": "valhall", "features": []}}
        adopt_library_fields(nulls, self.lib)
        self.assertNotIn("subgroupMinSize", nulls["webgpu"])
        own = {"name": "my capture", "webgpu": {"vendor": "nvidia", "architecture": "ampere"}}
        adopt_library_fields(own, self.lib)
        self.assertEqual(own["webgpu"], {"vendor": "nvidia", "architecture": "ampere"})


if __name__ == "__main__":
    unittest.main()
