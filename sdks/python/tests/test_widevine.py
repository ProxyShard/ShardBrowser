"""The Widevine CDM from Google's update server, checked as Chrome checks it and put into each profile.

Run: python3 -m unittest discover -s sdks/python/tests. The live test: SHARDX_LIVE_WIDEVINE=1 (needs httpx)."""
import hashlib
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import unittest
import warnings
from pathlib import Path

# widevine.py on its own: it needs httpx only to reach the network.
WIDEVINE_PY = Path(__file__).resolve().parents[1] / "shardx" / "widevine.py"
_spec = importlib.util.spec_from_file_location("shardx_widevine_under_test", WIDEVINE_PY)
wv = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = wv  # its dataclasses look themselves up there
_spec.loader.exec_module(wv)

FIXTURES = {k: v for k, v in json.loads((Path(__file__).parent / "fixtures" / "crx3.json").read_text()).items()}
signed_all = bytes.fromhex(FIXTURES["signed_rsa_p256_p384"])
signed_p256 = bytes.fromhex(FIXTURES["signed_p256"])
no_dev = bytes.fromhex(FIXTURES["no_developer_proof"])


def fake_cdm(d: Path, version: str) -> None:
    (d / "_platform_specific" / "mac_arm64").mkdir(parents=True, exist_ok=True)
    (d / "manifest.json").write_text(json.dumps({"name": "WidevineCdm", "version": version}))
    (d / "_platform_specific" / "mac_arm64" / "libwidevinecdm.dylib").write_text("cdm")


class RequestTest(unittest.TestCase):
    def test_the_request_names_this_platform_its_release_and_the_engine(self):
        r = wv.request_body("158.0.8127.36", None, False)["request"]
        self.assertEqual(r["apps"][0]["appid"], wv.WIDEVINE_ID)
        self.assertEqual(r["apps"][0]["version"], "0.0.0.0")
        self.assertEqual(r["apps"][0]["installsource"], "ondemand")
        self.assertEqual((r["protocol"], r["prodversion"], r["updaterversion"]), ("4.0", "158.0.8127.36", "158.0.8127.36"))
        self.assertIn(r["@os"], ("mac", "win", "linux"))
        self.assertRegex(r["os"]["version"], r"^\d+(\.\d+)+$")
        self.assertRegex(r["requestid"], r"^\{[0-9a-f-]{36}\}$")
        bg = wv.request_body("158", "4.10.3112.0", True)["request"]["apps"][0]
        self.assertEqual((bg["version"], bg["installsource"]), ("4.10.3112.0", ""))
        self.assertEqual(wv.updater_platform("darwin", "arm64"), ("mac", "Mac OS X", "arm64", "arm64"))
        self.assertEqual(wv.updater_platform("linux", "x86_64"), ("linux", "Linux", "x64", "x86_64"))
        self.assertEqual(wv.updater_platform("win32", "AMD64"), ("win", "Windows", "x64", "x86_64"))

    def test_an_offer_is_read_noupdate_is_none_an_error_raises(self):
        ok = (')]}\'\n{"response":{"protocol":"4.0","apps":[{"appid":"oimompecagnajdejgnnjijobebaeigek","status":"ok",'
              '"updatecheck":{"nextversion":"4.10.3112.0","status":"ok","pipelines":[{"pipeline_id":"1:full","operations":'
              '[{"size":20423379,"type":"download","out":{"sha256":"6bf3"},"urls":[{"url":"http://edgedl.me.gvt1.com/a.crx3"},'
              '{"url":"https://edgedl.me.gvt1.com/a.crx3"}]},{"type":"crx3"}]}]}}]}}')
        self.assertEqual(wv.parse_offer(ok), wv.Offer("4.10.3112.0", ("https://edgedl.me.gvt1.com/a.crx3",), "6bf3", 20423379))
        none = ')]}\'\n{"response":{"apps":[{"appid":"x","status":"ok","updatecheck":{"status":"noupdate"}}]}}'
        self.assertIsNone(wv.parse_offer(none))
        with self.assertRaisesRegex(wv.WidevineError, "error-unknownApplication"):
            wv.parse_offer('{"response":{"apps":[{"status":"error-unknownApplication"}]}}')
        with self.assertRaisesRegex(wv.WidevineError, "not JSON"):
            wv.parse_offer("<html>")


