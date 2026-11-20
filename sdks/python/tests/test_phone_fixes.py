"""#131: a phone's WebGL in the ANGLE form, its platform_version an Android release, its own screen.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import copy
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

# runtime.py and screen.py as one package; the host's monitor stood in for.
PKG = Path(__file__).resolve().parents[1] / "shardx"
pkg = types.ModuleType("shardx_phone_test")
pkg.__path__ = [str(PKG)]
sys.modules["shardx_phone_test"] = pkg
host = types.ModuleType("shardx_phone_test.host")
host.host_screen_size = lambda: (1440, 900)
sys.modules["shardx_phone_test.host"] = host


def _load(name):
    spec = importlib.util.spec_from_file_location(f"shardx_phone_test.{name}", PKG / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


rt = _load("runtime")
screen = _load("screen")

PHONE_UA = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Mobile Safari/537.36"


def phone(key, model, version, nav=None):
    cfg = {"_meta": {"gpu_preset_id": key}, "navigator": {"user_agent": PHONE_UA},
           "client_hints": {"mobile": True, "model": model, "platform_version": version}}
    if nav is not None:
        cfg["navigator"]["platform_version"] = nav
    return cfg


class PhoneFixesTest(unittest.TestCase):
    def setUp(self):
        self.lib = Path(tempfile.mkdtemp(prefix="shardx-phone-"))
        for name, model, v in (("android-s24", "SM-S928B", "16.0.0"), ("android-a35", "SM-A356E", "16.0.0"),
                               ("android-a35-2", "SM-A356E", "15.0.0")):
            (self.lib / f"{name}.json").write_text(json.dumps({"client_hints": {"model": model, "platform_version": v}}))

    def test_a_phones_bare_webgl_takes_the_angle_form_once(self):
        q = {"navigator": {"user_agent": PHONE_UA}, "webgl": {"vendor_masked": "WebKit", "vendor": "Qualcomm", "renderer": "Adreno (TM) 619"}}
        rt.adopt_library_fields(q, self.lib)
        rt.adopt_library_fields(q, self.lib)
        self.assertEqual(q["webgl"], {"vendor_masked": "WebKit", "vendor": "Google Inc. (Qualcomm)",
                                      "renderer": "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)"})
        half = {"client_hints": {"mobile": True}, "webgl": {"vendor": "Google Inc. (ARM)", "renderer": "Mali-G68"}}
        rt.adopt_library_fields(half, self.lib)
        self.assertEqual(half["webgl"]["renderer"], "ANGLE (ARM, Mali-G68, OpenGL ES 3.2)")
        # Another maker, and a desktop, stay.
        for cfg in ({"navigator": {"user_agent": PHONE_UA}, "webgl": {"vendor": "Imagination Technologies", "renderer": "PowerVR Rogue GE8320"}},
                    {"navigator": {"user_agent": "Mozilla/5.0 (X11; Linux x86_64)"}, "webgl": {"vendor": "ARM", "renderer": "Mali-G57"}}):
            before = copy.deepcopy(cfg)
            rt.adopt_library_fields(cfg, self.lib)
            self.assertEqual(cfg, before)

    def test_a_desktop_version_becomes_the_templates_else_the_models(self):
        mac = phone("android-s24", "SM-S928B", "15.7.1", "15.7.1")
        rt.adopt_library_fields(mac, self.lib)
        self.assertEqual((mac["client_hints"]["platform_version"], mac["navigator"]["platform_version"]), ("16.0.0", "16.0.0"))
        # Its template is gone: the newest release of the same model.
        gone = phone("android-gone", "SM-A356E", "26.1.0")
        rt.adopt_library_fields(gone, self.lib)
        self.assertEqual(gone["client_hints"]["platform_version"], "16.0.0")
        self.assertNotIn("platform_version", gone["navigator"])

    def test_no_template_and_no_model_leave_it_never_the_user_agents(self):
        for cfg in (phone("android-gone", "SM-X000", "26.1.0"), phone("android-s24", "SM-S928B", "13.0.0", "6.8.0"),
                    phone("android-s24", "SM-S928B", ""),
                    {"_meta": {"gpu_preset_id": "android-s24"}, "navigator": {"platform": "macOS"},
                     "client_hints": {"platform_version": "15.7.1"}}):
            before = copy.deepcopy(cfg)
            rt.adopt_library_fields(cfg, self.lib)
            self.assertEqual(cfg, before)

    def test_a_phone_keeps_its_own_screen_in_every_mode(self):
        self.assertEqual(screen.default_mode_for("Linux", True), "profile")
        self.assertEqual(screen.default_mode_for("Linux armv81", True), "profile")
        self.assertEqual(screen.default_mode_for("Linux"), "use_host")
        cfg = {"navigator": {"user_agent": PHONE_UA, "platform": "Linux"},
               "screen": {"width": 412, "height": 892, "avail_width": 412, "avail_height": 892}}
        before = copy.deepcopy(cfg)
        screen.apply_screen_strategy(cfg, "use_host")
        screen.apply_screen_strategy(cfg, "cap_to_host")
        self.assertEqual(cfg, before)
        # A desktop still takes the host's monitor.
        desk = {"navigator": {"platform": "Linux"}, "screen": {"width": 1920, "height": 1080}}
        screen.apply_screen_strategy(desk, "use_host")
        self.assertEqual((desk["screen"]["width"], desk["screen"]["height"]), (1440, 900))


if __name__ == "__main__":
    unittest.main()
