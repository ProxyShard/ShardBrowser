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


class AdoptLaptopTest(unittest.TestCase):
    R = "ANGLE (Apple, ANGLE Metal Renderer: Apple M3, Unspecified Version)"

    def setUp(self):
        self.lib = Path(tempfile.mkdtemp(prefix="shardx-lib-"))
        (self.lib / "mac-m3-air15.json").write_text(json.dumps({
            "screen": {"width": 1710, "height": 1112, "avail_width": 1710, "avail_height": 1075},
            "battery": {"laptop": True},
            "webgl": {"renderer": self.R, "extensions_v2": ["EXT_a", "WEBGL_shader_pixel_local_storage"],
                      "parameters": {"0x0D33": 16384, "0x96E0": 8, "0x96E1": 8}}}))
        (self.lib / "mac-m3-imac24.json").write_text(json.dumps({"battery": {"charging": True, "level": 1, "charging_time": 0}}))

    def test_an_air15_takes_its_battery_screen_and_extensions_once(self):
        cfg = {"_meta": {"gpu_preset_id": "mac-m3-air15"},
               "screen": {"width": 1680, "height": 1050, "avail_width": 1680, "avail_height": 1013},
               "window": {"outer_width": 1680, "outer_height": 1012, "inner_width": 1680, "inner_height": 925},
               "battery": {"charging": True, "level": 1, "charging_time": 0, "discharging_time": "Infinity"},
               "webgl": {"renderer": self.R, "extensions_v2": ["EXT_a"], "parameters": {"0x0D33": 8192}}}
        adopt_library_fields(cfg, self.lib)
        adopt_library_fields(cfg, self.lib)
        self.assertEqual(cfg["battery"], {"laptop": True})
        self.assertEqual((cfg["screen"]["width"], cfg["screen"]["avail_height"]), (1710, 1075))
        self.assertEqual((cfg["window"]["outer_height"], cfg["window"]["inner_height"]), (1074, 987))
        self.assertEqual(cfg["webgl"]["extensions_v2"], ["EXT_a", "WEBGL_shader_pixel_local_storage"])
        # New limits come in; a value the profile already has stays.
        self.assertEqual(cfg["webgl"]["parameters"], {"0x0D33": 8192, "0x96E0": 8, "0x96E1": 8})
        host = {"_meta": {"gpu_preset_id": "mac-m3-air15"}, "webgl": {"renderer": self.R, "parameters": {}}}
        adopt_library_fields(host, self.lib)
        self.assertEqual(host["webgl"]["parameters"], {}, "an empty table means the host's values")

    def test_an_own_battery_and_a_desktop_stay(self):
        own = {"_meta": {"gpu_preset_id": "mac-m3-air15"}, "battery": {"charging": False, "level": 0.4}}
        adopt_library_fields(own, self.lib)
        self.assertEqual(own["battery"], {"charging": False, "level": 0.4})
        desk = {"_meta": {"gpu_preset_id": "mac-m3-imac24"},
                "battery": {"charging": True, "level": 1, "charging_time": 0, "discharging_time": "Infinity"}}
        adopt_library_fields(desk, self.lib)
        self.assertEqual(desk["battery"]["level"], 1)

    def test_a_desktop_claim_under_four_cores_is_raised_a_phone_keeps_its_own(self):
        for n in (1, 2, 3):
            desk = {"navigator": {"platform": "Windows", "hardware_concurrency": n}}
            adopt_library_fields(desk, self.lib)
            self.assertEqual(desk["navigator"]["hardware_concurrency"], 4)
        for n in (4, 6, 0):
            desk = {"navigator": {"platform": "Win32", "hardware_concurrency": n}}
            adopt_library_fields(desk, self.lib)
            self.assertEqual(desk["navigator"]["hardware_concurrency"], n)
        phone = {"navigator": {"platform": "Linux armv81", "hardware_concurrency": 2,
                               "user_agent": "Mozilla/5.0 (Linux; Android 10; K)"}, "client_hints": {"mobile": True}}
        adopt_library_fields(phone, self.lib)
        self.assertEqual(phone["navigator"]["hardware_concurrency"], 2)


    def test_a_linux_profile_moves_from_vulkan_to_glx_and_cedar_to_the_rx6800(self):
        import copy
        ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36"
        glx = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)"
        vk = "ANGLE (Intel, Vulkan 1.3.246 (Intel(R) UHD Graphics 620 (KBL GT2) (0x00005917)), Intel open-source Mesa driver)"
        navi = "ANGLE (AMD, AMD Radeon RX 6800 (radeonsi navi21 LLVM 20.1.2), OpenGL 4.6)"
        (self.lib / "linux-uhd620.json").write_text(json.dumps({"notes": glx, "navigator": {"platform_value": "Linux x86_64", "max_touch_points": 0},
            "webgl": {"renderer": glx, "extensions": ["EXT_new"], "parameters": {"0x0D33": 16384}}}))
        (self.lib / "linux-navi21.json").write_text(json.dumps({"notes": navi, "navigator": {"platform_value": "Linux x86_64", "max_touch_points": 0},
            "webgl": {"renderer": navi, "extensions": ["EXT_amd"]}}))

        def old(gid, r, notes):
            return {"name": "Profile 7", "notes": notes, "_meta": {"gpu_preset_id": gid},
                    "navigator": {"user_agent": ua, "platform_value": "Linux armv81", "max_touch_points": 5},
                    "webgl": {"renderer": r, "extensions": ["EXT_old"], "parameters": {"0x0D33": 8192}, "own_key": 1}}

        p = old("linux-uhd620", vk, vk)
        adopt_library_fields(p, self.lib)
        once = copy.deepcopy(p)
        adopt_library_fields(p, self.lib)
        self.assertEqual(p, once)
        self.assertEqual(p["webgl"], {"renderer": glx, "extensions": ["EXT_new"], "parameters": {"0x0D33": 16384}, "own_key": 1})
        self.assertEqual(p["notes"], glx)
        self.assertEqual(p["navigator"]["platform_value"], "Linux armv81")
        kept = old("linux-uhd620", vk, "work account")
        adopt_library_fields(kept, self.lib)
        self.assertEqual(kept["notes"], "work account")

        cedar = old("linux-unknown-amdcedardr", "AMD CEDAR (DRM 2.49.0 / 4.9.194-primeos+, LLVM 7.0.0)", "x")
        adopt_library_fields(cedar, self.lib)
        self.assertEqual(cedar["webgl"]["renderer"], navi)
        self.assertEqual(cedar["_meta"]["gpu_preset_id"], "linux-navi21")
        self.assertEqual((cedar["navigator"]["platform_value"], cedar["navigator"]["max_touch_points"]), ("Linux x86_64", 0))

        # An operator's own GLX capture, a host-own list, and a non-Linux profile stay.
        es = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL ES 3.2)"
        own = old("linux-uhd620", es, "x")
        adopt_library_fields(own, self.lib)
        self.assertEqual(own["webgl"]["renderer"], es)
        host = old("linux-uhd620", vk, "x")
        host["webgl"]["extensions"] = []
        adopt_library_fields(host, self.lib)
        self.assertEqual((host["webgl"]["extensions"], host["webgl"]["renderer"]), ([], glx))
        win = old("linux-uhd620", vk, "x")
        win["navigator"]["user_agent"] = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"
        adopt_library_fields(win, self.lib)
        self.assertEqual(win["webgl"]["renderer"], vk)

if __name__ == "__main__":
    unittest.main()
