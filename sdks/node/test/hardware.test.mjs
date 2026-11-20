// A desktop profile never claims fewer than four logical cores, as in the launcher.
import { test } from "node:test";
import assert from "node:assert/strict";
import { MAC_HW_CONFIGS, X86_CORES, randomizeHardware } from "../dist/randomize.js";

test("the hardware randomizer never picks under four cores for a desktop", () => {
  assert.ok(Math.min(...X86_CORES) >= 4);
  for (const pairs of Object.values(MAC_HW_CONFIGS)) for (const [cores] of pairs) assert.ok(cores >= 4);
  for (let i = 0; i < 200; i++) {
    for (const [platform, id] of [["Windows", "win-x"], ["Linux", "linux-x"], ["macOS", "mac-m1-air13"]]) {
      const cfg = { navigator: { platform, hardware_concurrency: 2 } };
      randomizeHardware(cfg, id);
      assert.ok(cfg.navigator.hardware_concurrency >= 4, `${platform}: ${cfg.navigator.hardware_concurrency}`);
    }
  }
});
