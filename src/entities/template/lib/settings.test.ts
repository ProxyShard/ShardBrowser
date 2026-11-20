// node --test: what a profile template (#145) keeps of a profile, the same as the
// backend keeps (templates.rs `settings_of`, `os_of`), and never its fingerprint (`npm run test:unit`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { osOfPlatform, templateSettingsOf } from "./settings.ts";

const ROOT = fileURLToPath(new URL("../../../../", import.meta.url));
const rust = readFileSync(join(ROOT, "src-tauri/src/space/local/templates.rs"), "utf8");

/** The quoted words of the Rust constant `name`, in order. */
function words(name: string): string[] {
  const at = rust.indexOf(`const ${name}`);
  assert.ok(at >= 0, `templates.rs has ${name}`);
  const body = rust.slice(at, rust.indexOf("];", at));
  return [...body.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
}

const profile = {
  _meta: { id: "p1", rev: 4, proxy_id: "x1", gpu_preset_id: "win-rtx", color: "#ff0000", extensions: ["e1"], folder: "Ads", screen_custom: true },
  name: "Shop 1", notes: "secret notes", timezone: "auto", icu_locale: null, webrtc: "tcp_only",
  navigator: { platform: "Win32", user_agent: "UA", language: "de-DE", accept_language: "de-DE,de;q=0.9", languages: ["de-DE", "de"], hardware_concurrency: 12, device_memory: 16, do_not_track: "1" },
  screen: { width: 1920, height: 1080, refresh_rate: 144 },
  window: { outer_width: 1920, keep_focus: true },
  webgl: { renderer: "RTX" },
  noise: { canvas: { enabled: true, seed: 77 }, webgl: { enabled: true, seed: 9, intensity: 0.0005 } },
  blocked_ports: [3389], geolocation: { mode: "auto" }, media_devices: { audio_input_count: 2 },
};

test("the keys are the backend's, block by block", () => {
  const kept = templateSettingsOf(profile);
  const nested = words("NESTED");
  const top = words("TOP");
  assert.deepEqual(Object.keys(kept).filter((k) => !nested.includes(k)).sort(), top.filter((k) => k in profile).sort());
  for (const block of ["navigator", "screen", "window", "_meta"]) {
    const keys = Object.keys(kept[block]).sort();
    // Each key the TS keeps of a block is one the Rust names for it.
    for (const k of keys) assert.ok(nested.includes(k), `${block}.${k} is not in templates.rs NESTED`);
  }
  assert.deepEqual(kept._meta, { proxy_id: "x1", color: "#ff0000", extensions: ["e1"] });
  assert.deepEqual(kept.navigator, { language: "de-DE", accept_language: "de-DE,de;q=0.9", languages: ["de-DE", "de"], do_not_track: "1" });
  assert.deepEqual(kept.screen, { refresh_rate: 144 });
  assert.deepEqual(kept.window, { keep_focus: true });
});

test("nothing of the identity or the fingerprint is kept, and each noise seed is left to the profile", () => {
  const kept = templateSettingsOf(profile);
  for (const k of ["name", "notes", "webgl", "cookies"]) assert.ok(!(k in kept), k);
  assert.ok(!("gpu_preset_id" in kept._meta) && !("id" in kept._meta) && !("folder" in kept._meta));
  assert.deepEqual(kept.noise, { canvas: { enabled: true, seed: 0 }, webgl: { enabled: true, seed: 0, intensity: 0.0005 } });
  // The profile itself is not changed.
  assert.equal(profile.noise.canvas.seed, 77);
  assert.deepEqual(templateSettingsOf(kept), kept, "cut again, it stays");
  assert.deepEqual(templateSettingsOf(null), {});
});

test("a platform reads as the OS control names it", () => {
  assert.equal(osOfPlatform("Win32"), "Windows");
  assert.equal(osOfPlatform("MacIntel"), "macOS");
  assert.equal(osOfPlatform("Linux x86_64"), "Linux");
  assert.equal(osOfPlatform("Linux armv8l"), "Android");
  assert.equal(osOfPlatform("Plan 9"), "");
});
