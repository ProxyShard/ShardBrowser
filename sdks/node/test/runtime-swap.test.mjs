// Engine updates that fail at each step must leave a working engine (#122).
import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, constants as fsConstants, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { hostname, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import AdmZip from "adm-zip";
import {
  InstallLock, MANIFEST_URL, PUB_BASE, Runtime, countFiles, engineHooks, hostSpec, processesUnder, recoverEngine,
  swapEngineTree,
} from "../dist/runtime.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const DIST = join(HERE, "..", "dist", "runtime.js");
const TOP = "ShardX-Test";
const realFetch = globalThis.fetch;
const realWarn = console.warn;
const realHooks = { ...engineHooks };
const roots = [];

afterEach(() => {
  globalThis.fetch = realFetch;
  console.warn = realWarn;
  Object.assign(engineHooks, realHooks);
  for (const r of roots.splice(0)) rmSync(r, { recursive: true, force: true });
});

function tempRoot() {
  const r = mkdtempSync(join(tmpdir(), "shardx-swap-"));
  roots.push(r);
  return r;
}

function tree(dir, version) {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "chrome"), "engine");
  writeFileSync(join(dir, "version.txt"), version);
  writeFileSync(join(dir, `only-in-${version}`), "x");
}

function setup() {
  const root = tempRoot();
  tree(join(root, TOP), "156");
  const staged = join(root, `.${TOP}.staging`, TOP);
  tree(staged, "157");
  return {
    root, staged, live: join(root, TOP),
    previous: join(root, `.${TOP}.previous`), journal: join(root, `.${TOP}.swapping`),
  };
}

const version = (dir) => readFileSync(join(dir, "version.txt"), "utf8");
const real = { rename: renameSync, rm: (p) => rmSync(p, { recursive: true, force: true }) };
const refusing = (bad) => ({
  ...real,
  rename: (a, b) => {
    if (bad.includes(a)) throw Object.assign(new Error("EBUSY: resource busy or locked"), { code: "EBUSY" });
    renameSync(a, b);
  },
});

// ---- the swap itself ----

test("an update replaces the engine whole", () => {
  const t = setup();
  swapEngineTree(t.root, TOP, t.staged);
  assert.equal(version(t.live), "157");
  assert.ok(!existsSync(join(t.live, "only-in-156")), "nothing of the old version lingers");
  assert.ok(!existsSync(t.previous) && !existsSync(t.journal));
});

test("a first rename refused (engine in use) leaves everything", () => {
  const t = setup();
  assert.throws(() => swapEngineTree(t.root, TOP, t.staged, refusing([t.live])), /in use/);
  assert.equal(version(t.live), "156");
  assert.ok(existsSync(t.staged));
  assert.ok(!existsSync(t.journal));
});

test("a second rename that fails puts the engine back", () => {
  const t = setup();
  assert.throws(() => swapEngineTree(t.root, TOP, t.staged, refusing([t.staged])), /EBUSY/);
  assert.equal(version(t.live), "156");
  assert.ok(!existsSync(t.previous) && !existsSync(t.journal));
});

test("a rename refused for a moment is tried again", () => {
  const t = setup();
  let refusals = 0;
  const fs = {
    ...real, tries: 4,
    rename: (a, b) => {
      if (a === t.staged && refusals++ < 2) throw Object.assign(new Error("EBUSY"), { code: "EBUSY" });
      renameSync(a, b);
    },
  };
  swapEngineTree(t.root, TOP, t.staged, fs);
  assert.equal(refusals, 3);
  assert.equal(version(t.live), "157");
});

test("a failed put-back is restored by the next install()", () => {
  const t = setup();
  assert.throws(() => swapEngineTree(t.root, TOP, t.staged, refusing([t.staged, t.previous])), /restored by the next install/);
  assert.ok(!existsSync(t.live));
  assert.ok(existsSync(t.journal), "the journal is what lets it back");
  assert.ok(recoverEngine(t.root, TOP));
  assert.equal(version(t.live), "156");
  assert.ok(!existsSync(t.journal));
});

