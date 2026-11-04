// node --test: the studio's start refusals are worded as the backend words them (`npm run test:unit`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { AUTOMATION_REFUSALS, isOpenWithoutAutomation } from "./automationRefusals.ts";

const ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const rust = (path: string) => readFileSync(join(ROOT, "src-tauri/src", path), "utf8");

function rustConst(src: string, name: string): string {
  const m = new RegExp(`pub const ${name}: &str = "([^"]*)";`).exec(src);
  assert.ok(m, `${name} not found`);
  return m[1];
}

test("the refusals are the backend's constants", () => {
  const lib = rust("lib.rs");
  assert.equal(AUTOMATION_REFUSALS.openWithout, rustConst(lib, "OPEN_WITHOUT_AUTOMATION"));
  assert.equal(AUTOMATION_REFUSALS.didNotClose, rustConst(lib, "DID_NOT_CLOSE"));
  assert.equal(AUTOMATION_REFUSALS.noDebugPort, rustConst(lib, "NO_DEBUG_PORT"));
  assert.equal(AUTOMATION_REFUSALS.drivenByRun, rustConst(lib, "DRIVEN_BY_RUN"));
  assert.ok(rust("space/mod.rs").includes(`"${AUTOMATION_REFUSALS.notAutomated}"`), "cdp_pass's refusal moved");
});

test("open without automation is read back, however it arrives", () => {
  assert.equal(isOpenWithoutAutomation(AUTOMATION_REFUSALS.openWithout), true);
  assert.equal(isOpenWithoutAutomation(new Error(`Error: ${AUTOMATION_REFUSALS.openWithout}`)), true);
  assert.equal(isOpenWithoutAutomation(AUTOMATION_REFUSALS.notAutomated), false);
  assert.equal(isOpenWithoutAutomation(undefined), false);
});
