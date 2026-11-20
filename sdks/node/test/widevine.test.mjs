// The Widevine CDM from Google's update server, checked as Chrome checks it and put into each profile.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { createServer as httpServer } from "node:http";
import { createServer as netServer, connect as netConnect } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import AdmZip from "adm-zip";
import {
  WIDEVINE_ID, cdmVersionOf, ensureWidevine, fetchWidevine, harvestWidevine, idFromHash, idFromKey,
  installWidevine, parseOffer, requestBody, verifyCrx,
} from "../dist/widevine.js";

const tmp = (p) => mkdtempSync(join(tmpdir(), `shardx-wv-${p}-`));

test("the request names this platform, its release, and the engine's version", () => {
  const r = requestBody("158.0.8127.36", undefined, false).request;
  assert.equal(r.apps[0].appid, WIDEVINE_ID);
  assert.equal(r.apps[0].version, "0.0.0.0");
  assert.equal(r.apps[0].installsource, "ondemand");
  assert.equal(r.protocol, "4.0");
  assert.equal(r.prodversion, "158.0.8127.36");
  assert.equal(r.updaterversion, "158.0.8127.36");
  assert.ok(["mac", "win", "linux"].includes(r["@os"]));
  assert.match(r.os.version, /^\d+(\.\d+)+$/);
  assert.ok(r.os.platform && r.os.arch && r.arch);
  assert.match(r.requestid, /^\{[0-9a-f-]{36}\}$/);
  const bg = requestBody("158", "4.10.3112.0", true).request;
  assert.equal(bg.apps[0].version, "4.10.3112.0");
  assert.equal(bg.apps[0].installsource, "");
});

test("an offer is read, noupdate is null, an error throws", () => {
  const ok = `)]}'
{"response":{"protocol":"4.0","apps":[{"appid":"oimompecagnajdejgnnjijobebaeigek","status":"ok","updatecheck":{"nextversion":"4.10.3112.0","status":"ok","pipelines":[{"pipeline_id":"1:full","operations":[{"size":20423379,"type":"download","out":{"sha256":"6bf3"},"urls":[{"url":"http://edgedl.me.gvt1.com/a.crx3"},{"url":"https://edgedl.me.gvt1.com/a.crx3"}]},{"type":"crx3"}]}]}}]}}`;
  assert.deepEqual(parseOffer(ok), { version: "4.10.3112.0", urls: ["https://edgedl.me.gvt1.com/a.crx3"], sha256: "6bf3", size: 20423379 });
  const none = `)]}'
{"response":{"apps":[{"appid":"x","status":"ok","updatecheck":{"status":"noupdate"}}]}}`;
  assert.equal(parseOffer(none), null);
  assert.throws(() => parseOffer(`{"response":{"apps":[{"status":"error-unknownApplication"}]}}`), /error-unknownApplication/);
  assert.throws(() => parseOffer("<html>"), /not JSON/);
});

// ---- CRX3 ----

function varint(n) {
  const out = [];
  while (n > 127) { out.push((n & 127) | 128); n = Math.floor(n / 128); }
  out.push(n);
  return Buffer.from(out);
}
const field = (num, bytes) => Buffer.concat([varint(num * 8 + 2), varint(bytes.length), bytes]);
const spkiOf = (k) => k.publicKey.export({ type: "spki", format: "der" });

/** A CRX3 of `archive` signed for `idKey`'s id by each of `signers`. */
function crx3(archive, signers, idKey) {
  const signed = field(1, createHash("sha256").update(spkiOf(idKey)).digest().subarray(0, 16));
  const len = Buffer.alloc(4);
  len.writeUInt32LE(signed.length);
  const msg = Buffer.concat([Buffer.from("CRX3 SignedData\x00", "latin1"), len, signed, archive]);
  const proofs = signers.map((k) => field(k.publicKey.asymmetricKeyType === "rsa" ? 2 : 3,
    Buffer.concat([field(1, spkiOf(k)), field(2, sign("sha256", msg, k.privateKey))])));
  const header = Buffer.concat([...proofs, field(10000, signed)]);
  const size = Buffer.alloc(4);
  size.writeUInt32LE(header.length);
  return Buffer.concat([Buffer.from("Cr24"), Buffer.from([3, 0, 0, 0]), size, header, archive]);
}

