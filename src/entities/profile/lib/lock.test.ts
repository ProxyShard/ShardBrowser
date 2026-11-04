// node --test: what a password-protected profile's row and stored file tell the UI (`npm run test:unit`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { isLockedStub, isProtected, isTooShort, isWrongPassword, PASSWORD_MIN, passwordProblem } from "./lock.ts";
import { LOCK_REFUSALS } from "../../../shared/api/lockRefusals.ts";

test("only a row the backend marks protected is protected", () => {
  assert.equal(isProtected({ protected: true }), true);
  assert.equal(isProtected({ protected: false }), false);
  // Rows from before #127, a team's, a sync's: no field, no lock.
  assert.equal(isProtected({}), false);
  assert.equal(isProtected(null), false);
});

test("the stub profile_get answers is told from a full profile", () => {
  const meta = { id: "p1", rev: 3, folder: "", pinned: false, color: null, lock: { v: 1 } };
  assert.equal(isLockedStub({ _meta: meta, name: "Bank" }), true);
  // Unlocked: the whole profile, the lock still in `_meta`.
  assert.equal(isLockedStub({ _meta: meta, name: "Bank", navigator: { user_agent: "x" } }), false);
  assert.equal(isLockedStub({ _meta: { id: "p2", rev: 1 }, name: "Open" }), false);
  assert.equal(isLockedStub(null), false);
});

test("a new password needs the minimum and a matching repeat; none at all is fine", () => {
  assert.equal(passwordProblem("", ""), null);
  assert.equal(passwordProblem("a".repeat(PASSWORD_MIN - 1), "a".repeat(PASSWORD_MIN - 1)), "short");
  assert.equal(passwordProblem("", "something long"), "short");
  assert.equal(passwordProblem("a".repeat(PASSWORD_MIN), "b".repeat(PASSWORD_MIN)), "mismatch");
  assert.equal(passwordProblem("a".repeat(PASSWORD_MIN), "a".repeat(PASSWORD_MIN)), null);
  // Counted in characters, not UTF-16 units: eight emoji are eight.
  assert.equal(passwordProblem("🔑".repeat(PASSWORD_MIN), "🔑".repeat(PASSWORD_MIN)), null);
  assert.equal(passwordProblem("🔑".repeat(PASSWORD_MIN - 1), "🔑".repeat(PASSWORD_MIN - 1)), "short");
});

test("the backend's password refusals are read back, however they arrive", () => {
  assert.equal(isWrongPassword(LOCK_REFUSALS.wrongPassword), true);
  assert.equal(isWrongPassword(new Error(`Error: ${LOCK_REFUSALS.wrongPassword}`)), true);
  assert.equal(isWrongPassword(LOCK_REFUSALS.needsPassword), false);
  assert.equal(isTooShort(LOCK_REFUSALS.tooShort), true);
  assert.equal(isTooShort(LOCK_REFUSALS.wrongPassword), false);
});

const ROOT = fileURLToPath(new URL("../../../../", import.meta.url));
const rust = (path: string) => readFileSync(join(ROOT, "src-tauri/src", path), "utf8");
/** A `pub const NAME: &str = "…";` of a Rust source. */
function rustConst(src: string, name: string): string {
  const m = new RegExp(`pub const ${name}: &str = "([^"]*)";`).exec(src);
  assert.ok(m, `${name} not found`);
  return m[1];
}

test("the password refusals are the backend's own words", () => {
  const protect = rust("space/local/protect.rs");
  assert.equal(LOCK_REFUSALS.needsPassword, rustConst(protect, "PASSWORD_REQUIRED"));
  assert.equal(LOCK_REFUSALS.tooShort, rustConst(protect, "TOO_SHORT"));
  assert.equal(LOCK_REFUSALS.closeFirst, rustConst(protect, "CLOSE_FIRST"));
  assert.equal(LOCK_REFUSALS.notAvailable, rustConst(protect, "NOT_FOR_PROTECTED"));
  assert.equal(LOCK_REFUSALS.wrongPassword, rustConst(rust("space/local/seal.rs"), "WRONG_PASSWORD"));
  assert.equal(LOCK_REFUSALS.syncOn, rustConst(rust("space/mod.rs"), "SYNCED_FIRST"));
});