class CrxTest(unittest.TestCase):
    def test_the_id_is_the_keys_hash_as_a_to_p(self):
        self.assertEqual(wv.id_from_hash(bytes.fromhex("0123456789abcdeffedcba9876543210")), "abcdefghijklmnopponmlkjihgfedcba")

    def test_rsa_and_ecdsa_proofs_made_by_openssl_verify(self):
        v = wv.verify_crx(signed_all)
        self.assertEqual(v.crx_id, FIXTURES["signed_rsa_p256_p384_id"])
        self.assertEqual(wv.id_from_key(v.developer_key), v.crx_id)
        self.assertEqual(signed_all[v.archive_offset:v.archive_offset + 2], b"PK")
        self.assertEqual(wv.verify_crx(signed_p256).crx_id, FIXTURES["signed_p256_id"])

    def test_a_package_signed_for_another_id_is_refused(self):
        with self.assertRaises(wv.CrxReject) as e:
            wv.verify_crx(signed_all, id=wv.WIDEVINE_ID)
        self.assertEqual(e.exception.code, "WrongId")

    def test_a_changed_byte_a_wrong_hash_or_size_or_no_developer_proof_is_refused(self):
        def code(data, **kw):
            with self.assertRaises(wv.CrxReject) as e:
                wv.verify_crx(data, **kw)
            return e.exception.code

        for pkg in (signed_all, signed_p256):  # RSA first, and ECDSA alone
            bent = bytearray(pkg)
            bent[-3] ^= 1
            self.assertEqual(code(bytes(bent)), "BadSignature")
        sha = hashlib.sha256(signed_all).hexdigest()
        self.assertEqual(wv.verify_crx(signed_all, sha256=sha.upper(), size=len(signed_all)).sha256, sha)
        self.assertEqual(code(signed_all, sha256="00" * 32), "WrongHash")
        self.assertEqual(code(signed_all, size=len(signed_all) + 1), "WrongSize")
        self.assertEqual(code(no_dev), "NoDeveloperProof")
        self.assertEqual(code(b"PK\x03\x04 not a crx"), "NotCrx3")

    def test_a_verified_package_is_unpacked_into_the_cache_for_its_version_only(self):
        root = Path(tempfile.mkdtemp(prefix="shardx-wv-pkg-"))
        try:
            cache = root / "widevine-cdm"
            v = wv.verify_crx(signed_all)
            with self.assertRaisesRegex(wv.WidevineError, "another version"):
                wv.install_package(signed_all, v.archive_offset, "4.10.9999.0", cache)
            self.assertFalse(cache.exists())
            wv.install_package(signed_all, v.archive_offset, "4.10.3112.0", cache)
            self.assertEqual(wv.cdm_version_of(cache), "4.10.3112.0")
            self.assertTrue((cache / "_platform_specific" / "linux_x64" / "libwidevinecdm.so").is_file())
            self.assertEqual(sorted(p.name for p in root.iterdir()), ["widevine-cdm"])
        finally:
            shutil.rmtree(root, ignore_errors=True)


class ProfileTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="shardx-wv-"))
        self.cache = self.root / "widevine-cdm"

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def test_a_profile_gets_the_version_folder_and_the_hint_once(self):
        udd = self.root / "profiles" / "p1"
        udd.mkdir(parents=True)
        self.assertIsNone(wv.install_widevine(self.cache, udd), "nothing cached yet")
        fake_cdm(self.cache, "4.10.3112.0")
        # A flat layout an older build left.
        (udd / "WidevineCdm" / "_platform_specific").mkdir(parents=True)
        (udd / "WidevineCdm" / "manifest.json").write_text('{"version":"4.10.1.0"}')

        versioned = wv.install_widevine(self.cache, udd)
        self.assertEqual(versioned, udd / "WidevineCdm" / "4.10.3112.0")
        self.assertEqual(wv.cdm_version_of(versioned), "4.10.3112.0")
        self.assertTrue((versioned / "_platform_specific" / "mac_arm64" / "libwidevinecdm.dylib").is_file())
        self.assertFalse((udd / "WidevineCdm" / "manifest.json").exists(), "the flat layout is gone")
        hint = udd / "WidevineCdm" / "latest-component-updated-widevine-cdm"
        self.assertEqual(json.loads(hint.read_text()), {"Path": os.path.abspath(versioned)})
        self.assertEqual(sorted(p.name for p in (udd / "WidevineCdm").iterdir()),
                         ["4.10.3112.0", "latest-component-updated-widevine-cdm"])
        self.assertEqual(wv.install_widevine(self.cache, udd), versioned)

    def test_a_newer_cdm_the_engine_fetched_is_taken_an_older_one_is_not(self):
        udd = self.root / "p"
        fake_cdm(self.cache, "4.10.3112.0")
        fake_cdm(udd / "WidevineCdm" / "4.10.2891.0", "4.10.2891.0")
        self.assertIsNone(wv.harvest_widevine(self.cache, udd))
        fake_cdm(udd / "WidevineCdm" / "4.11.0.0", "4.11.0.0")
        self.assertEqual(wv.harvest_widevine(self.cache, udd), "4.11.0.0")
        self.assertEqual(wv.cdm_version_of(self.cache), "4.11.0.0")
        self.assertFalse(any("incoming" in p.name for p in self.root.iterdir()))

    def test_a_first_cdm_is_waited_for_a_fresh_one_is_not_checked_and_starts_at_once_check_once(self):
        calls = []

        def fetcher(proxy, chrome, have, cache, background, cancel):
            calls.append((have, background))
            time.sleep(0.02)
            fake_cdm(cache, "4.10.3112.0")
            return "4.10.3112.0"

        ts = [threading.Thread(target=wv.ensure_widevine, args=(self.cache, "158.0.8127.36", None),
                               kwargs={"fetcher": fetcher}) for _ in range(2)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertEqual(calls, [(None, False)])
        self.assertTrue((self.root / "widevine-cdm.checked").exists())
        wv.ensure_widevine(self.cache, "158.0.8127.36", None, fetcher=fetcher)
        self.assertEqual(len(calls), 1, "checked within 5 hours")
        # An old stamp: checked again, in the background.
        old = time.time() - 6 * 3600
        os.utime(self.root / "widevine-cdm.checked", (old, old))
        done = threading.Event()

        def again(proxy, chrome, have, cache, background, cancel):
            calls.append((have, background))
            done.set()
            return None

        wv.ensure_widevine(self.cache, "158.0.8127.36", None, fetcher=again)
        self.assertTrue(done.wait(5))
        self.assertEqual(calls[1], ("4.10.3112.0", True))

    def test_a_first_check_that_takes_too_long_is_given_up(self):
        seen = threading.Event()

        def slow(proxy, chrome, have, cache, background, cancel):
            cancel.wait(5)
            seen.set()
            raise RuntimeError("cancelled")

        with warnings.catch_warnings(record=True) as w:
            warnings.simplefilter("always")
            wv.ensure_widevine(self.cache, "158.0.8127.36", None, fetcher=slow, first_wait=0.05)
        self.assertTrue(seen.wait(5), "the check was told to stop")
        self.assertTrue(any("no Widevine CDM yet" in str(x.message) for x in w))
        self.assertFalse((self.root / "widevine-cdm.checked").exists())


@unittest.skipUnless(os.environ.get("SHARDX_LIVE_WIDEVINE"), "reaches Google's servers; set SHARDX_LIVE_WIDEVINE=1")
class LiveTest(unittest.TestCase):
    def test_live_googles_current_cdm_signed_and_in_place(self):
        root = Path(tempfile.mkdtemp(prefix="shardx-wv-live-"))
        try:
            cache = root / "widevine-cdm"
            v = wv.fetch_widevine(None, "158.0.8127.36", None, cache, False)
            self.assertTrue(v, "an offer for a profile with none")
            self.assertEqual(wv.cdm_version_of(cache), v)
            libs = sorted(str(p.relative_to(cache)) for p in (cache / "_platform_specific").rglob("*") if p.is_file())
            print(f"widevine {v}: {', '.join(libs)}", flush=True)
            self.assertTrue(any("widevinecdm" in p for p in libs))
            self.assertIsNone(wv.fetch_widevine(None, "158.0.8127.36", v, cache, True), "the same version again is current")
        finally:
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
