// End-to-end: create a persistent profile, configure anti-fingerprint noise,
// launch + drive it, then reopen it later. The same fingerprint and browser
// state come back; delete it when you're done.
//
// Run:  node examples/quickstart.mjs
import { ShardX } from "@proxyshard/shardx";

const sdk = new ShardX();

// 1. Create a persistent profile: enriches a random library template (or pass
//    an id, e.g. "win-rtx4060") with randomized params and freezes it under a
//    fresh unique id. Do this ONCE per profile.
const profile = await sdk.createProfile();
console.log("created", profile.id);

// 1a. Anti-fingerprint noise — declarative: exactly these vectors on, the rest
//     off. Vectors: canvas, webgl, audio, client_rects, sensors, fonts.
profile.setNoise("canvas", "audio", "webgl");
sdk.saveProfile(profile); // persist the choice

console.log("saved profiles:", sdk.listSavedProfiles());

// 2. Launch + drive. randomize defaults false so the frozen fingerprint stays
//    stable; cookies/cache persist in the profile's folder.
const { browser, close } = await sdk.session(profile);
try {
  const page = await browser.contexts()[0].newPage();
  await page.goto("https://example.com");
  console.log("title:", await page.title());
} finally {
  await close();
}

// 3. Later (even another process): reopen by id — same fingerprint + state.
const reopened = sdk.openProfile(profile.id);
console.log("reopened", reopened.id);

// 4. Delete when done (wipes config + state).
// sdk.deleteProfile(profile.id);