function zipOf(files) {
  const z = new AdmZip();
  for (const [name, body] of Object.entries(files)) z.addFile(name, Buffer.from(body));
  return z.toBuffer();
}

const rsa = generateKeyPairSync("rsa", { modulusLength: 2048 });
const p256 = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
const p384 = generateKeyPairSync("ec", { namedCurve: "secp384r1" });
const archive = zipOf({ "manifest.json": '{"version":"4.10.3112.0"}' });

test("the id is the key's hash, first 16 bytes as a..p", () => {
  assert.equal(idFromHash(Buffer.from("0123456789abcdeffedcba9876543210", "hex")), "abcdefghijklmnopponmlkjihgfedcba");
  assert.equal(idFromKey(spkiOf(rsa)), idFromHash(createHash("sha256").update(spkiOf(rsa)).digest()));
});

test("a package signed by its developer passes; one signed for another id does not", () => {
  const pkg = crx3(archive, [rsa, p256, p384], rsa);
  const v = verifyCrx(pkg);
  assert.equal(v.crxId, idFromKey(spkiOf(rsa)));
  assert.equal(pkg.subarray(v.archiveOffset).toString("latin1"), archive.toString("latin1"));
  assert.throws(() => verifyCrx(pkg, { id: WIDEVINE_ID }), (e) => e.code === "WrongId");
  // An ECDSA developer key works the same.
  assert.equal(verifyCrx(crx3(archive, [p256], p256)).crxId, idFromKey(spkiOf(p256)));
});

test("a changed byte, a wrong hash or size, or no developer proof is refused", () => {
  const pkg = crx3(archive, [rsa, p256], rsa);
  const bent = Buffer.from(pkg);
  bent[bent.length - 3] ^= 1;
  assert.throws(() => verifyCrx(bent), (e) => e.code === "BadSignature");
  const sha = createHash("sha256").update(pkg).digest("hex");
  assert.equal(verifyCrx(pkg, { sha256: sha.toUpperCase(), size: pkg.length }).sha256, sha);
  assert.throws(() => verifyCrx(pkg, { sha256: "00".repeat(32) }), (e) => e.code === "WrongHash");
  assert.throws(() => verifyCrx(pkg, { size: pkg.length + 1 }), (e) => e.code === "WrongSize");
  // Signed for p384's id, but p384 gave no proof.
  assert.throws(() => verifyCrx(crx3(archive, [rsa], p384)), (e) => e.code === "NoDeveloperProof");
  assert.throws(() => verifyCrx(Buffer.from("PK\x03\x04 not a crx")), (e) => e.code === "NotCrx3");
});

// ---- the cache and the profile ----

function fakeCdm(dir, version) {
  mkdirSync(join(dir, "_platform_specific", "mac_arm64"), { recursive: true });
  writeFileSync(join(dir, "manifest.json"), JSON.stringify({ name: "WidevineCdm", version }));
  writeFileSync(join(dir, "_platform_specific", "mac_arm64", "libwidevinecdm.dylib"), "cdm");
}

test("a profile gets <udd>/WidevineCdm/<version>/ and the hint, once", () => {
  const root = tmp("layout");
  const cache = join(root, "widevine-cdm");
  const udd = join(root, "profiles", "p1");
  mkdirSync(udd, { recursive: true });
  assert.equal(installWidevine(cache, udd), undefined, "nothing cached yet");
  fakeCdm(cache, "4.10.3112.0");
  // A flat layout an older build left.
  mkdirSync(join(udd, "WidevineCdm", "_platform_specific"), { recursive: true });
  writeFileSync(join(udd, "WidevineCdm", "manifest.json"), '{"version":"4.10.1.0"}');

  const versioned = installWidevine(cache, udd);
  assert.equal(versioned, join(udd, "WidevineCdm", "4.10.3112.0"));
  assert.equal(cdmVersionOf(versioned), "4.10.3112.0");
  assert.ok(existsSync(join(versioned, "_platform_specific", "mac_arm64", "libwidevinecdm.dylib")));
  assert.ok(!existsSync(join(udd, "WidevineCdm", "manifest.json")), "the flat layout is gone");
  const hint = join(udd, "WidevineCdm", "latest-component-updated-widevine-cdm");
  assert.deepEqual(JSON.parse(readFileSync(hint, "utf8")), { Path: resolve(versioned) });
  assert.deepEqual(readdirSync(join(udd, "WidevineCdm")).sort(), ["4.10.3112.0", "latest-component-updated-widevine-cdm"]);
  assert.equal(installWidevine(cache, udd), versioned);
  rmSync(root, { recursive: true, force: true });
});

