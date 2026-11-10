// #147: a profile reports only an OS version Chrome itself can report.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  randomizePlatformVersion, settlePlatformVersion, MACOS_PLATFORM_VERSIONS, WINDOWS_PLATFORM_VERSIONS,
} from "../dist/randomize.js";

const M1 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";
const M4 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version)";
const M5 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)";

const profile = (platform, version, renderer = "") => ({
  navigator: { platform, platform_version: version },
  client_hints: { platform_version: version },
  webgl: { renderer },
});
const pv = (c) => [c.navigator.platform_version, c.client_hints.platform_version];

test("Windows keeps what Chrome reports and moves the rest", () => {
  for (const [from, to] of [
    ["10.0.0", "10.0.0"], ["14.0.0", "14.0.0"], ["15.0.0", "15.0.0"], ["19.0.0", "19.0.0"],
    ["13.0.0", "19.0.0"], ["16.0.0", "19.0.0"], ["17.0.0", "19.0.0"], ["20.0.0", "19.0.0"],
    ["5.0.0", "10.0.0"], ["12.0.0", "10.0.0"], ["", "19.0.0"], ["x", "19.0.0"],
  ]) {
    const c = profile("Windows", from);
    assert.equal(settlePlatformVersion(c), from !== to, from);
    assert.deepEqual(pv(c), [to, to], from);
  }
});

test("a Mac gets three parts and a version its chip runs", () => {
  for (const [from, gpu, to] of [
    ["15.6.1", M1, "15.6.1"], ["15.4", M1, "15.4.0"], ["26", M1, "26.0.0"],
    ["14.7", M1, "14.7.0"], ["14.7", M4, "15.8.2"], ["15.6.1", M4, "15.6.1"],
    ["15.6.1", M5, "26.7.2"], ["26.1", M5, "26.1.0"], ["10.15.7", M1, "26.7.2"],
    ["", M1, "26.7.2"], ["15.6.1.2", M1, "26.7.2"],
  ]) {
    const c = profile("macOS", from, gpu);
    settlePlatformVersion(c);
    assert.deepEqual(pv(c), [to, to], `${from} on ${gpu}`);
    assert.equal(settlePlatformVersion(c), false, `once: ${from}`);
  }
});

test("client_hints win and navigator is brought along", () => {
  const c = profile("Windows", "15.0.0");
  c.navigator.platform_version = "17.0.0";
  assert.equal(settlePlatformVersion(c), true);
  assert.deepEqual(pv(c), ["15.0.0", "15.0.0"]);
});

test("Linux and phones are left alone", () => {
  assert.equal(settlePlatformVersion(profile("Linux", "6.8.0")), false);
  assert.equal(settlePlatformVersion(profile("Android", "16.0.0")), false);
});

test("every pool value is already settled, and an M5 draws 26 or later", () => {
  for (const v of WINDOWS_PLATFORM_VERSIONS) assert.equal(settlePlatformVersion(profile("Windows", v)), false, v);
  for (const v of MACOS_PLATFORM_VERSIONS) assert.equal(settlePlatformVersion(profile("macOS", v, M1)), false, v);
  for (let i = 0; i < 200; i++) {
    const c = profile("macOS", "", M5);
    randomizePlatformVersion(c);
    assert.ok(Number(pv(c)[0].split(".")[0]) >= 26, pv(c)[0]);
    assert.equal(settlePlatformVersion(c), false);
  }
});
