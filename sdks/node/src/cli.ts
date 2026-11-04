#!/usr/bin/env node
// `shardx` command line: fetch the engine ahead of the first launch (#132).
//   shardx install [--force] [--cache-dir DIR]
// Useful in a Docker image, where the first launch should not be the one that
// downloads several hundred megabytes.
import { Runtime } from "./runtime.js";

const USAGE = "usage: shardx install [--force] [--cache-dir DIR]";

async function main(argv: string[]): Promise<number> {
  const [command, ...rest] = argv;
  if (command !== "install") {
    console.error(USAGE);
    return 2;
  }
  let force = false;
  let cacheDir: string | undefined;
  for (let i = 0; i < rest.length; i++) {
    if (rest[i] === "--force") force = true;
    else if (rest[i] === "--cache-dir" && rest[i + 1]) cacheDir = rest[++i];
    else {
      console.error(USAGE);
      return 2;
    }
  }
  // Every tenth percent, not every chunk: a build log stays readable.
  const shown = new Map<string, number>();
  const runtime = new Runtime({
    cacheDir,
    progress: (label, received, total) => {
      if (total <= 0) return;
      const pct = Math.floor((received * 100) / total);
      if (Math.floor(pct / 10) !== shown.get(label)) {
        shown.set(label, Math.floor(pct / 10));
        console.error(`${label}: ${pct}%`);
      }
    },
  });
  await runtime.install({ force });
  console.log(`engine ${runtime.chromiumVersion} at ${runtime.binaryPath}`);
  return 0;
}

main(process.argv.slice(2)).then(
  (code) => process.exit(code),
  (e) => {
    console.error(e instanceof Error ? e.message : String(e));
    process.exit(1);
  },
);
