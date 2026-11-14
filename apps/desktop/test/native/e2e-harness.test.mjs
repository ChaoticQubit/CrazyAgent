import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:net";
import { freeLowPort } from "./e2e-harness.mjs";

test("a low free port lies below every port the system hands out and the proxy blocks, takes a listener, and the next call moves on", async () => {
  const first = await freeLowPort();
  const second = await freeLowPort();
  assert.notEqual(first, second);
  for (const port of [first, second]) {
    assert.ok(Number.isInteger(port) && port >= 20000 && port <= 28999, `${port}`);
    const server = createServer();
    await new Promise((done, fail) => {
      server.once("error", fail);
      server.listen(port, "127.0.0.1", done);
    });
    assert.equal(server.address().port, port);
    await new Promise((done) => server.close(done));
  }
});