test("a newer CDM the engine fetched itself is taken into the cache; an older one is not", () => {
  const root = tmp("harvest");
  const cache = join(root, "widevine-cdm");
  const udd = join(root, "p");
  fakeCdm(cache, "4.10.3112.0");
  fakeCdm(join(udd, "WidevineCdm", "4.10.2891.0"), "4.10.2891.0");
  assert.equal(harvestWidevine(cache, udd), undefined);
  fakeCdm(join(udd, "WidevineCdm", "4.11.0.0"), "4.11.0.0");
  assert.equal(harvestWidevine(cache, udd), "4.11.0.0");
  assert.equal(cdmVersionOf(cache), "4.11.0.0");
  assert.ok(!readdirSync(root).some((n) => n.includes("incoming")));
  rmSync(root, { recursive: true, force: true });
});

test("a first CDM is waited for; a fresh one is not checked; starts at once check once", async () => {
  const root = tmp("ensure");
  const cache = join(root, "widevine-cdm");
  const calls = [];
  const fetcher = async (proxy, chrome, have, dir, background) => {
    calls.push({ have, background });
    await new Promise((r) => setTimeout(r, 20));
    fakeCdm(dir, "4.10.3112.0");
    return "4.10.3112.0";
  };
  await Promise.all([ensureWidevine(cache, "158.0.8127.36", null, { fetcher }), ensureWidevine(cache, "158.0.8127.36", null, { fetcher })]);
  assert.deepEqual(calls, [{ have: undefined, background: false }]);
  assert.equal(cdmVersionOf(cache), "4.10.3112.0");
  assert.ok(existsSync(`${cache}.checked`));
  await ensureWidevine(cache, "158.0.8127.36", null, { fetcher });
  assert.equal(calls.length, 1, "checked within 5 hours");
  // An old stamp: checked again, in the background.
  const old = new Date(Date.now() - 6 * 3600 * 1000);
  utimesSync(`${cache}.checked`, old, old);
  await ensureWidevine(cache, "158.0.8127.36", null, { fetcher: async (...a) => { calls.push({ have: a[2], background: a[4] }); return null; } });
  await new Promise((r) => setTimeout(r, 50));
  assert.deepEqual(calls[1], { have: "4.10.3112.0", background: true });
  rmSync(root, { recursive: true, force: true });
});

test("a first check that takes too long is given up and the start goes on", async () => {
  const root = tmp("slow");
  const cache = join(root, "widevine-cdm");
  let aborted = false;
  const fetcher = (proxy, chrome, have, dir, background, signal) => new Promise((_, fail) => {
    signal.addEventListener("abort", () => { aborted = true; fail(new Error("cancelled")); });
  });
  const warn = console.warn;
  console.warn = () => {};
  try { await ensureWidevine(cache, "158.0.8127.36", null, { fetcher, firstWaitMs: 30 }); } finally { console.warn = warn; }
  assert.ok(aborted);
  assert.ok(!existsSync(`${cache}.checked`));
  rmSync(root, { recursive: true, force: true });
});

// ---- the wire: straight, through SOCKS5, through HTTP CONNECT; and not holding the process ----

const NOUPDATE = `)]}'\n{"response":{"apps":[{"appid":"${WIDEVINE_ID}","status":"ok","updatecheck":{"status":"noupdate"}}]}}`;

async function listen(srv) {
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  return srv.address().port;
}

