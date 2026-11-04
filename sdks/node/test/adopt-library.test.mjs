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