test("a run killed between the renames comes back to the old engine", () => {
  const t = setup();
  // A real kill: the second rename never returns and nothing after it runs.
  const r = spawnSync(process.execPath, ["--input-type=module", "-e", `
    import { renameSync } from "node:fs";
    import * as rt from ${JSON.stringify(DIST)};
    const staged = ${JSON.stringify(t.staged)};
    rt.swapEngineTree(${JSON.stringify(t.root)}, ${JSON.stringify(TOP)}, staged, {
      rename: (a, b) => { if (a === staged) process.kill(process.pid, "SIGKILL"); renameSync(a, b); },
      rm: () => {},
    });`], { encoding: "utf8" });
  assert.equal(r.signal, "SIGKILL", r.stderr);
  assert.ok(!existsSync(t.live) && existsSync(t.previous) && existsSync(t.journal));

  assert.ok(recoverEngine(t.root, TOP));
  assert.equal(version(t.live), "156");
  assert.ok(!existsSync(t.previous) && !existsSync(t.journal));
  // And the update goes through afterwards.
  swapEngineTree(t.root, TOP, t.staged);
  assert.equal(version(t.live), "157");
});

test("a leftover previous copy is removed, never swapped back in", () => {
  const t = setup();
  tree(t.previous, "half-of-155");
  assert.equal(recoverEngine(t.root, TOP), false);
  assert.ok(!existsSync(t.previous));
  tree(t.previous, "half-of-155");
  assert.throws(() => swapEngineTree(t.root, TOP, t.staged, refusing([t.staged])));
  assert.equal(version(t.live), "156", "the live engine, not the leftover");
});

test("a previous copy a cleanup left half removed is never put back for a missing engine", () => {
  const t = setup();
  // A cleanup cut short after a finished swap: no journal, part of the files.
  tree(t.previous, "155");
  rmSync(join(t.previous, "version.txt"));
  rmSync(t.live, { recursive: true });
  assert.equal(recoverEngine(t.root, TOP), false);
  assert.ok(!existsSync(t.live) && !existsSync(t.previous));
});

test("an unfinished swap whose previous tree lost a file is not trusted", () => {
  const t = setup();
  assert.throws(() => swapEngineTree(t.root, TOP, t.staged, refusing([t.staged, t.previous])));
  rmSync(join(t.previous, "version.txt"));
  assert.equal(recoverEngine(t.root, TOP), false);
  assert.ok(!existsSync(t.live));
});

test("a cleanup Windows refuses does not fail the update", () => {
  const t = setup();
  const locked = { ...real, rm: (p) => { if (p === t.previous) throw new Error("EPERM"); real.rm(p); } };
  swapEngineTree(t.root, TOP, t.staged, locked);
  assert.equal(version(t.live), "157");
  assert.ok(existsSync(t.previous));
  assert.equal(recoverEngine(t.root, TOP), false, "the next install() removes it");
  assert.ok(!existsSync(t.previous));
});

test("a first install has nothing to move aside", () => {
  const t = setup();
  rmSync(t.live, { recursive: true, force: true });
  swapEngineTree(t.root, TOP, t.staged);
  assert.equal(version(t.live), "157");
});

test("processes running from the engine folder are counted", { skip: process.platform === "win32" }, async () => {
  const dir = join(tempRoot(), "engine");
  mkdirSync(dir);
  const bin = join(dir, "browser");
  // macOS kills a copied system binary at exec; a copy of node itself runs.
  const mac = process.platform === "darwin";
  copyFileSync(mac ? process.execPath : "/bin/sleep", bin, fsConstants.COPYFILE_FICLONE);
  chmodSync(bin, 0o755);
  assert.equal(processesUnder(dir), 0);
  const p = spawn(bin, mac ? ["-e", "setTimeout(() => {}, 30000)"] : ["30"]);
  try {
    let n = 0;
    for (let i = 0; i < 40 && n === 0; i++) {
      await new Promise((r) => setTimeout(r, 50));
      n = processesUnder(dir);
    }
    assert.equal(n, 1);
    assert.equal(processesUnder(join(dirname(dir), "elsewhere")), 0);
  } finally {
    p.kill();
  }
});

// ---- the lock between processes ----

const lockPath = (root) => join(root, ".install.lock");
const settle = (p, ms) => Promise.race([p.then(() => "got"), new Promise((r) => setTimeout(() => r("waiting"), ms))]);