/** Exactly `n` bytes from a socket, in pull mode. */
async function readN(s, n) {
  for (;;) {
    const b = s.read(n);
    if (b) return b;
    await once(s, "readable");
  }
}

/** A SOCKS5 proxy wanting `login` ("user:pass") that notes what it is asked for. */
function socksProxy(login, seen) {
  return netServer(async (c) => {
    c.on("error", () => {});
    try {
      const [, n] = await readN(c, 2);
      await readN(c, n);
      c.write(Buffer.from([5, 2]));
      const [, ul] = await readN(c, 2);
      const u = (await readN(c, ul)).toString();
      const [pl] = await readN(c, 1);
      const pw = (await readN(c, pl)).toString();
      seen.push(`login ${u}:${pw}`);
      c.write(Buffer.from([1, `${u}:${pw}` === login ? 0 : 1]));
      const [, , , atyp] = await readN(c, 4);
      const host = atyp === 1 ? [...await readN(c, 4)].join(".") : (await readN(c, (await readN(c, 1))[0])).toString();
      const port = (await readN(c, 2)).readUInt16BE(0);
      seen.push(`to ${host}:${port}`);
      const up = netConnect(port, host, () => { c.write(Buffer.from([5, 0, 0, 1, 0, 0, 0, 0, 0, 0])); up.pipe(c); c.pipe(up); });
      up.on("error", () => c.destroy());
    } catch { c.destroy(); }
  });
}

/** An HTTP proxy answering CONNECT, noting each request head. */
function connectProxy(seen) {
  return netServer(async (c) => {
    c.on("error", () => {});
    let head = "";
    while (!head.endsWith("\r\n\r\n")) head += (await readN(c, 1)).toString("latin1");
    seen.push(head);
    const [host, port] = /^CONNECT (\S+) /.exec(head)[1].split(":");
    const up = netConnect(Number(port), host, () => { c.write("HTTP/1.1 200 Connection established\r\n\r\n"); up.pipe(c); c.pipe(up); });
    up.on("error", () => c.destroy());
  });
}

test("the check goes out straight, through SOCKS5 and through HTTP CONNECT, with its login", async () => {
  const asked = [];
  const update = httpServer((req, res) => {
    let body = "";
    req.on("data", (d) => { body += d; });
    req.on("end", () => { asked.push({ url: req.url, headers: req.headers, body: JSON.parse(body) }); res.end(NOUPDATE); });
  });
  const viaSocks = [], viaConnect = [];
  const socks = socksProxy("u ser:p@ss", viaSocks);
  const http = connectProxy(viaConnect);
  const [up, sp, hp] = [await listen(update), await listen(socks), await listen(http)];
  const url = `http://127.0.0.1:${up}/service/update2/json`;
  const root = tmp("wire");
  try {
    for (const proxy of [null,
      { scheme: "socks5", host: "127.0.0.1", port: sp, username: "u ser", password: "p@ss" },
      { scheme: "http", host: "127.0.0.1", port: hp, username: "u", password: "p" }]) {
      assert.equal(await fetchWidevine(proxy, "158.0.8127.36", "4.10.3112.0", join(root, "c"), true, undefined, url), null);
    }
    assert.equal(asked.length, 3);
    for (const a of asked) {
      assert.equal(a.url, "/service/update2/json");
      assert.equal(a.headers["x-goog-update-appid"], WIDEVINE_ID);
      assert.equal(a.headers["x-goog-update-interactivity"], "bg");
      assert.equal(a.headers["x-goog-update-updater"], "chrome-158.0.8127.36");
      assert.equal(a.body.request.protocol, "4.0");
      assert.equal(a.body.request.apps[0].version, "4.10.3112.0");
    }
    assert.deepEqual(viaSocks, ["login u ser:p@ss", `to 127.0.0.1:${up}`]);
    assert.match(viaConnect[0], new RegExp(`^CONNECT 127\\.0\\.0\\.1:${up} HTTP/1\\.1\r\n`));
    assert.match(viaConnect[0], new RegExp(`Proxy-Authorization: Basic ${Buffer.from("u:p").toString("base64")}`));
    // A wrong login is an error, not a hang.
    await assert.rejects(fetchWidevine({ scheme: "socks5", host: "127.0.0.1", port: sp, username: "x", password: "y" },
      "158.0.8127.36", undefined, join(root, "c"), true, undefined, url), /refused the login/);
  } finally {
    for (const s of [update, socks, http]) s.close();
    rmSync(root, { recursive: true, force: true });
  }
});

