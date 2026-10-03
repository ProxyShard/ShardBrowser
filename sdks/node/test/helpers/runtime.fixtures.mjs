import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

// Plausible native headers with inert test data, never real runnable binaries.
export function makeNativeBinary(name = "synthetic-not-runnable", platform = process.platform, library = false, little = true) {
  const payload = Buffer.from(name);
  if (platform === "win32") {
    const bytes = Buffer.alloc(1024);
    bytes.write("MZ");
    bytes.writeUInt32LE(128, 60);
    bytes.write("PE\0\0", 128);
    bytes.writeUInt16LE(0x8664, 132);
    bytes.writeUInt16LE(1, 134);
    bytes.writeUInt16LE(240, 148);
    bytes.writeUInt16LE(library ? 0x2022 : 0x0022, 150);
    bytes.writeUInt16LE(0x20b, 152);
    bytes.writeUInt32LE(512, 156);
    bytes.writeUInt32LE(0x1000, 168);
    bytes.writeBigUInt64LE(0x140000000n, 176);
    bytes.writeUInt32LE(4096, 184);
    bytes.writeUInt32LE(512, 188);
    bytes.writeUInt32LE(8192, 208);
    bytes.writeUInt32LE(512, 212);
    bytes.writeUInt16LE(2, 220);
    bytes.writeUInt32LE(16, 260);
    bytes.write(".text", 392);
    bytes.writeUInt32LE(512, 400);
    bytes.writeUInt32LE(4096, 404);
    bytes.writeUInt32LE(512, 408);
    bytes.writeUInt32LE(512, 412);
    bytes.writeUInt32LE(0x60000020, 428);
    payload.copy(bytes, 512);
    return bytes;
  }
  if (platform === "linux") {
    const bytes = Buffer.alloc(152 + payload.length);
    bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]);
    bytes.writeUInt16LE(3, 16); // ET_DYN covers PIE executables and shared libraries.
    bytes.writeUInt16LE(62, 18);
    bytes.writeUInt32LE(1, 20);
    bytes.writeBigUInt64LE(library ? 0n : 152n, 24);
    bytes.writeBigUInt64LE(64n, 32);
    bytes.writeUInt16LE(64, 52);
    bytes.writeUInt16LE(56, 54);
    bytes.writeUInt16LE(1, 56);
    bytes.writeUInt32LE(1, 64);
    bytes.writeUInt32LE(5, 68);
    bytes.writeBigUInt64LE(BigInt(bytes.length), 96);
    bytes.writeBigUInt64LE(BigInt(bytes.length), 104);
    bytes.writeBigUInt64LE(4096n, 112);
    payload.copy(bytes, 152);
    return bytes;
  }
  if (platform !== "darwin") throw new Error("unsupported fixture platform");
  const bytes = Buffer.alloc(136 + payload.length);
  const u32 = (value, offset) => little ? bytes.writeUInt32LE(value, offset) : bytes.writeUInt32BE(value, offset);
  const u64 = (value, offset) => little ? bytes.writeBigUInt64LE(value, offset) : bytes.writeBigUInt64BE(value, offset);
  u32(0xfeedfacf, 0);
  u32(0x0100000c, 4);
  u32(library ? 6 : 2, 12);
  u32(1, 16);
  u32(72, 20);
  u32(0x19, 32);
  u32(72, 36);
  bytes.write("__TEXT", 40);
  u64(BigInt(bytes.length), 64);
  u64(BigInt(bytes.length), 80);
  u32(7, 88);
  u32(5, 92);
  payload.copy(bytes, 136);
  return bytes;
}

export function makeUniversalBinary(name, { little = false, fat64 = false, library = false } = {}) {
  const thin = makeNativeBinary(name, "darwin", library);
  const bytes = Buffer.alloc(256 + thin.length);
  const u32 = (value, offset) => little ? bytes.writeUInt32LE(value, offset) : bytes.writeUInt32BE(value, offset);
  const u64 = (value, offset) => little ? bytes.writeBigUInt64LE(value, offset) : bytes.writeBigUInt64BE(value, offset);
  u32(fat64 ? 0xcafebabf : 0xcafebabe, 0);
  u32(1, 4);
  u32(0x0100000c, 8);
  if (fat64) {
    u64(256n, 16);
    u64(BigInt(thin.length), 24);
    u32(8, 32);
  } else {
    u32(256, 16);
    u32(thin.length, 20);
    u32(8, 24);
  }
  thin.copy(bytes, 256);
  return bytes;
}

export function widevineBinaryPath(platform = process.platform) {
  if (platform === "win32") return "_platform_specific/win_x64/widevinecdm.dll";
  if (platform === "linux") return "_platform_specific/linux_x64/libwidevinecdm.so";
  if (platform === "darwin") return "_platform_specific/mac_arm64/libwidevinecdm.dylib";
  throw new Error("unsupported fixture platform");
}

export function widevineEntries(prefix, version = "4.10.0.0", platform = process.platform) {
  return {
    [prefix + "/manifest.json"]: JSON.stringify({ version }),
    [prefix + "/" + widevineBinaryPath(platform)]: makeNativeBinary("test-cdm-" + version, platform, true),
  };
}

export function writeWidevine(dir, version = "4.10.0.0", platform = process.platform) {
  for (const [name, bytes] of Object.entries(widevineEntries(dir, version, platform))) {
    mkdirSync(dirname(name), { recursive: true });
    writeFileSync(name, bytes);
  }
}
