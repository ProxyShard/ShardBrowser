// #131: a phone's WebGL in the ANGLE form, its platform_version an Android release, its own screen.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { adoptLibraryFields } from "../dist/profile.js";
import { applyScreenStrategy, defaultScreenModeFor } from "../dist/screen.js";

const PHONE_UA = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Mobile Safari/537.36";
const lib = mkdtempSync(join(tmpdir(), "shardx-phone-"));
writeFileSync(join(lib, "android-s24.json"), JSON.stringify({ client_hints: { model: "SM-S928B", platform_version: "16.0.0" } }));
writeFileSync(join(lib, "android-a35.json"), JSON.stringify({ client_hints: { model: "SM-A356E", platform_version: "16.0.0" } }));
writeFileSync(join(lib, "android-a35-2.json"), JSON.stringify({ client_hints: { model: "SM-A356E", platform_version: "15.0.0" } }));

const phone = (key, model, version, nav) => ({
  _meta: { gpu_preset_id: key },
  navigator: { user_agent: PHONE_UA, ...(nav ? { platform_version: nav } : {}) },
  client_hints: { mobile: true, model, platform_version: version },
});

test("a phone's bare WebGL takes the ANGLE form, once", () => {
  const q = { navigator: { user_agent: PHONE_UA }, webgl: { vendor_masked: "WebKit", vendor: "Qualcomm", renderer: "Adreno (TM) 619" } };
  adoptLibraryFields(q, lib);
  adoptLibraryFields(q, lib);
  assert.deepEqual(q.webgl, { vendor_masked: "WebKit", vendor: "Google Inc. (Qualcomm)", renderer: "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)" });
  const half = { client_hints: { mobile: true }, webgl: { vendor: "Google Inc. (ARM)", renderer: "Mali-G68" } };
  adoptLibraryFields(half, lib);
  assert.equal(half.webgl.renderer, "ANGLE (ARM, Mali-G68, OpenGL ES 3.2)");
  // Another maker, and a desktop, stay.
  const pvr = { navigator: { user_agent: PHONE_UA }, webgl: { vendor: "Imagination Technologies", renderer: "PowerVR Rogue GE8320" } };
  const desk = { navigator: { user_agent: "Mozilla/5.0 (X11; Linux x86_64)" }, webgl: { vendor: "ARM", renderer: "Mali-G57" } };
  for (const cfg of [pvr, desk]) {
    const before = structuredClone(cfg);
    adoptLibraryFields(cfg, lib);
    assert.deepEqual(cfg, before);
  }
});

test("a phone's desktop platform_version becomes its template's, else its model's", () => {
  const mac = phone("android-s24", "SM-S928B", "15.7.1", "15.7.1");
  adoptLibraryFields(mac, lib);
  assert.deepEqual([mac.client_hints.platform_version, mac.navigator.platform_version], ["16.0.0", "16.0.0"]);
  // Its template is gone: the newest release of the same model.
  const gone = phone("android-gone", "SM-A356E", "26.1.0");
  adoptLibraryFields(gone, lib);
  assert.equal(gone.client_hints.platform_version, "16.0.0");
  assert.equal("platform_version" in gone.navigator, false);
});

test("no template and no model: the version stays, never the user agent's Android 10", () => {
  for (const cfg of [phone("android-gone", "SM-X000", "26.1.0"), phone("android-s24", "SM-S928B", "13.0.0", "6.8.0"),
                     phone("android-s24", "SM-S928B", ""),
                     { _meta: { gpu_preset_id: "android-s24" }, navigator: { platform: "macOS" }, client_hints: { platform_version: "15.7.1" } }]) {
    const before = structuredClone(cfg);
    adoptLibraryFields(cfg, lib);
    assert.deepEqual(cfg, before);
  }
});

test("a phone keeps its own screen in every mode", () => {
  assert.equal(defaultScreenModeFor("Linux", true), "profile");
  assert.equal(defaultScreenModeFor("Linux armv81", true), "profile");
  assert.equal(defaultScreenModeFor("Linux"), "use_host");
  const cfg = { navigator: { user_agent: PHONE_UA, platform: "Linux" }, screen: { width: 412, height: 892, avail_width: 412, avail_height: 892 } };
  const before = structuredClone(cfg);
  applyScreenStrategy(cfg, "use_host");
  applyScreenStrategy(cfg, "cap_to_host");
  assert.deepEqual(cfg, before);
});