const WIDEVINE_JS = pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "dist", "widevine.js")).href;
const CHILD = `
const { ensureWidevine } = await import(process.env.T_MODULE);
await ensureWidevine(process.env.T_CACHE, "158.0.8127.36", null,
  { updateUrl: process.env.T_URL, firstWaitMs: Number(process.env.T_WAIT || 45000) });
console.log("returned");
setTimeout(() => {}, 300); // the script's own work: time for the check to reach the server
`;

test("a background check in flight does not keep a finished script alive; a first one waits", async () => {
  const heard = [], held = [];
  // Takes the connection and never answers.
  const mute = netServer((s) => { held.push(s); s.on("data", (d) => heard.push(d.toString("latin1"))); s.on("error", () => {}); });
  const url = `http://127.0.0.1:${await listen(mute)}/service/update2/json`;
  const run = (env) => new Promise((done) => {
    const started = Date.now();
    const p = spawn(process.execPath, ["--input-type=module", "-e", CHILD], {
      env: { ...process.env, T_MODULE: WIDEVINE_JS, T_URL: url, ...env }, stdio: ["ignore", "pipe", "ignore"] });
    let out = "";
    p.stdout.on("data", (d) => { out += d; });
    const kill = setTimeout(() => p.kill("SIGKILL"), 20_000);
    p.on("exit", (code) => { clearTimeout(kill); done({ ms: Date.now() - started, code, out }); });
  });
  const root = tmp("unref");
  try {
    // A cached CDM with no recent check: checked in the background.
    fakeCdm(join(root, "widevine-cdm"), "4.10.3112.0");
    const bg = await run({ T_CACHE: join(root, "widevine-cdm") });
    assert.equal(bg.code, 0);
    assert.match(bg.out, /returned/);
    assert.ok(heard.some((d) => d.startsWith("POST /service/update2/json")), "the check was in flight");
    assert.ok(bg.ms < 3000, `the script took ${bg.ms} ms to exit`);
    // No CDM yet: the start waits for it, here until the 1.5 s wait gives up.
    const first = await run({ T_CACHE: join(root, "none", "widevine-cdm"), T_WAIT: "1500" });
    assert.equal(first.code, 0);
    assert.match(first.out, /returned/);
    assert.ok(first.ms >= 1400, `the first check held the start only ${first.ms} ms`);
  } finally {
    for (const s of held) s.destroy();
    mute.close();
    rmSync(root, { recursive: true, force: true });
  }
});

// By hand: SHARDX_LIVE_WIDEVINE=1 npm test
test("live: Google's current CDM, signed and in place", { skip: !process.env.SHARDX_LIVE_WIDEVINE }, async () => {
  const root = tmp("live");
  const cache = join(root, "widevine-cdm");
  const v = await fetchWidevine(null, "158.0.8127.36", undefined, cache, false);
  assert.ok(v, "an offer for a profile with none");
  assert.equal(cdmVersionOf(cache), v);
  const walk = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) => e.isDirectory() ? walk(join(d, e.name)) : [join(d, e.name)]);
  const libs = walk(join(cache, "_platform_specific"));
  console.log(`widevine ${v}: ${libs.map((p) => p.slice(cache.length + 1)).join(", ")}`);
  assert.ok(libs.some((p) => p.includes("widevinecdm")));
  // A background check holds nothing: the test keeps the process alive meanwhile.
  const alive = setInterval(() => {}, 1000);
  try {
    assert.equal(await fetchWidevine(null, "158.0.8127.36", v, cache, true), null, "the same version again is current");
  } finally {
    clearInterval(alive);
  }
  rmSync(root, { recursive: true, force: true });
});
