// #130: a CDP session is stopped with Browser.close, the way the launcher closes a profile.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { closeOverCdp } from "../dist/browser.js";

test("Browser.close goes out as one masked frame after the upgrade", async () => {
  let got = null;
  const server = createServer((sock) => {
    sock.once("data", (hello) => {
      assert.match(hello.toString(), /^GET \/devtools\/browser\/x HTTP\/1\.1\r\n/);
      sock.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n");
      sock.once("data", (f) => {
        const len = f[1] & 0x7f, mask = f.subarray(2, 6);
        got = Buffer.from(f.subarray(6, 6 + len).map((b, i) => b ^ mask[i % 4])).toString();
        sock.end();
      });
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const ok = await closeOverCdp(`ws://127.0.0.1:${server.address().port}/devtools/browser/x`);
  await new Promise((r) => setTimeout(r, 100));
  server.close();
  assert.equal(ok, true);
  assert.deepEqual(JSON.parse(got), { id: 1, method: "Browser.close" });
});

test("nothing listening is a no, not a hang", async () => {
  assert.equal(await closeOverCdp("ws://127.0.0.1:1/devtools/browser/x", 1000), false);
});
