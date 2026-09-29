import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import AdmZip from "adm-zip";
import { awaitBrowserSpawn } from "../dist/browser.js";
import { acquireBrowserSessionLease, MANIFEST_URL, PUB_BASE, Runtime } from "../dist/runtime.js";

function makeZip(entries) {
  const archive = new AdmZip();
  for (const [name, contents] of Object.entries(entries)) {
    archive.addFile(name, Buffer.from(contents));
  }
  return archive.toBuffer();
}

function makeEngineBinary(name = "new-engine") {
  const signature = process.platform === "win32"
    ? [0x4d, 0x5a, 0, 0]
    : process.platform === "darwin"
      ? [0xcf, 0xfa, 0xed, 0xfe]
      : [0x7f, 0x45, 0x4c, 0x46];
  return Buffer.concat([Buffer.from(signature), Buffer.from(name)]);
}

function createInstalledRuntime() {
  const root = mkdtempSync(join(tmpdir(), "shardx-runtime-update-"));
  const runtime = new Runtime({ cacheDir: root });
  mkdirSync(dirname(runtime.binaryPath), { recursive: true });
  writeFileSync(runtime.binaryPath, makeEngineBinary("old-engine"));
  if (runtime.spec.widevine) {
    const widevineDir = join(root, ...runtime.spec.widevineSubpath);
    mkdirSync(widevineDir, { recursive: true });
    const widevine = join(widevineDir, "manifest.json");
    writeFileSync(widevine, "old-widevine");
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
    responses.set(runtime.spec.widevine.key, makeZip({
      [`${wrapper}/WidevineCdm/manifest.json`]: "new-widevine",
    }));
  }
  responses.set("ShardX-Fingerprints.zip", makeZip({
    "shardx-fingerprints/template.json": '{"version":"new"}',
  }));
  return responses;
}

function mockFetch(runtime, options = {}) {
  const originalFetch = globalThis.fetch;
  const archives = createArchiveResponses(runtime);
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
        readFileSync(join(root, ...runtime.spec.widevineSubpath, "manifest.json"), "utf8"),
        "new-widevine",
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
    await runtime.install();
    assertNoStagingDirectories(root);
  } finally {
    restoreFetch();
    rmSync(root, { recursive: true, force: true });
  }
});

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
