import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import fs, { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import test from "node:test";
import AdmZip from "adm-zip";
import { awaitBrowserSpawn } from "../dist/browser.js";
import { acquireBrowserSessionLease, MANIFEST_URL, PUB_BASE, Runtime } from "../dist/runtime.js";
import { makeNativeBinary, widevineEntries, writeWidevine } from "./helpers/runtime.fixtures.mjs";

function makeZip(entries) {
  const archive = new AdmZip();
  for (const [name, contents] of Object.entries(entries)) {
    archive.addFile(name, Buffer.from(contents));
  }
  return archive.toBuffer();
}

function makeEngineBinary(name = "new-engine") {
  return makeNativeBinary(name);
}

function createInstalledRuntime() {
  const root = mkdtempSync(join(tmpdir(), "shardx-runtime-update-"));
  const runtime = new Runtime({ cacheDir: root });
  mkdirSync(dirname(runtime.binaryPath), { recursive: true });
  writeFileSync(runtime.binaryPath, makeEngineBinary("old-engine"));
  if (process.platform !== "win32") fs.chmodSync(runtime.binaryPath, 0o755);
  if (runtime.spec.widevine) {
    const widevineDir = join(root, ...runtime.spec.widevineSubpath);
    writeWidevine(widevineDir, "4.10.0.0");
  }
  mkdirSync(runtime.fingerprintsDir, { recursive: true });
  writeFileSync(join(runtime.fingerprintsDir, "template.json"), '{"version":"old"}');
  writeFileSync(join(runtime.fingerprintsDir, "user-custom.json"), '{"custom":true}');
  writeFileSync(join(runtime.fingerprintsDir, "user-notes.txt"), "keep me");
  writeFileSync(runtime.manifestPath, JSON.stringify({
    browser_etag: "old-browser",
    widevine_etag: "old-widevine",
    fingerprints_etag: "old-fingerprints",
    installed_chromium_version: "151.0.0",
    installed_engine_build: "old-build",
    installed_grease_brand: "old-brand",
    installed_grease_version: "151",
    installed_tls: { cipher_suites: ["old-suite"] },
  }));
  return { root, runtime };
}

function createArchiveResponses(runtime) {
  const responses = new Map();
  responses.set(runtime.spec.browser.key, makeZip({
    [runtime.spec.binarySubpath.join("/")]: makeEngineBinary(),
  }));
  if (runtime.spec.widevine) {
    const wrapper = runtime.spec.widevine.key.replace(/\.zip$/, "");
    responses.set(runtime.spec.widevine.key, makeZip(widevineEntries(`${wrapper}/WidevineCdm`, "4.10.0.1")));
  }
  responses.set("ShardX-Fingerprints.zip", makeZip({
    "shardx-fingerprints/template.json": '{"version":"new"}',
  }));
  return responses;
}

function mockFetch(runtime, options = {}) {
  const originalFetch = globalThis.fetch;
  const archives = createArchiveResponses(runtime);
  for (const [key, entries] of options.archiveEntries ?? []) archives.set(key, makeZip(entries));
  const manifest = options.manifest ?? {
    archives: Object.fromEntries([...archives.keys()].map((key) => [key, `etag-${key}`])),
    chromium_version: "153.0.0",
    engine_build: "new-build",
  };

  globalThis.fetch = async (input) => {
    const url = String(input);
    if (url === MANIFEST_URL) {
      await options.onManifest?.();
      return new Response(JSON.stringify(manifest), {
        headers: { "content-type": "application/json" },
      });
    }
    const key = url.slice(`${PUB_BASE}/`.length);
    if (key === options.failKey) return new Response("unavailable", { status: 503 });
    if (options.onArchive) options.onArchive(key);
    const bytes = key === options.corruptKey
      ? Buffer.from("not a zip")
      : key === options.invalidBinaryKey
        ? makeZip({ [runtime.spec.binarySubpath.join("/")]: "not an executable" })
        : archives.get(key);
    if (!bytes) throw new Error(`Unexpected test request: ${url}`);
    const headers = { etag: `etag-${key}` };
    if (key === options.incompleteKey) headers["content-length"] = String(bytes.length + 1);
    if (key === options.interruptedKey) {
      let firstChunk = true;
      return new Response(new ReadableStream({
        pull(controller) {
          if (firstChunk) {
            firstChunk = false;
            controller.enqueue(bytes.subarray(0, 4));
          } else {
            controller.error(new Error("simulated response stream failure"));
          }
        },
      }, { highWaterMark: 0 }), { headers });
    }
    return new Response(bytes, { headers });
  };

  return () => { globalThis.fetch = originalFetch; };
}

function assertNoStagingDirectories(root) {
  assert.equal(readdirSync(root).some((name) => name.startsWith(".update-")), false);
}

test("HTTP download failure leaves the installed runtime untouched", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime, { failKey: runtime.spec.browser.key });
  try {
    await assert.rejects(runtime.install({ force: true }), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("automatic update failure keeps the installed runtime available", async () => {
  const { root, runtime } = createInstalledRuntime();
  let manifestRequests = 0;
  const originalWarn = console.warn;
  const restoreFetch = mockFetch(runtime, {
    failKey: runtime.spec.browser.key,
    onManifest: () => { manifestRequests++; },
  });
  console.warn = () => {};
  try {
    await runtime.install();
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(runtime.chromiumVersion, "151.0.0");
    assert.equal(runtime.greaseBrand, "old-brand");
    assert.equal(runtime.greaseVersion, "151");
    assert.deepEqual(runtime.tls, { cipher_suites: ["old-suite"] });
    await runtime.install();
    assert.equal(manifestRequests, 1);
  } finally {
    console.warn = originalWarn;
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("automatic update failure is reported when the previous install lacks fingerprints", async () => {
  const { root, runtime } = createInstalledRuntime();
  rmSync(runtime.fingerprintsDir, { recursive: true, force: true });
  mkdirSync(runtime.fingerprintsDir, { recursive: true });
  const restoreFetch = mockFetch(runtime, {
    failKey: "ShardX-Fingerprints.zip",
    manifest: {
      archives: {},
      chromium_version: "151.0.0",
      engine_build: "old-build",
    },
  });
  try {
    await assert.rejects(runtime.install(), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readdirSync(runtime.fingerprintsDir), []);
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("automatic update failure is reported when the previous Widevine bundle is incomplete", async () => {
  const { root, runtime } = createInstalledRuntime();
  writeFileSync(join(root, ...runtime.spec.widevineSubpath, "manifest.json"), "{}");
  const previousManifest = readFileSync(runtime.manifestPath);
  const restoreFetch = mockFetch(runtime, { failKey: runtime.spec.browser.key });
  try {
    assert.equal(runtime.hasUsableInstalledRuntime(), false);
    await assert.rejects(runtime.install(), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint-only update preserves engine metadata omitted from the manifest", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime, {
    manifest: {
      archives: { "ShardX-Fingerprints.zip": "new-fingerprints" },
    },
  });
  try {
    await runtime.install();
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(runtime.chromiumVersion, "151.0.0");
    assert.equal(runtime.greaseBrand, "old-brand");
    assert.equal(runtime.greaseVersion, "151");
    assert.deepEqual(runtime.tls, { cipher_suites: ["old-suite"] });
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("a failed forced update reports the error but leaves the old runtime usable", async () => {
  const { root, runtime } = createInstalledRuntime();
  let manifestRequests = 0;
  const restoreFetch = mockFetch(runtime, {
    failKey: runtime.spec.browser.key,
    onManifest: () => { manifestRequests++; },
  });
  try {
    await assert.rejects(runtime.install({ force: true }), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    await runtime.install();
    assert.equal(manifestRequests, 1);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("corrupt engine archive leaves the installed runtime untouched", async () => {
  const { root, runtime } = createInstalledRuntime();
  let manifestRequests = 0;
  const restoreFetch = mockFetch(runtime, {
    corruptKey: runtime.spec.browser.key,
    onManifest: () => { manifestRequests++; },
  });
  try {
    await assert.rejects(runtime.install({ force: true }));
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(runtime.chromiumVersion, "151.0.0");
    await runtime.install();
    assert.equal(manifestRequests, 1);
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("incomplete engine download leaves the installed runtime untouched", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime, { incompleteKey: runtime.spec.browser.key });
  try {
    await assert.rejects(runtime.install({ force: true }), /incomplete response/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

for (const archive of ["engine", "fingerprints"]) {
  test(`interrupted ${archive} download preserves the installed runtime and removes staging`, async () => {
    const { root, runtime } = createInstalledRuntime();
    const previousManifest = readFileSync(runtime.manifestPath);
    const interruptedKey = archive === "engine" ? runtime.spec.browser.key : "ShardX-Fingerprints.zip";
    const restoreFetch = mockFetch(runtime, { interruptedKey });
    try {
      await assert.rejects(runtime.install({ force: true }), /simulated response stream failure/);
      assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
      assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
      assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
      assertNoStagingDirectories(root);
      assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
      const releaseBrowser = await acquireBrowserSessionLease(runtime);
      releaseBrowser();
      await runtime.install();
    } finally {
      restoreFetch();
      rmSync(root, { recursive: true, force: true });
    }
  });
}

const nonObjectFingerprints = [
  ["null", "null"], ["array", "[]"], ["number", "42"], ["boolean", "true"], ["string", '"profile"'],
];

for (const [name, json] of nonObjectFingerprints) {
  for (const force of [false, true]) {
    test(`a ${force ? "forced" : "normal"} update rejects ${name} fingerprint JSON and keeps the old installation`, async () => {
      const { root, runtime } = createInstalledRuntime();
      const previousManifest = readFileSync(runtime.manifestPath);
      const restoreFetch = mockFetch(runtime, {
        archiveEntries: new Map([["ShardX-Fingerprints.zip", {
          "shardx-fingerprints/template.json": json,
        }]]),
        ...(!force ? { manifest: {
          archives: { "ShardX-Fingerprints.zip": "new-fingerprints" },
          chromium_version: "151.0.0", engine_build: "old-build",
        } } : {}),
      });
      const originalWarn = console.warn;
      const warnings = [];
      console.warn = (message) => { warnings.push(String(message)); };
      runtime.activateWhenIdle = async () => { assert.fail("non-object fingerprints must not reach activation"); };
      try {
        if (force) await assert.rejects(runtime.install({ force }), /invalid JSON profile: template.json/);
        else {
          await runtime.install();
          assert.match(warnings.join("\n"), /invalid JSON profile: template.json/);
        }
        assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
        assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
        assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
        assert.equal(readFileSync(join(runtime.fingerprintsDir, "user-custom.json"), "utf8"), '{"custom":true}');
        assert.equal(readFileSync(join(runtime.fingerprintsDir, "user-notes.txt"), "utf8"), "keep me");
        assert.equal(runtime.chromiumVersion, "151.0.0");
        assertNoStagingDirectories(root);
        assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
      } finally {
        console.warn = originalWarn;
        restoreFetch();
        rmSync(root, { recursive: true, force: true });
      }
    });
  }
}

test("automatic update failure is reported when no installed fingerprint is a JSON object", async () => {
  const { root, runtime } = createInstalledRuntime();
  writeFileSync(join(runtime.fingerprintsDir, "template.json"), "null");
  writeFileSync(join(runtime.fingerprintsDir, "user-custom.json"), "[]");
  const previousManifest = readFileSync(runtime.manifestPath);
  const restoreFetch = mockFetch(runtime, { failKey: runtime.spec.browser.key });
  try {
    assert.equal(runtime.hasUsableInstalledRuntime(), false);
    await assert.rejects(runtime.install(), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), "null");
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("archive with a non-executable file at the browser path is rejected", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime, { invalidBinaryKey: runtime.spec.browser.key });
  try {
    await assert.rejects(runtime.install({ force: true }), /missing a valid executable/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

const invalidUpdates = [
  ["executable containing only magic bytes", (runtime) => [
    runtime.spec.browser.key, { [runtime.spec.binarySubpath.join("/")]: makeEngineBinary().subarray(0, 4) },
  ], /missing a valid executable/],
  ["executable with a truncated header", (runtime) => [
    runtime.spec.browser.key, { [runtime.spec.binarySubpath.join("/")]: makeEngineBinary().subarray(0, 20) },
  ], /missing a valid executable/],
  ["executable with truncated section data", (runtime) => [
    runtime.spec.browser.key, { [runtime.spec.binarySubpath.join("/")]: makeEngineBinary().subarray(0, -1) },
  ], /missing a valid executable/],
  ["Widevine containing only an unrelated file", (runtime) => [
    runtime.spec.widevine.key, { [runtime.spec.widevine.key.slice(0, -4) + "/WidevineCdm/notes.txt"]: "not a CDM" },
  ], /Widevine archive did not produce/],
  ["Widevine without its library", (runtime) => [
    runtime.spec.widevine.key, {
      [runtime.spec.widevine.key.slice(0, -4) + "/WidevineCdm/manifest.json"]: '{"version":"4.10.0.1"}',
    },
  ], /Widevine archive did not produce/],
  ["Widevine with a corrupt manifest", (runtime) => {
    const prefix = runtime.spec.widevine.key.slice(0, -4) + "/WidevineCdm";
    return [runtime.spec.widevine.key, { ...widevineEntries(prefix), [prefix + "/manifest.json"]: "{" }];
  }, /Widevine archive did not produce/],
  ["Widevine with truncated library data", (runtime) => {
    const prefix = runtime.spec.widevine.key.slice(0, -4) + "/WidevineCdm";
    const entries = widevineEntries(prefix);
    for (const key of Object.keys(entries)) {
      if (!key.endsWith("/manifest.json")) entries[key] = entries[key].subarray(0, -1);
    }
    return [runtime.spec.widevine.key, entries];
  }, /Widevine archive did not produce/],
];

for (const [name, invalidArchive, expectedError] of invalidUpdates) {
  test(name + " never activates and preserves the installed runtime", async () => {
    const { root, runtime } = createInstalledRuntime();
    const previousManifest = readFileSync(runtime.manifestPath);
    const previousWidevine = Object.keys(widevineEntries(join(root, ...runtime.spec.widevineSubpath)))
      .map((path) => [path, readFileSync(path)]);
    const restoreFetch = mockFetch(runtime, { archiveEntries: new Map([invalidArchive(runtime)]) });
    const warn = console.warn;
    const warnings = [];
    console.warn = (message) => { warnings.push(String(message)); };
    runtime.activateWhenIdle = async () => { assert.fail("invalid archives must not reach activation"); };
    try {
      await assert.rejects(runtime.install({ force: true }), expectedError);
      const normalRuntime = new Runtime({ cacheDir: root });
      normalRuntime.activateWhenIdle = runtime.activateWhenIdle;
      await normalRuntime.install(); // A new instance must attempt the update and keep the old installation.
      assert.match(warnings.join("\n"), expectedError);
      assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
      assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
      assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
      for (const [path, bytes] of previousWidevine) assert.deepEqual(readFileSync(path), bytes);
      assert.equal(runtime.hasUsableInstalledRuntime(), true);
      assertNoStagingDirectories(root);
      assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
    } finally {
      console.warn = warn;
      restoreFetch();
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("a failed Widevine-only update leaves the existing component untouched", async () => {
  const { root, runtime } = createInstalledRuntime();
  const metadata = JSON.parse(readFileSync(runtime.manifestPath, "utf8"));
  delete metadata.widevine_etag;
  writeFileSync(runtime.manifestPath, JSON.stringify(metadata));
  const previousManifest = readFileSync(runtime.manifestPath);
  const previousWidevine = Object.keys(widevineEntries(join(root, ...runtime.spec.widevineSubpath)))
    .map((path) => [path, readFileSync(path)]);
  const requestedArchives = [];
  const [, invalidArchive] = invalidUpdates[3][1](runtime);
  const restoreFetch = mockFetch(runtime, {
    archiveEntries: new Map([[runtime.spec.widevine.key, invalidArchive]]),
    manifest: { archives: {}, chromium_version: "151.0.0", engine_build: "old-build" },
    onArchive: (key) => requestedArchives.push(key),
  });
  const warn = console.warn;
  const warnings = [];
  console.warn = (message) => { warnings.push(String(message)); };
  runtime.activateWhenIdle = async () => { assert.fail("invalid Widevine must not reach activation"); };
  try {
    await runtime.install();
    assert.deepEqual(requestedArchives, [runtime.spec.widevine.key]);
    assert.match(warnings.join("\n"), /Widevine archive did not produce/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
    for (const [path, bytes] of previousWidevine) assert.deepEqual(readFileSync(path), bytes);
    assertNoStagingDirectories(root);
  } finally {
    console.warn = warn;
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

for (const platform of ["win32", "linux", "darwin"]) {
  test(platform + " validation rejects malformed native headers without running binaries", () => {
    const root = mkdtempSync(join(tmpdir(), "shardx-runtime-headers-"));
    try {
      // Only this Node subprocess runs. All engine files are inert fixtures.
      const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
        import assert from "node:assert/strict";
        import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
        import { syncBuiltinESMExports } from "node:module";
        import os from "node:os";
        import { dirname, join } from "node:path";

        const platform = process.argv[3];
        os.platform = () => platform;
        os.arch = () => platform === "darwin" ? "arm64" : "x64";
        syncBuiltinESMExports();
        globalThis.fetch = async () => { throw new Error("Unexpected network request"); };
        const { Runtime } = await import(process.argv[1]);
        const { makeNativeBinary, writeWidevine } = await import(process.argv[4]);
        const runtime = new Runtime({ cacheDir: process.argv[2] });
        mkdirSync(dirname(runtime.binaryPath), { recursive: true });
        writeWidevine(join(runtime.root, ...runtime.spec.widevineSubpath), "4.10.0.0", platform);
        writeFileSync(join(runtime.fingerprintsDir, "template.json"), "{}");
        const valid = makeNativeBinary("not-runnable", platform);
        writeFileSync(runtime.binaryPath, valid);
        if (process.platform !== "win32") chmodSync(runtime.binaryPath, 0o755);
        assert.equal(runtime.hasUsableInstalledRuntime(), true);
        const wrongArchitecture = Buffer.from(valid);
        if (platform === "win32") wrongArchitecture.writeUInt16LE(0x14c, 132);
        else if (platform === "linux") wrongArchitecture.writeUInt16LE(183, 18);
        else wrongArchitecture.writeUInt32LE(0x01000007, 4);
        const badTable = Buffer.from(valid);
        if (platform === "win32") badTable.writeUInt32LE(0xffffffff, 60);
        else if (platform === "linux") badTable.writeBigUInt64LE(1n << 63n, 32);
        else badTable.writeUInt32LE(0xffffffff, 20);
        for (const invalid of [
          valid.subarray(0, 4), valid.subarray(0, 20), valid.subarray(0, -1),
          wrongArchitecture, badTable, makeNativeBinary("library", platform, true),
        ]) {
          writeFileSync(runtime.binaryPath, invalid);
          assert.equal(runtime.hasUsableInstalledRuntime(), false);
        }
      `, new URL("../dist/runtime.js", import.meta.url).href, root, platform,
      new URL("./helpers/runtime.fixtures.mjs", import.meta.url).href], { encoding: "utf8", timeout: 10000 });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr || result.stdout);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("macOS executable validation rejects Linux and Windows files before activation", () => {
  const root = mkdtempSync(join(tmpdir(), "shardx-runtime-macos-"));
  try {
    // Isolate the host mock from other tests and replace all download/extraction IO.
    const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import assert from "node:assert/strict";
      import { chmodSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
      import { syncBuiltinESMExports } from "node:module";
      import os from "node:os";
      import { dirname, join } from "node:path";

      os.platform = () => "darwin";
      os.arch = () => "arm64";
      syncBuiltinESMExports();
      globalThis.fetch = async () => { throw new Error("Unexpected network request"); };
      const { Runtime } = await import(process.argv[1]);
      const { makeNativeBinary, makeUniversalBinary, writeWidevine } = await import(process.argv[3]);
      const runtime = new Runtime({ cacheDir: process.argv[2] });
      const machOHeaders = [
        makeNativeBinary("arm64-le", "darwin"),
        makeNativeBinary("arm64-be", "darwin", false, false),
        makeUniversalBinary("fat-be"),
        makeUniversalBinary("fat-le", { little: true }),
        makeUniversalBinary("fat64-be", { fat64: true }),
        makeUniversalBinary("fat64-le", { fat64: true, little: true }),
      ];
      const invalidHeaders = [
        makeNativeBinary("linux", "linux"),
        makeNativeBinary("windows", "win32"),
        Buffer.from([0xcf, 0xfa, 0xed, 0xfe]),
        Buffer.from([0xfe, 0xed, 0xfa, 0xce]),
        Buffer.from([0xca, 0xfe, 0xba, 0xbe]),
      ];
      writeWidevine(join(runtime.root, ...runtime.spec.widevineSubpath), "4.10.0.0", "darwin");
      mkdirSync(dirname(runtime.binaryPath), { recursive: true });
      const fingerprintPath = join(runtime.fingerprintsDir, "template.json");
      writeFileSync(fingerprintPath, '{"version":"old"}');
      writeFileSync(runtime.manifestPath, '{"installed_chromium_version":"151.0.0"}');
      const previousManifest = readFileSync(runtime.manifestPath);

      for (const header of machOHeaders) {
        writeFileSync(runtime.binaryPath, header);
        if (process.platform !== "win32") chmodSync(runtime.binaryPath, 0o755);
        assert.equal(runtime.hasUsableInstalledRuntime(), true);
      }
      for (const header of invalidHeaders) {
        writeFileSync(runtime.binaryPath, header);
        assert.equal(runtime.hasUsableInstalledRuntime(), false);
      }

      const oldEngine = makeNativeBinary("old-engine", "darwin");
      writeFileSync(runtime.binaryPath, oldEngine);
      runtime.fetchManifest = async () => ({ archives: {}, chromiumVersion: "153.0.0" });
      runtime.installFingerprints = async () => {
        throw new Error("Invalid executable reached fingerprint preparation");
      };
      for (const header of invalidHeaders) {
        runtime.downloadAndExtract = async (archive, destination) => {
          if (archive.key === runtime.spec.browser.key) {
            const path = join(destination, ...runtime.spec.binarySubpath);
            mkdirSync(dirname(path), { recursive: true });
            writeFileSync(path, header);
          } else {
            writeWidevine(join(destination, archive.key.slice(0, -4), "WidevineCdm"), "4.10.0.1", "darwin");
          }
          return "new-etag";
        };
        await assert.rejects(runtime.install({ force: true }), /missing a valid executable/);
        assert.deepEqual(readFileSync(runtime.binaryPath), oldEngine);
        assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
        assert.equal(readFileSync(fingerprintPath, "utf8"), '{"version":"old"}');
        assert.equal(readdirSync(runtime.root).some((name) => name.startsWith(".update-")), false);
        assert.equal(readdirSync(runtime.root).includes(".runtime-update-recovery"), false);
      }
    `, new URL("../dist/runtime.js", import.meta.url).href, root, new URL("./helpers/runtime.fixtures.mjs", import.meta.url).href], {
      encoding: "utf8",
      timeout: 10000,
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr || result.stdout);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

for (const platform of ["linux", "darwin"]) {

  test(`${platform} permission preparation preserves the old runtime on failure`, () => {
    const root = mkdtempSync(join(tmpdir(), "shardx-runtime-permissions-"));
    try {
      const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
        import assert from "node:assert/strict";
        import childProcess from "node:child_process";
        import fs from "node:fs";
        import { createRequire, syncBuiltinESMExports } from "node:module";
        import os from "node:os";
        import { dirname, join } from "node:path";

        const platform = process.argv[3];
        os.platform = () => platform;
        os.arch = () => platform === "darwin" ? "arm64" : "x64";
        childProcess.spawn = () => { throw new Error("Real browser launch is forbidden"); };
        syncBuiltinESMExports();
        const { MANIFEST_URL, PUB_BASE, Runtime } = await import(process.argv[1]);
        const { makeNativeBinary, widevineEntries, writeWidevine } = await import(process.argv[4]);
        const AdmZip = createRequire(process.argv[1])("adm-zip");
        const original = {
          chmod: fs.chmodSync, access: fs.accessSync, unzip: childProcess.spawnSync,
          fetch: globalThis.fetch, warn: console.warn,
        };
        const oldEngine = makeNativeBinary("old-inert-engine", platform);
        const newEngine = makeNativeBinary("new-inert-engine", platform);
        const helper = makeNativeBinary("new-inert-helper", platform);
        let checked = 0;
        let nextCase = 0;
        const prepare = () => {
          const runtime = new Runtime({ cacheDir: join(process.argv[2], "case-" + nextCase++) });
          fs.mkdirSync(dirname(runtime.binaryPath), { recursive: true });
          fs.writeFileSync(runtime.binaryPath, oldEngine);
          fs.chmodSync(runtime.binaryPath, 0o755);
          writeWidevine(join(runtime.root, ...runtime.spec.widevineSubpath), "4.10.0.0", platform);
          fs.writeFileSync(join(runtime.fingerprintsDir, "template.json"), '{"version":"old"}');
          fs.writeFileSync(join(runtime.fingerprintsDir, "user-custom.json"), '{"custom":true}');
          fs.writeFileSync(runtime.manifestPath, JSON.stringify({
            browser_etag: "old", widevine_etag: "old", fingerprints_etag: "old",
            installed_chromium_version: "151.0.0.0", installed_engine_build: "old",
          }));
          return { runtime, manifest: fs.readFileSync(runtime.manifestPath) };
        };
        const zip = (entries) => {
          const archive = new AdmZip();
          for (const [name, bytes] of Object.entries(entries)) archive.addFile(name, Buffer.from(bytes), "", 0o644);
          return archive.toBuffer();
        };
        const assertPreserved = ({ runtime, manifest }) => {
          assert.deepEqual(fs.readFileSync(runtime.binaryPath), oldEngine);
          assert.deepEqual(fs.readFileSync(runtime.manifestPath), manifest);
          assert.equal(fs.readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
          assert.equal(fs.readFileSync(join(runtime.fingerprintsDir, "user-custom.json"), "utf8"), '{"custom":true}');
          assert.equal(fs.existsSync(join(runtime.root, ".runtime-update-recovery")), false);
          assert.equal(fs.readdirSync(runtime.root).some(name => name.startsWith(".update-")), false);
        };
        const restore = () => {
          fs.chmodSync = original.chmod;
          fs.accessSync = original.access;
          childProcess.spawnSync = original.unzip;
          globalThis.fetch = original.fetch;
          console.warn = original.warn;
          syncBuiltinESMExports();
        };
        for (const failure of ["engine chmod", "helper chmod", "execute access", null]) {
          for (const force of [false, true]) {
            const local = prepare();
            const { runtime } = local;
            const binaryEntry = runtime.spec.binarySubpath.join("/");
            const helperEntry = runtime.spec.binarySubpath[0] + "/inert-helper";
            const archives = new Map([
              [runtime.spec.browser.key, zip({ [binaryEntry]: newEngine, [helperEntry]: helper })],
              [runtime.spec.widevine.key, zip(widevineEntries(
                runtime.spec.widevine.key.slice(0, -4) + "/WidevineCdm", "4.10.0.1", platform,
              ))],
              ["ShardX-Fingerprints.zip", zip({ "shardx-fingerprints/template.json": '{"version":"new"}' })],
            ]);
            const permissionError = Object.assign(new Error("simulated " + failure + " failure"), { code: "EACCES" });
            const warnings = [];
            let injected = false;
            let extracting = false;
            let requests = 0;
            const staged = path => typeof path === "string" && path.startsWith(join(runtime.root, ".update-"));
            globalThis.fetch = async input => {
              requests++;
              const url = String(input);
              if (url === MANIFEST_URL) return new Response(JSON.stringify({
                archives: Object.fromEntries([...archives.keys()].map(key => [key, "new"])),
                chromium_version: "153.0.0.0", engine_build: "new",
              }));
              assert.ok(url.startsWith(PUB_BASE + "/"));
              const bytes = archives.get(url.slice((PUB_BASE + "/").length));
              assert.ok(bytes, "Unexpected mock archive");
              return new Response(bytes, { headers: { etag: "new", "content-length": String(bytes.length) } });
            };
            childProcess.spawnSync = (command, args) => {
              assert.equal(command, "unzip");
              extracting = true;
              try { new AdmZip(args[2]).extractAllTo(args[4], true); }
              finally { extracting = false; }
              return { status: 0, signal: null, stderr: Buffer.alloc(0) };
            };
            fs.chmodSync = (path, ...args) => {
              const target = failure === "engine chmod" ? newEngine : failure === "helper chmod" ? helper : null;
              if (!extracting && target && staged(path) && fs.readFileSync(path).equals(target)) {
                injected = true;
                throw permissionError;
              }
              return original.chmod(path, ...args);
            };
            fs.accessSync = (path, mode) => {
              if (failure === "execute access" && mode === fs.constants.X_OK && staged(path)
                  && fs.readFileSync(path).equals(newEngine)) {
                injected = true;
                throw permissionError;
              }
              return original.access(path, mode);
            };
            console.warn = message => warnings.push(String(message));
            syncBuiltinESMExports();
            try {
              if (failure) {
                if (force) await assert.rejects(runtime.install({ force }), error => error === permissionError);
                else {
                  await runtime.install();
                  assert.ok(warnings.some(message => message.includes(permissionError.message)));
                }
                assert.equal(injected, true, "The permission failure must be exercised");
                assertPreserved(local);
                assert.equal(runtime.chromiumVersion, "151.0.0.0");
                assert.equal(runtime.hasUsableInstalledRuntime(), true);
                const previousRequests = requests;
                await runtime.install();
                assert.equal(requests, previousRequests);
              } else {
                await runtime.install({ force });
                assert.equal(injected, false);
                assert.deepEqual(fs.readFileSync(runtime.binaryPath), newEngine);
                assert.deepEqual(fs.readFileSync(join(runtime.root, helperEntry)), helper);
                assert.equal(runtime.chromiumVersion, "153.0.0.0");
                assert.equal(fs.readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"new"}');
              }
              assert.equal(fs.readdirSync(runtime.root).some(name => name.startsWith(".update-")), false);
              checked++;
            } finally { restore(); }
          }
        }
        for (const force of [false, true]) {
          const local = prepare();
          const { runtime } = local;
          globalThis.fetch = async input => {
            if (String(input) === MANIFEST_URL) return new Response('{"archives":{},"chromium_version":"153.0.0.0"}');
            assert.equal(String(input), PUB_BASE + "/" + runtime.spec.browser.key);
            return new Response("mock failure", { status: 503 });
          };
          fs.accessSync = (path, mode) => {
            if (path === runtime.binaryPath && mode === fs.constants.X_OK) {
              throw Object.assign(new Error("old runtime lacks execute access"), { code: "EACCES" });
            }
            return original.access(path, mode);
          };
          syncBuiltinESMExports();
          try {
            assert.equal(runtime.hasUsableInstalledRuntime(), false);
            await assert.rejects(runtime.install({ force }), /HTTP 503/);
            assertPreserved(local);
            checked++;
          } finally { restore(); }
        }
        console.log("permission cases: " + checked);
      `, new URL("../dist/runtime.js", import.meta.url).href, root, platform,
      new URL("./helpers/runtime.fixtures.mjs", import.meta.url).href], { encoding: "utf8", timeout: 10000 });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr || result.stdout);
      assert.match(result.stdout, /permission cases: 10/);
    } finally {
      assert.equal(dirname(root), tmpdir());
      rmSync(root, { recursive: true, force: true });
    }
  });

  test(`${platform} unzip failures preserve the installed runtime and valid exit codes still activate`, () => {
    const root = mkdtempSync(join(tmpdir(), "shardx-runtime-unzip-"));
    try {
      // Only the isolated Node test process runs; unzip and network IO are mocked.
      const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
        import assert from "node:assert/strict";
        import childProcess from "node:child_process";
        import { chmodSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
        import { createRequire, syncBuiltinESMExports } from "node:module";
        import os from "node:os";
        import { basename, dirname, join } from "node:path";

        const platform = process.argv[3];
        os.platform = () => platform;
        os.arch = () => platform === "darwin" ? "arm64" : "x64";
        syncBuiltinESMExports();
        const { acquireBrowserSessionLease, PUB_BASE, Runtime } = await import(process.argv[1]);
        const { makeNativeBinary, widevineEntries, writeWidevine } = await import(process.argv[4]);
        const AdmZip = createRequire(process.argv[1])("adm-zip");
        const makeZip = (entries) => {
          const archive = new AdmZip();
          for (const [name, bytes] of Object.entries(entries)) archive.addFile(name, Buffer.from(bytes));
          return archive.toBuffer();
        };
        const oldEngine = makeNativeBinary("old-engine", platform);
        const newEngine = makeNativeBinary("new-engine", platform);
        const cases = [
          { status: null, signal: "SIGKILL", success: false },
          { status: 0, signal: "SIGKILL", success: false },
          { status: null, signal: null, success: false },
          { status: 2, signal: null, success: false },
          { status: 0, signal: null, success: true },
          { status: 1, signal: null, success: true },
        ];
        for (const [index, scenario] of cases.entries()) {
          const runtime = new Runtime({ cacheDir: join(process.argv[2], "case-" + index) });
          const fingerprintPath = join(runtime.fingerprintsDir, "template.json");
          mkdirSync(dirname(runtime.binaryPath), { recursive: true });
          writeFileSync(runtime.binaryPath, oldEngine);
          if (process.platform !== "win32") chmodSync(runtime.binaryPath, 0o755);
          writeWidevine(join(runtime.root, ...runtime.spec.widevineSubpath), "4.10.0.0", platform);
          writeFileSync(fingerprintPath, '{"version":"old"}');
          writeFileSync(runtime.manifestPath, '{"installed_chromium_version":"151.0.0"}');
          const previousManifest = readFileSync(runtime.manifestPath);
          const binaryEntry = runtime.spec.binarySubpath.join("/");
          const supportEntry = runtime.spec.binarySubpath[0] + "/support.dat";
          const widevineWrapper = runtime.spec.widevine.key.slice(0, -4);
          const archives = new Map([
            [runtime.spec.browser.key, makeZip({ [binaryEntry]: newEngine, [supportEntry]: "support" })],
            [runtime.spec.widevine.key, makeZip(widevineEntries(widevineWrapper + "/WidevineCdm", "4.10.0.1", platform))],
            ["ShardX-Fingerprints.zip", makeZip({ "shardx-fingerprints/template.json": '{"version":"new"}' })],
          ]);
          runtime.fetchManifest = async () => ({ archives: {}, chromiumVersion: "153.0.0" });
          globalThis.fetch = async (input) => {
            const bytes = archives.get(String(input).slice((PUB_BASE + "/").length));
            if (!bytes) throw new Error("Unexpected test request: " + String(input));
            return new Response(bytes);
          };
          childProcess.spawnSync = (command, args) => {
            assert.equal(command, "unzip");
            const archive = new AdmZip(args[2]);
            const destination = args[4];
            if (basename(args[2]) === "." + runtime.spec.browser.key + ".tmp") {
              if (scenario.success) {
                archive.extractAllTo(destination, true);
              } else {
                const stagedBinary = join(destination, ...runtime.spec.binarySubpath);
                mkdirSync(dirname(stagedBinary), { recursive: true });
                writeFileSync(stagedBinary, archive.getEntry(binaryEntry).getData());
              }
              return { status: scenario.status, signal: scenario.signal, stderr: Buffer.alloc(0) };
            }
            archive.extractAllTo(destination, true);
            return { status: 0, signal: null, stderr: Buffer.alloc(0) };
          };
          syncBuiltinESMExports();

          if (scenario.success) {
            await runtime.install({ force: true });
            assert.deepEqual(readFileSync(runtime.binaryPath), newEngine);
            assert.equal(readFileSync(join(runtime.root, supportEntry), "utf8"), "support");
            assert.equal(readFileSync(fingerprintPath, "utf8"), '{"version":"new"}');
            assert.equal(runtime.chromiumVersion, "153.0.0");
          } else {
            await assert.rejects(runtime.install({ force: true }), /unzip failed/);
            assert.deepEqual(readFileSync(runtime.binaryPath), oldEngine);
            assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
            assert.equal(readFileSync(fingerprintPath, "utf8"), '{"version":"old"}');
            assert.equal(runtime.chromiumVersion, "151.0.0");
          }
          assert.equal(readdirSync(runtime.root).some((name) => name.startsWith(".update-")), false);
          assert.equal(readdirSync(runtime.root).includes(".runtime-update-recovery"), false);
          const releaseBrowser = await acquireBrowserSessionLease(runtime);
          releaseBrowser();
        }
      `, new URL("../dist/runtime.js", import.meta.url).href, root, platform, new URL("./helpers/runtime.fixtures.mjs", import.meta.url).href], {
        encoding: "utf8",
        timeout: 10000,
      });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr || result.stdout);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("new runtime is activated only after all archives have been prepared", async () => {
  const { root, runtime } = createInstalledRuntime();
  let oldRuntimeStayedActive = false;
  const restoreFetch = mockFetch(runtime);
  const installFingerprints = runtime.installFingerprints.bind(runtime);
  runtime.installFingerprints = async (...args) => {
    await installFingerprints(...args);
    oldRuntimeStayedActive = readFileSync(runtime.binaryPath).equals(makeEngineBinary("old-engine"));
  };
  try {
    await runtime.install({ force: true });
    assert.equal(oldRuntimeStayedActive, true);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
    assert.equal(runtime.chromiumVersion, "153.0.0");
    if (runtime.spec.widevine) {
      assert.equal(
        JSON.parse(readFileSync(join(root, ...runtime.spec.widevineSubpath, "manifest.json"), "utf8")).version,
        "4.10.0.1",
      );
    }
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"new"}');
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "user-custom.json"), "utf8"), '{"custom":true}');
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "user-notes.txt"), "utf8"), "keep me");
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("a partial recovery marker write is cleaned up and does not block restart", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  const markerPath = join(root, ".runtime-update-recovery");
  const previousManifest = readFileSync(runtime.manifestPath);
  const originalOpen = fs.openSync;
  const originalWrite = fs.writeFileSync;
  let markerFd;
  try {
    try {
      fs.openSync = function (path, ...args) {
        const fd = originalOpen.call(fs, path, ...args);
        if (path === markerPath) markerFd = fd;
        return fd;
      };
      fs.writeFileSync = function (file, data, ...args) {
        if (file === markerPath || file === markerFd) {
          originalWrite.call(fs, file, String(data).slice(0, 4), ...args);
          throw Object.assign(new Error("simulated marker write failure"), { code: "ENOSPC" });
        }
        return originalWrite.call(fs, file, data, ...args);
      };
      syncBuiltinESMExports();
      await assert.rejects(runtime.install({ force: true }), /simulated marker write failure/);
    } finally {
      fs.openSync = originalOpen;
      fs.writeFileSync = originalWrite;
      syncBuiltinESMExports();
    }
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
    assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
    assertNoStagingDirectories(root);
    const restartedRuntime = new Runtime({ cacheDir: root });
    restartedRuntime.fetchManifest = async () => ({ archives: {} });
    await restartedRuntime.install();
    const releaseBrowser = await acquireBrowserSessionLease(restartedRuntime);
    releaseBrowser();
    assert.deepEqual(readFileSync(restartedRuntime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(restartedRuntime.chromiumVersion, "151.0.0");
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("activation never removes a recovery marker it did not create", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  const markerPath = join(root, ".runtime-update-recovery");
  const markerContents = "existing recovery data";
  const previousManifest = readFileSync(runtime.manifestPath);
  writeFileSync(markerPath, markerContents);
  try {
    await assert.rejects(runtime.install({ force: true }), { code: "EEXIST" });
    assert.equal(readFileSync(markerPath, "utf8"), markerContents);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
    assertNoStagingDirectories(root);
    const restartedRuntime = new Runtime({ cacheDir: root });
    await assert.rejects(restartedRuntime.install(), /unfinished runtime update requires recovery/);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("activation failure rolls the old runtime and fingerprints back", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  runtime.saveManifest = () => { throw new Error("simulated manifest switch failure"); };
  try {
    await assert.rejects(runtime.install({ force: true }), /simulated manifest switch failure/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "user-custom.json"), "utf8"), '{"custom":true}');
    assert.equal(runtime.chromiumVersion, "151.0.0");
    assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
    await runtime.install();
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

for (const force of [false, true]) {
  test(`manifest temporary-file cleanup failure does not undo a ${force ? "forced" : "normal"} update after restart`, async () => {
    const { root, runtime } = createInstalledRuntime();
    const requestedArchives = [];
    const restoreFetch = mockFetch(runtime, { onArchive: (key) => requestedArchives.push(key) });
    const originalRm = fs.rmSync;
    const originalWarn = console.warn;
    const warnings = [];
    let injected = false;
    console.warn = (message) => { warnings.push(String(message)); };
    fs.rmSync = function (path, ...args) {
      if (!injected && typeof path === "string"
          && dirname(path).startsWith(join(root, ".update-"))
          && /^manifest-.+\.tmp$/.test(basename(path))) {
        injected = true;
        throw Object.assign(new Error("simulated manifest temporary-file cleanup failure"), { code: "EACCES" });
      }
      return originalRm.call(fs, path, ...args);
    };
    syncBuiltinESMExports();
    try {
      await runtime.install({ force });
      assert.equal(injected, true);
      assert.match(warnings.join("\n"), /runtime manifest temporary-file cleanup failed/);
      assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
      assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"new"}');
      const committedManifest = readFileSync(runtime.manifestPath);
      assert.equal(JSON.parse(committedManifest).installed_chromium_version, "153.0.0");
      assert.equal(runtime.chromiumVersion, "153.0.0");
      assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
      assertNoStagingDirectories(root);
      requestedArchives.length = 0;
      const restartedRuntime = new Runtime({ cacheDir: root });
      await restartedRuntime.install();
      assert.deepEqual(requestedArchives, []);
      assert.deepEqual(readFileSync(restartedRuntime.binaryPath), makeEngineBinary());
      assert.deepEqual(readFileSync(restartedRuntime.manifestPath), committedManifest);
      assert.equal(restartedRuntime.chromiumVersion, "153.0.0");
      assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
      assertNoStagingDirectories(root);
    } finally {
      fs.rmSync = originalRm;
      syncBuiltinESMExports();
      console.warn = originalWarn;
      restoreFetch();
      rmSync(root, { recursive: true, force: true });
    }
  });
}

for (const operation of ["write", "rename"]) {
  test(`a manifest ${operation} failure is preserved when temporary-file cleanup also fails`, async () => {
    const { root, runtime } = createInstalledRuntime();
    const previousManifest = readFileSync(runtime.manifestPath);
    const restoreFetch = mockFetch(runtime);
    const originalWrite = fs.writeFileSync;
    const originalRename = fs.renameSync;
    const originalRm = fs.rmSync;
    const originalWarn = console.warn;
    const primaryError = Object.assign(new Error(`simulated manifest ${operation} failure`), { code: "EIO" });
    const warnings = [];
    let primaryInjected = false;
    let cleanupInjected = false;
    const isManifestTemp = (path) => typeof path === "string"
      && dirname(path).startsWith(join(root, ".update-"))
      && /^manifest-.+\.tmp$/.test(basename(path));
    console.warn = (message) => { warnings.push(String(message)); };
    fs.writeFileSync = function (path, ...args) {
      if (operation === "write" && isManifestTemp(path)) {
        primaryInjected = true;
        throw primaryError;
      }
      return originalWrite.call(fs, path, ...args);
    };
    fs.renameSync = function (source, ...args) {
      if (operation === "rename" && isManifestTemp(source)) {
        primaryInjected = true;
        throw primaryError;
      }
      return originalRename.call(fs, source, ...args);
    };
    fs.rmSync = function (path, ...args) {
      if (isManifestTemp(path)) {
        cleanupInjected = true;
        throw Object.assign(new Error("simulated manifest temporary-file cleanup failure"), { code: "EACCES" });
      }
      return originalRm.call(fs, path, ...args);
    };
    syncBuiltinESMExports();
    try {
      await assert.rejects(runtime.install({ force: true }), (error) => error === primaryError);
      assert.equal(primaryInjected, true);
      assert.equal(cleanupInjected, true);
      assert.match(warnings.join("\n"), /runtime manifest temporary-file cleanup failed/);
      assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
      assert.deepEqual(readFileSync(runtime.manifestPath), previousManifest);
      assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
      assert.equal(runtime.chromiumVersion, "151.0.0");
      assert.equal(readdirSync(root).includes(".runtime-update-recovery"), false);
      assertNoStagingDirectories(root);
    } finally {
      fs.writeFileSync = originalWrite;
      fs.renameSync = originalRename;
      fs.rmSync = originalRm;
      syncBuiltinESMExports();
      console.warn = originalWarn;
      restoreFetch();
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("staging cleanup failure does not fail a successful activation", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  const originalWarn = console.warn;
  const warnings = [];
  const activateWhenIdle = runtime.activateWhenIdle.bind(runtime);
  runtime.activateWhenIdle = async (...args) => {
    await activateWhenIdle(...args);
    const backupRoot = join(args[2], ".backups");
    rmSync(backupRoot, { recursive: true, force: true });
    writeFileSync(backupRoot, "cleanup failure fixture");
  };
  console.warn = (message) => { warnings.push(String(message)); };
  try {
    await runtime.install({ force: true });
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
    assert.match(warnings.join("\n"), /runtime update staging cleanup failed/);
    assert.equal(readdirSync(root).some((name) => name.startsWith(".update-")), true);
    const restartedRuntime = new Runtime({ cacheDir: root });
    await restartedRuntime.install();
    assert.deepEqual(readFileSync(restartedRuntime.binaryPath), makeEngineBinary());
  } finally {
    console.warn = originalWarn;
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("staging files are removed after a fingerprint download failure", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime, { failKey: "ShardX-Fingerprints.zip" });
  try {
    await assert.rejects(runtime.install({ force: true }), /HTTP 503/);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    assert.equal(readFileSync(join(runtime.fingerprintsDir, "template.json"), "utf8"), '{"version":"old"}');
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("a force request during a normal install is not lost", async () => {
  const { root, runtime } = createInstalledRuntime();
  let releaseFirstManifest;
  let signalFirstManifest;
  const firstManifestReached = new Promise((resolve) => { signalFirstManifest = resolve; });
  const firstManifestGate = new Promise((resolve) => { releaseFirstManifest = resolve; });
  let manifestRequests = 0;
  const restoreFetch = mockFetch(runtime, {
    onManifest: async () => {
      manifestRequests++;
      if (manifestRequests === 1) {
        signalFirstManifest();
        await firstManifestGate;
      }
    },
  });
  try {
    const normalInstall = runtime.install();
    await firstManifestReached;
    const forcedInstall = runtime.install({ force: true });
    releaseFirstManifest();
    await Promise.all([normalInstall, forcedInstall]);
    assert.equal(manifestRequests, 2);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
    assertNoStagingDirectories(root);
  } finally {
    releaseFirstManifest();
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("activation waits for active browser sessions to close", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  const releaseBrowser = await acquireBrowserSessionLease(runtime);
  let activationReached;
  const reachedActivation = new Promise((resolve) => { activationReached = resolve; });
  const originalActivate = runtime.activateWhenIdle.bind(runtime);
  runtime.activateWhenIdle = async (...args) => {
    activationReached();
    return originalActivate(...args);
  };
  try {
    let completed = false;
    const update = runtime.install({ force: true }).then(() => { completed = true; });
    await reachedActivation;
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(completed, false);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary("old-engine"));
    releaseBrowser();
    await update;
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
    assertNoStagingDirectories(root);
  } finally {
    releaseBrowser();
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("incomplete rollback survives restart and blocks browser launches", async () => {
  const { root, runtime } = createInstalledRuntime();
  const restoreFetch = mockFetch(runtime);
  const releaseBrowser = await acquireBrowserSessionLease(runtime);
  let signalActivation;
  const activationReached = new Promise((resolve) => { signalActivation = resolve; });
  const activateWhenIdle = runtime.activateWhenIdle.bind(runtime);
  runtime.activateWhenIdle = async (...args) => {
    signalActivation();
    return activateWhenIdle(...args);
  };
  runtime.saveManifest = (_manifest, stagingRoot) => {
    rmSync(join(stagingRoot, ".backups", "engine"), { recursive: true, force: true });
    throw new Error("simulated manifest switch failure");
  };
  try {
    const update = runtime.install({ force: true });
    await activationReached;
    const waitingBrowser = acquireBrowserSessionLease(runtime);
    const waitingRejected = assert.rejects(waitingBrowser, /requires recovery/);
    releaseBrowser();
    await assert.rejects(update, /rollback was incomplete/);
    await waitingRejected;
    assert.equal(runtime.installed, true);
    assert.deepEqual(readFileSync(runtime.binaryPath), makeEngineBinary());
    assert.ok(readdirSync(root).some((name) => name.startsWith(".update-")));
    assert.ok(readdirSync(root).includes(".runtime-update-recovery"));
    const restartedRuntime = new Runtime({ cacheDir: root });
    await assert.rejects(restartedRuntime.install(), /unfinished runtime update requires recovery/);
    await assert.rejects(
      acquireBrowserSessionLease(restartedRuntime),
      /unfinished runtime update requires recovery/,
    );
    runtime._checkedInProcess = true;
    await assert.rejects(runtime.install(), /requires recovery/);
  } finally {
    releaseBrowser();
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

test("browser spawn errors reject launch and hold the lease until close", async () => {
  const child = new EventEmitter();
  let released = false;
  const started = awaitBrowserSpawn(child, () => { released = true; });
  const rejected = assert.rejects(started, /spawn denied/);

  child.emit("error", new Error("spawn denied"));
  await rejected;
  assert.equal(released, false);
  child.emit("close", -1, null);
  assert.equal(released, true);
});

test("post-spawn process errors do not release the lease before close", async () => {
  const child = new EventEmitter();
  let released = false;
  const originalWarn = console.warn;
  console.warn = () => {};
  try {
    const started = awaitBrowserSpawn(child, () => { released = true; });
    child.emit("spawn");
    await started;
    child.emit("error", new Error("signal denied"));
    assert.equal(released, false);
    child.emit("close", null, "SIGTERM");
    assert.equal(released, true);
  } finally {
    console.warn = originalWarn;
  }
});