test("a lock another live process holds is waited for, then taken", async () => {
  const root = tempRoot();
  console.warn = () => {};
  writeFileSync(lockPath(root), JSON.stringify({ pid: process.pid, host: hostname().toLowerCase(), token: "other", at: Date.now() }));
  const p = InstallLock.acquire(root, { pollMs: 20 });
  assert.equal(await settle(p, 300), "waiting");
  rmSync(lockPath(root));
  const lock = await p;
  assert.ok(lock.held());
  lock.release();
  assert.ok(!existsSync(lockPath(root)));
});

test("the lock of a process that died is taken over", async () => {
  const root = tempRoot();
  const gone = spawnSync(process.execPath, ["-e", "process.stdout.write(String(process.pid))"], { encoding: "utf8" });
  writeFileSync(lockPath(root), JSON.stringify({ pid: Number(gone.stdout), host: hostname().toLowerCase(), token: "dead", at: Date.now() }));
  const lock = await InstallLock.acquire(root, { pollMs: 20 });
  assert.ok(lock.held());
  assert.equal(JSON.parse(readFileSync(lockPath(root), "utf8")).pid, process.pid);
  lock.release();
});

test("an unreadable lock is taken only once it is old", async () => {
  const root = tempRoot();
  console.warn = () => {};
  writeFileSync(lockPath(root), "");
  assert.equal(await settle(InstallLock.acquire(root, { pollMs: 20 }), 200), "waiting");
  const old = (Date.now() - 60_000) / 1000;
  utimesSync(lockPath(root), old, old);
  const lock = await InstallLock.acquire(root, { pollMs: 20 });
  assert.ok(lock.held());
  lock.release();
});

test("a holder whose lock was taken over knows it", async () => {
  const root = tempRoot();
  const lock = await InstallLock.acquire(root);
  writeFileSync(lockPath(root), JSON.stringify({ pid: 1, host: "elsewhere", token: "new", at: Date.now() }));
  assert.equal(lock.held(), false);
  lock.release();
  assert.ok(existsSync(lockPath(root)), "not ours to remove");
});

// ---- install() end to end, the network stood in for ----

const SPEC = hostSpec();
const HTOP = SPEC.binarySubpath[0];
const KEY = SPEC.browser.key;

/** An engine tree for this host at `v`, as archive entries under its folder. */
function engineEntries(v, extra = 0) {
  const e = {
    [SPEC.binarySubpath.join("/")]: `engine ${v}`,
    [`${HTOP}/version.txt`]: v,
    [`${HTOP}/resources.pak`]: "R".repeat(256),
  };
  if (process.platform === "darwin") {
    e[`${HTOP}/ShardX.app/Contents/Frameworks/ShardX Framework.framework/Versions/${v}/ShardX Framework`] = "fw";
  }
  if (process.platform === "win32") e[`${HTOP}/${v}.manifest`] = "<assembly/>";
  for (let i = 0; i < extra; i++) e[`${HTOP}/f/${i}.bin`] = `file ${i}`;
  return e;
}

function zipOf(entries) {
  const z = new AdmZip();
  for (const [name, body] of Object.entries(entries)) {
    z.addFile(name, Buffer.from(body));
    z.getEntry(name).header.method = 0; // stored: its bytes can be found and damaged
  }
  return z.toBuffer();
}

/** A cache with engine `v` installed, its record, and a fingerprint library. */
function installedCache(v) {
  const root = tempRoot();
  for (const [name, body] of Object.entries(engineEntries(v))) {
    const p = join(root, name);
    mkdirSync(dirname(p), { recursive: true });
    writeFileSync(p, body);
  }
  writeFileSync(join(root, HTOP, `only-in-${v}`), "x");
  mkdirSync(join(root, "fingerprints"), { recursive: true });
  writeFileSync(join(root, "fingerprints", "a.json"), "{}");
  writeFileSync(join(root, "manifest.json"), JSON.stringify({
    browser_etag: `e${v}`, widevine_etag: "w1", fingerprints_etag: "f1", installed_chromium_version: v,
  }));
  return root;
}

const remoteFor = (v) => ({ chromium_version: v, archives: { [KEY]: `e${v}`, "ShardX-Fingerprints.zip": "f1" } });

