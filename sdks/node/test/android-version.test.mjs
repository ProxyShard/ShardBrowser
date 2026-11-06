// Phone profiles report the Chrome for Android build of the engine's major.
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyEngineVersion } from "../dist/profile.js";

const phone = () => ({ navigator: { user_agent: "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36" },
  client_hints: { mobile: true, brand_full_version: "156.0.8078.48" } });

test("a phone gets the Android build, with the build and patch from it", () => {
  const cfg = phone();
  applyEngineVersion(cfg, "157.0.8103.39", undefined, undefined, undefined, "157.0.8103.38");
  assert.equal(cfg.client_hints.brand_version, "157");
  assert.equal(cfg.client_hints.brand_full_version, "157.0.8103.38");
  assert.equal(cfg.client_hints.chrome_patch, 38);
  assert.match(cfg.navigator.user_agent, /Chrome\/157\.0\.0\.0 Mobile/);
});

test("no Android build, or one of another major, keeps the engine's", () => {
  for (const android of [undefined, "156.0.8078.81", "157.0.8103", ""]) {
    const cfg = phone();
    applyEngineVersion(cfg, "157.0.8103.39", undefined, undefined, undefined, android);
    assert.equal(cfg.client_hints.brand_full_version, "157.0.8103.39", String(android));
  }
});
