// A saved profile picks up what its library template gained after it was frozen.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { adoptLibraryFields } from "../dist/profile.js";

const lib = mkdtempSync(join(tmpdir(), "shardx-lib-"));
writeFileSync(join(lib, "win-rtx3060.json"), JSON.stringify({ name: "win-rtx3060", fonts_local: ["Arial"],
  webgpu: { vendor: "nvidia", architecture: "ampere", features: ["shader-f16"], subgroupMinSize: 32, subgroupMaxSize: 32 } }));
writeFileSync(join(lib, "android-x.json"), JSON.stringify({ name: "android-x",
  webgpu: { vendor: "arm", architecture: "valhall", features: ["shader-f16"], subgroupMinSize: null, subgroupMaxSize: null } }));

test("the same GPU gains the subgroup sizes and missing fields, once", () => {
  const cfg = { name: "win-rtx3060", webgpu: { vendor: "nvidia", architecture: "ampere" } };
  adoptLibraryFields(cfg, lib);
  adoptLibraryFields(cfg, lib);
  assert.deepEqual(cfg.webgpu, { vendor: "nvidia", architecture: "ampere", features: ["shader-f16"], subgroupMinSize: 32, subgroupMaxSize: 32 });
  assert.deepEqual(cfg.fonts_local, ["Arial"]);
});

test("a pre-157 capture and a stored null give way to the template", () => {
  const stale = { name: "win-rtx3060", webgpu: { vendor: "nvidia", architecture: "ampere", features: [], subgroupMinSize: 32, subgroupMaxSize: 128 } };
  adoptLibraryFields(stale, lib);
  assert.equal(stale.webgpu.subgroupMaxSize, 32);
  const stored = { name: "win-rtx3060", webgpu: { vendor: "nvidia", architecture: "ampere", features: [], subgroupMinSize: null, subgroupMaxSize: null } };
  adoptLibraryFields(stored, lib);
  assert.deepEqual([stored.webgpu.subgroupMinSize, stored.webgpu.subgroupMaxSize], [32, 32]);
});

test("another GPU, a null in the template, or no template change no sizes", () => {
  const other = { name: "win-rtx3060", webgpu: { vendor: "amd", architecture: "rdna-2", features: [] } };
  adoptLibraryFields(other, lib);
  assert.equal(other.webgpu.subgroupMinSize, undefined);
  const nulls = { name: "android-x", webgpu: { vendor: "arm", architecture: "valhall", features: [] } };
  adoptLibraryFields(nulls, lib);
  assert.equal("subgroupMinSize" in nulls.webgpu, false);
  const own = { name: "my capture", webgpu: { vendor: "nvidia", architecture: "ampere" } };
  adoptLibraryFields(own, lib);
  assert.deepEqual(own.webgpu, { vendor: "nvidia", architecture: "ampere" });
});

test("an Air 15 profile takes its laptop battery, screen and new WebGL extensions, once", () => {
  const r = "ANGLE (Apple, ANGLE Metal Renderer: Apple M3, Unspecified Version)";
  writeFileSync(join(lib, "mac-m3-air15.json"), JSON.stringify({
    screen: { width: 1710, height: 1112, avail_width: 1710, avail_height: 1075 },
    battery: { laptop: true },
    webgl: { renderer: r, extensions_v2: ["EXT_a", "WEBGL_shader_pixel_local_storage"],
             parameters: { "0x0D33": 16384, "0x96E0": 8, "0x96E1": 8 } } }));
  const cfg = { _meta: { gpu_preset_id: "mac-m3-air15" },
    screen: { width: 1680, height: 1050, avail_width: 1680, avail_height: 1013 },
    window: { outer_width: 1680, outer_height: 1012, inner_width: 1680, inner_height: 925 },
    battery: { charging: true, level: 1, charging_time: 0, discharging_time: "Infinity" },
    webgl: { renderer: r, extensions_v2: ["EXT_a"], parameters: { "0x0D33": 8192 } } };
  adoptLibraryFields(cfg, lib);
  adoptLibraryFields(cfg, lib);
  assert.deepEqual(cfg.battery, { laptop: true });
  assert.deepEqual([cfg.screen.width, cfg.screen.avail_height], [1710, 1075]);
  assert.deepEqual([cfg.window.outer_height, cfg.window.inner_height], [1074, 987]);
  assert.deepEqual(cfg.webgl.extensions_v2, ["EXT_a", "WEBGL_shader_pixel_local_storage"]);
  // New limits come in; a value the profile already has stays.
  assert.deepEqual(cfg.webgl.parameters, { "0x0D33": 8192, "0x96E0": 8, "0x96E1": 8 });
  const host = { _meta: { gpu_preset_id: "mac-m3-air15" }, webgl: { renderer: r, parameters: {} } };
  adoptLibraryFields(host, lib);
  assert.deepEqual(host.webgl.parameters, {}, "an empty table means the host's values");
  const own = { _meta: { gpu_preset_id: "mac-m3-air15" }, battery: { charging: false, level: 0.4 } };
  adoptLibraryFields(own, lib);
  assert.deepEqual(own.battery, { charging: false, level: 0.4 });
});

