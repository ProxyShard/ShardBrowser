// Apple M-series profiles gain the WebGPU feature Chrome 156 reports there.
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyAppleWebGpuFeatures } from "../dist/profile.js";

const M1 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";
const mac = (renderer, vendor, features) => ({ webgl: { renderer }, webgpu: { vendor, features } });

test("an Apple M list gains the feature in sorted place, once", () => {
  const cfg = mac(M1, "apple", ["shader-f16", "texture-compression-etc2", "texture-formats-tier1"]);
  applyAppleWebGpuFeatures(cfg);
  applyAppleWebGpuFeatures(cfg);
  assert.deepEqual(cfg.webgpu.features,
    ["shader-f16", "texture-compression-etc2", "texture-compression-unaligned", "texture-formats-tier1"]);
});

test("M2 and later also gain the Chrome 157 feature, M1 does not", () => {
  const m3 = mac("ANGLE (Apple, ANGLE Metal Renderer: Apple M3 Max, Unspecified Version)", "apple",
    ["shader-f16", "texture-compression-unaligned"]);
  applyAppleWebGpuFeatures(m3);
  assert.deepEqual(m3.webgpu.features, ["atomic-vec2u-min-max", "shader-f16", "texture-compression-unaligned"]);
  const m1 = mac(M1, "apple", ["shader-f16", "texture-compression-unaligned"]);
  applyAppleWebGpuFeatures(m1);
  assert.deepEqual(m1.webgpu.features, ["shader-f16", "texture-compression-unaligned"]);
});

test("other GPUs and an empty list are left alone", () => {
  for (const cfg of [
    mac("ANGLE (Intel, Intel(R) Iris(TM) Plus Graphics)", "intel", ["shader-f16"]),
    mac(M1, "intel", ["shader-f16"]),
    mac(M1, "apple", []),
  ]) {
    const before = JSON.stringify(cfg);
    applyAppleWebGpuFeatures(cfg);
    assert.equal(JSON.stringify(cfg), before);
  }
});
