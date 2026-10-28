"""A phone offers the handset's voices, not the machine the launcher runs on.

The helper lives in browser.py, whose driver imports are not installed in this
checkout, so the function is lifted out of the file by name and run on its own —
the same way test_runtime_swap.py loads runtime.py in isolation.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import ast
import unittest
from pathlib import Path

BROWSER_PY = Path(__file__).resolve().parents[1] / "shardx" / "browser.py"


def _load(names):
    """Compile just these module-level names out of browser.py."""
    tree = ast.parse(BROWSER_PY.read_text(encoding="utf-8"))
    keep = []
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name in names:
            keep.append(node)
        elif isinstance(node, ast.Assign):
            targets = [t.id for t in node.targets if isinstance(t, ast.Name)]
            if any(t in names for t in targets):
                keep.append(node)
    mod = ast.Module(body=keep, type_ignores=[])
    scope = {}
    exec(compile(mod, str(BROWSER_PY), "exec"), scope)  # noqa: S102 — our own source
    return scope


scope = _load({"apply_phone_voices", "_ANDROID_SPEECH_VOICES"})
apply_phone_voices = scope["apply_phone_voices"]
ROSTER = scope["_ANDROID_SPEECH_VOICES"]


class PhoneVoicesTest(unittest.TestCase):
    def test_a_phone_without_voices_gets_the_handsets(self):
        cfg = {
            "navigator": {
                "user_agent": "Mozilla/5.0 (Linux; Android 14; SM-S921B) Mobile Safari/537.36",
            }
        }
        apply_phone_voices(cfg)
        voices = cfg["speech"]["voices"]
        self.assertEqual(len(voices), len(ROSTER))
        # Chrome's rule: the first voice in the list is the default one.
        self.assertTrue(voices[0]["is_default"])
        self.assertEqual(voices[0]["name"], "Google US English")
        self.assertFalse(any(v["is_default"] for v in voices[1:]))
        # Android's voices come from Google TTS, so none of them is on the device.
        self.assertFalse(any(v["local_service"] for v in voices))
        self.assertTrue(all(v["name"] and v["lang"] for v in voices))

    def test_a_desktop_profile_is_left_alone(self):
        cfg = {
            "navigator": {
                "user_agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/537.36",
            },
            "speech": {"voices": [{"name": "Samantha", "lang": "en-US",
                                   "local_service": True, "is_default": True}]},
        }
        apply_phone_voices(cfg)
        self.assertEqual([v["name"] for v in cfg["speech"]["voices"]], ["Samantha"])

    def test_a_phone_that_declares_its_own_voices_keeps_them(self):
        cfg = {
            "client_hints": {"mobile": True},
            "speech": {"voices": [{"name": "English (United States)", "lang": "en-US",
                                   "local_service": True, "is_default": True}]},
        }
        apply_phone_voices(cfg)
        self.assertEqual([v["name"] for v in cfg["speech"]["voices"]],
                         ["English (United States)"])

    def test_an_empty_voice_list_is_not_a_claim(self):
        # The engine reads an empty list as none and enumerates the host's
        # voices anyway, so a phone carrying one still needs the roster.
        cfg = {"client_hints": {"mobile": True}, "speech": {"voices": []}}
        apply_phone_voices(cfg)
        self.assertEqual(len(cfg["speech"]["voices"]), len(ROSTER))


if __name__ == "__main__":
    unittest.main()