/** Global fetch answering for the manifest and the archives in `files`. */
function serve(manifest, files, calls = []) {
  globalThis.fetch = async (url) => {
    url = String(url);
    calls.push(url);
    if (url === MANIFEST_URL) return new Response(JSON.stringify(manifest));
    const f = files[url.slice(PUB_BASE.length + 1)];
    if (!f) return new Response("no", { status: 404 });
    return new Response(f.body, { headers: { etag: `"${f.etag}"`, "content-length": String(f.body.length) } });
  };
  return calls;
}

function warnings() {
  const seen = [];
  console.warn = (m) => seen.push(String(m));
  return seen;
}

const liveVersion = (root) => readFileSync(join(root, HTOP, "version.txt"), "utf8");
const record = (root) => JSON.parse(readFileSync(join(root, "manifest.json"), "utf8"));
const staging = (root) => join(root, `.${HTOP}.staging`);

test("an update swaps the engine in and records it at once", async () => {
  const root = installedCache("156.0.0.1");
  const calls = serve(remoteFor("157.0.0.1"), { [KEY]: { etag: "e157.0.0.1", body: zipOf(engineEntries("157.0.0.1")) } });
  const seen = warnings();
  const rt = new Runtime({ cacheDir: root });
  await rt.install();
  assert.equal(liveVersion(root), "157.0.0.1");
  assert.ok(!existsSync(join(root, HTOP, "only-in-156.0.0.1")));
  assert.equal(rt.chromiumVersion, "157.0.0.1");
  const m = record(root);
  assert.equal(m.installed_chromium_version, "157.0.0.1");
  assert.equal(m.browser_etag, "e157.0.0.1");
  // The CDM comes from Google at launch, never from the CDN; the old record stays readable.
  assert.ok(!calls.some((u) => /Widevine/i.test(u)), calls.join(", "));
  assert.equal(m.widevine_etag, "w1");
  assert.deepEqual(seen, []);
  assert.ok(!existsSync(staging(root)) && !existsSync(lockPath(root)));
});

for (const [what, body] of [
  ["a damaged archive", () => Buffer.from("PK\x03\x04 cut short")],
  ["an archive without the engine", () => zipOf({ [`${HTOP}/readme.txt`]: "hi" })],
  ["an archive whose unpack fails half way", () => {
    const b = zipOf(engineEntries("157.0.0.1"));
    b[b.indexOf("RRRRRRRR")] = 0x58; // its CRC no longer matches
    return b;
  }],
]) {
  test(`${what} leaves the engine, and the run goes on on it`, async () => {
    const root = installedCache("156.0.0.1");
    serve(remoteFor("157.0.0.1"), { [KEY]: { etag: "e157.0.0.1", body: body() } });
    const seen = warnings();
    const rt = new Runtime({ cacheDir: root });
    await rt.install();
    assert.equal(liveVersion(root), "156.0.0.1");
    assert.equal(rt.chromiumVersion, "156.0.0.1", "profiles keep claiming the engine they run on");
    assert.equal(record(root).installed_chromium_version, "156.0.0.1");
    assert.ok(seen.some((w) => /Staying on the installed engine/.test(w)), seen.join("\n"));
    assert.ok(!existsSync(staging(root)));
    assert.ok(!existsSync(join(root, `.${KEY}.tmp`)));
  });
}

test("with nothing installed a failed install is an error", async () => {
  const root = tempRoot();
  serve(remoteFor("157.0.0.1"), { [KEY]: { etag: "e157.0.0.1", body: Buffer.from("not a zip") } });
  await assert.rejects(new Runtime({ cacheDir: root }).install(), /not a readable zip/);
  assert.ok(!existsSync(join(root, HTOP)) && !existsSync(lockPath(root)));
});

test("not enough room refuses before a byte is written", async () => {
  const root = installedCache("156.0.0.1");
  serve(remoteFor("157.0.0.1"), { [KEY]: { etag: "e157.0.0.1", body: zipOf(engineEntries("157.0.0.1")) } });
  engineHooks.freeBytes = () => 1024;
  const seen = warnings();
  await new Runtime({ cacheDir: root }).install();
  assert.equal(liveVersion(root), "156.0.0.1");
  assert.ok(seen.some((w) => /not enough free disk space/.test(w)), seen.join("\n"));
  assert.ok(!existsSync(join(root, `.${KEY}.tmp`)));
});