test("a desktop claim under four cores is raised to four; a phone keeps its own", () => {
  for (const n of [1, 2, 3]) {
    const desk = { navigator: { platform: "Windows", hardware_concurrency: n } };
    adoptLibraryFields(desk, lib);
    assert.equal(desk.navigator.hardware_concurrency, 4);
  }
  for (const n of [4, 6, 0]) {
    const desk = { navigator: { platform: "Win32", hardware_concurrency: n } };
    adoptLibraryFields(desk, lib);
    assert.equal(desk.navigator.hardware_concurrency, n);
  }
  const phone = { navigator: { platform: "Linux armv81", hardware_concurrency: 2, user_agent: "Mozilla/5.0 (Linux; Android 10; K)" }, client_hints: { mobile: true } };
  adoptLibraryFields(phone, lib);
  assert.equal(phone.navigator.hardware_concurrency, 2);
});

test("a Linux profile moves from Vulkan to GLX, and Cedar to the RX 6800", () => {
  const ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36";
  const glx = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)";
  const vk = "ANGLE (Intel, Vulkan 1.3.246 (Intel(R) UHD Graphics 620 (KBL GT2) (0x00005917)), Intel open-source Mesa driver)";
  const navi = "ANGLE (AMD, AMD Radeon RX 6800 (radeonsi navi21 LLVM 20.1.2), OpenGL 4.6)";
  const d = mkdtempSync(join(tmpdir(), "shardx-lib-gl-"));
  writeFileSync(join(d, "linux-uhd620.json"), JSON.stringify({ notes: glx, navigator: { platform_value: "Linux x86_64", max_touch_points: 0 },
    webgl: { renderer: glx, extensions: ["EXT_new"], parameters: { "0x0D33": 16384 } } }));
  writeFileSync(join(d, "linux-navi21.json"), JSON.stringify({ notes: navi, navigator: { platform_value: "Linux x86_64", max_touch_points: 0 },
    webgl: { renderer: navi, extensions: ["EXT_amd"] } }));
  const old = (id, r, notes) => ({ name: "Profile 7", notes, _meta: { gpu_preset_id: id },
    navigator: { user_agent: ua, platform_value: "Linux armv81", max_touch_points: 5 },
    webgl: { renderer: r, extensions: ["EXT_old"], parameters: { "0x0D33": 8192 }, own_key: 1 } });

  const p = old("linux-uhd620", vk, vk);
  adoptLibraryFields(p, d);
  const once = structuredClone(p);
  adoptLibraryFields(p, d);
  assert.deepEqual(p, once);
  assert.deepEqual(p.webgl, { renderer: glx, extensions: ["EXT_new"], parameters: { "0x0D33": 16384 }, own_key: 1 });
  assert.equal(p.notes, glx);
  assert.equal(p.navigator.platform_value, "Linux armv81");
  const kept = old("linux-uhd620", vk, "work account");
  adoptLibraryFields(kept, d);
  assert.equal(kept.notes, "work account");

  const cedar = old("linux-unknown-amdcedardr", "AMD CEDAR (DRM 2.49.0 / 4.9.194-primeos+, LLVM 7.0.0)", "x");
  adoptLibraryFields(cedar, d);
  assert.equal(cedar.webgl.renderer, navi);
  assert.equal(cedar._meta.gpu_preset_id, "linux-navi21");
  assert.deepEqual([cedar.navigator.platform_value, cedar.navigator.max_touch_points], ["Linux x86_64", 0]);

  // An operator's own GLX capture, a host-own list, and a non-Linux profile stay.
  const es = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL ES 3.2)";
  const own = old("linux-uhd620", es, "x");
  adoptLibraryFields(own, d);
  assert.equal(own.webgl.renderer, es);
  const host = old("linux-uhd620", vk, "x");
  host.webgl.extensions = [];
  adoptLibraryFields(host, d);
  assert.deepEqual([host.webgl.extensions, host.webgl.renderer], [[], glx]);
  const win = old("linux-uhd620", vk, "x");
  win.navigator.user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
  adoptLibraryFields(win, d);
  assert.equal(win.webgl.renderer, vk);
});
