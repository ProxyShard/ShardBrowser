"""#130: a CDP session is stopped with Browser.close, the way the launcher closes a profile.

Run: python3 -m unittest discover -s sdks/python/tests (or pytest)."""
import importlib.util
import json
import socket
import sys
import threading
import types
import unittest
from pathlib import Path

# browser.py on its own, with its package imports stood in for: no browser driver needed.
PKG = Path(__file__).resolve().parents[1] / "shardx"
pkg = types.ModuleType("shardx_close_test")
pkg.__path__ = [str(PKG)]
sys.modules["shardx_close_test"] = pkg
for name in ("auto_resolve", "geo", "profile", "proxy", "runtime", "screen"):
    stub = types.ModuleType(f"shardx_close_test.{name}")
    stub.__getattr__ = lambda attr: object  # every name the module imports exists
    sys.modules[f"shardx_close_test.{name}"] = stub
_spec = importlib.util.spec_from_file_location("shardx_close_test.browser", PKG / "browser.py")
browser = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = browser
_spec.loader.exec_module(browser)


class CloseOverCdpTest(unittest.TestCase):
    def test_browser_close_goes_out_as_one_masked_frame_after_the_upgrade(self):
        srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(1)
        got = {}

        def serve():
            c, _ = srv.accept()
            hello = c.recv(1024)
            got["hello"] = hello
            c.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            f = c.recv(1024)
            n, mask = f[1] & 0x7F, f[2:6]
            got["payload"] = bytes(b ^ mask[i % 4] for i, b in enumerate(f[6:6 + n]))
            c.close()

        t = threading.Thread(target=serve); t.start()
        ok = browser._close_over_cdp(f"ws://127.0.0.1:{srv.getsockname()[1]}/devtools/browser/x")
        t.join(3); srv.close()
        self.assertTrue(ok)
        self.assertTrue(got["hello"].startswith(b"GET /devtools/browser/x HTTP/1.1\r\n"))
        self.assertEqual(json.loads(got["payload"]), {"id": 1, "method": "Browser.close"})

    def test_nothing_listening_is_a_no_not_a_hang(self):
        self.assertFalse(browser._close_over_cdp("ws://127.0.0.1:1/devtools/browser/x", timeout=1.0))


if __name__ == "__main__":
    unittest.main()
