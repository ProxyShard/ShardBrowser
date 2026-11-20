// node --test: what makes a profile a phone, and which platform_version a template pick
// writes (#131), the same as the backend decides (`npm run test:unit`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { claimsMobile, isAndroidRelease, pickedPlatformVersion, templatePlatformVersion } from "./phone.ts";

const ROOT = fileURLToPath(new URL("../../../../", import.meta.url));
const PHONE_UA = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Mobile Safari/537.36";
const MAC_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36";

test("a phone is told by the engine's three signals, a form by its user agent", () => {
  assert.equal(claimsMobile({ user_agent: PHONE_UA }), true);
  assert.equal(claimsMobile({ navigator: { user_agent: PHONE_UA } }), true);
  assert.equal(claimsMobile({ client_hints: { mobile: true } }), true);
  assert.equal(claimsMobile({ client_hints: { platform: "Android" } }), true);
  assert.equal(claimsMobile({ user_agent: MAC_UA }), false);
  assert.equal(claimsMobile({ user_agent: MAC_UA, client_hints: { mobile: false, platform: "macOS" } }), false);
});

test("an Android release is <major>.0.0 from Android 10, as in platform_version.rs", () => {
  for (const v of ["10.0.0", "14.0.0", "16.0.0"]) assert.equal(isAndroidRelease(v), true, v);
  for (const v of ["15.7.1", "26.1.0", "6.8.0", "9.0.0", "19.0.0", "16", "16.0", ""]) assert.equal(isAndroidRelease(v), false, v);
  const rust = readFileSync(join(ROOT, "src-tauri/src/platform_version.rs"), "utf8");
  const range = /const ANDROID_MAJORS[^=]*=\s*(\d+)\.\.=(\d+);/.exec(rust);
  assert.ok(range, "platform_version.rs has ANDROID_MAJORS");
  assert.equal(isAndroidRelease(`${range[1]}.0.0`), true);
  assert.equal(isAndroidRelease(`${Number(range[1]) - 1}.0.0`), false);
  assert.equal(isAndroidRelease(`${range[2]}.0.0`), true);
  assert.equal(isAndroidRelease(`${Number(range[2]) + 1}.0.0`), false);
});

test("a pick falls back to the template's version, never the previous OS's", () => {
  const phone = { navigator: { user_agent: PHONE_UA }, client_hints: { platform_version: "16.0.0" } };
  // The backend could not draw one: the template's own, not the Mac's the form held.
  assert.equal(pickedPlatformVersion(undefined, phone), "16.0.0");
  assert.equal(pickedPlatformVersion(null, phone), "16.0.0");
  assert.equal(pickedPlatformVersion("13.0.0", phone), "13.0.0");
  // The engine reads client_hints first; navigator is the older place.
  assert.equal(templatePlatformVersion({ navigator: { platform_version: "6.8.0" } }), "6.8.0");
  assert.equal(templatePlatformVersion({ navigator: { platform_version: "1" }, client_hints: { platform_version: "15.0.0" } }), "15.0.0");
  // None anywhere: empty, which toStored reads as "the template's own".
  assert.equal(pickedPlatformVersion(undefined, { navigator: {} }), "");
  assert.equal(pickedPlatformVersion(undefined, undefined), "");
});