test("not enough room to unpack refuses before unpacking", () => {
  const root = installedCache("156.0.0.1");
  const tmp = join(root, `.${KEY}.tmp`);
  writeFileSync(tmp, zipOf(engineEntries("157.0.0.1", 50)));
  // Enough for the margin alone, not for the files.
  engineHooks.freeBytes = () => 256 * 1024 * 1024 + 10;
  const rt = new Runtime({ cacheDir: root });
  assert.throws(() => rt.stageEngine(tmp, "e157.0.0.1"), /not enough free disk space/);
  assert.ok(!existsSync(join(staging(root), HTOP)));
  assert.equal(liveVersion(root), "156.0.0.1");
});

test("browsers running from the engine refuse the swap; the next run swaps without downloading", async () => {
  const root = installedCache("156.0.0.1");
  const body = zipOf(engineEntries("157.0.0.1"));
  const calls = serve(remoteFor("157.0.0.1"), { [KEY]: { etag: "e157.0.0.1", body } });
  engineHooks.processesUnder = () => 2;
  const seen = warnings();
  const busy = new Runtime({ cacheDir: root });
  await busy.install();
  assert.equal(liveVersion(root), "156.0.0.1");
  assert.equal(busy.chromiumVersion, "156.0.0.1");
  assert.ok(seen.some((w) => /2 processes are still running/.test(w)), seen.join("\n"));
  assert.ok(existsSync(join(staging(root), "staged.json")), "the unpacked update is kept");

  engineHooks.processesUnder = () => 0;
  calls.length = 0;
  const next = new Runtime({ cacheDir: root });
  await next.install();
  assert.equal(liveVersion(root), "157.0.0.1");
  assert.ok(!calls.some((u) => u.endsWith(KEY)), "not downloaded again");
  assert.ok(!existsSync(staging(root)));
});

test("a process that checked earlier follows an engine another one swapped in", async () => {
  const root = installedCache("156.0.0.1");
  serve(remoteFor("156.0.0.1"), {});
  warnings();
  const early = new Runtime({ cacheDir: root });
  await early.install();
  assert.equal(early.chromiumVersion, "156.0.0.1");

  serve({ ...remoteFor("157.0.0.1"), grease_brand: "Not.A/Brand", grease_version: "99" },
    { [KEY]: { etag: "e157.0.0.1", body: zipOf(engineEntries("157.0.0.1")) } });
  await new Runtime({ cacheDir: root }).install();
  globalThis.fetch = () => { throw new Error("no network needed"); };
  await early.install();
  assert.equal(early.chromiumVersion, "157.0.0.1");
  assert.equal(early.greaseBrand, "Not.A/Brand");
});

test("two processes updating one cache at once take turns", async () => {
  const root = installedCache("156.0.0.1");
  const zip = join(tempRoot(), "engine.zip");
  const entries = engineEntries("157.0.0.1", 3000);
  writeFileSync(zip, zipOf(entries));
  const run = () => new Promise((resolve, reject) => {
    const p = spawn(process.execPath, [join(HERE, "install-worker.mjs"), root, zip], { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    p.stdout.on("data", (d) => { out += d; });
    p.stderr.on("data", (d) => { out += d; });
    p.on("error", reject);
    p.on("close", (code) => resolve({ code, out }));
  });
  const [a, b] = await Promise.all([run(), run()]);
  assert.equal(a.code, 0, a.out);
  assert.equal(b.code, 0, b.out);
  const results = [a, b].map((r) => JSON.parse(r.out.trim().split("\n").pop()));
  assert.ok(results.every((r) => r.ok && r.version === "157.0.0.1"), JSON.stringify(results));
  assert.equal(results.filter((r) => r.downloaded).length, 1, "the second found it done");
  assert.equal(liveVersion(root), "157.0.0.1");
  assert.equal(countFiles(join(root, HTOP)), Object.keys(entries).length);
  assert.ok(!existsSync(lockPath(root)) && !existsSync(staging(root)));
});
