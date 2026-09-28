import assert from "node:assert/strict";
import { createRequire } from "node:module";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import AdmZip from "adm-zip";

const require = createRequire(import.meta.url);
const admZipVersion = require("adm-zip/package.json").version;

function assertPatchedAdmZipVersion() {
  const [major, minor, patch] = admZipVersion.split(".").map(Number);
  const isPatched =
    major > 0 || (major === 0 && (minor > 6 || (minor === 6 && patch >= 1)));

  assert.ok(
    isPatched,
    `Refusing to run the oversized ZIP fixture with adm-zip ${admZipVersion}`,
  );
}

test("extracts a stored entry with an oversized declared size safely", () => {
  assertPatchedAdmZipVersion();

  const payload = Buffer.from("small payload");
  const archive = new AdmZip();
  archive.addFile("payload.txt", payload);
  const entry = archive.getEntry("payload.txt");
  assert.ok(entry);
  entry.header.method = 0;

  const bytes = archive.toBuffer();
  const centralDirectoryOffset = bytes.lastIndexOf(
    Buffer.from([0x50, 0x4b, 0x01, 0x02]),
  );
  assert.notEqual(centralDirectoryOffset, -1);
  assert.equal(bytes.readUInt16LE(centralDirectoryOffset + 10), 0);
  bytes.writeUInt32LE(0x60000000, centralDirectoryOffset + 24);

  const destination = mkdtempSync(join(tmpdir(), "adm-zip-size-test-"));
  try {
    new AdmZip(bytes).extractAllTo(destination, true);
    assert.deepEqual(readFileSync(join(destination, "payload.txt")), payload);
  } finally {
    rmSync(destination, { recursive: true, force: true });
  }
});

test("rejects extraction through a pre-existing symlink", (context) => {
  assertPatchedAdmZipVersion();

  const sandbox = mkdtempSync(join(tmpdir(), "adm-zip-symlink-test-"));
  try {
    const destination = join(sandbox, "destination");
    const outside = join(sandbox, "outside");
    const target = join(outside, "payload.txt");
    mkdirSync(destination);
    mkdirSync(outside);

    try {
      symlinkSync(
        outside,
        join(destination, "redirect"),
        process.platform === "win32" ? "junction" : "dir",
      );
    } catch (error) {
      if (
        error &&
        typeof error === "object" &&
        "code" in error &&
        ["EACCES", "EPERM", "ENOTSUP"].includes(String(error.code))
      ) {
        context.skip(`Directory symlinks are unavailable (${String(error.code)})`);
        return;
      }
      throw error;
    }

    const archive = new AdmZip();
    archive.addFile("redirect/payload.txt", Buffer.from("should not escape"));

    assert.throws(() => archive.extractAllTo(destination, true));
    assert.equal(existsSync(target), false);
  } finally {
    rmSync(sandbox, { recursive: true, force: true });
  }
});
