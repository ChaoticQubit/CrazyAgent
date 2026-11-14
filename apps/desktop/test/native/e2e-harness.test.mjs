import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { connect, createServer } from "node:net";
import { closedPort, freeLowPort, refusingPort } from "./e2e-harness.mjs";

/** The processes lsof names for a local TCP port in one state, either end of a connection
 * included. Input: the port and the TCP state. Output: the distinct pids. */
function holders(port, state) {
  const result = spawnSync("/usr/sbin/lsof", ["-nP", `-iTCP:${port}`, `-sTCP:${state}`, "-t"], { encoding: "utf8", timeout: 10000, killSignal: "SIGKILL" });
  assert.equal(result.signal, null, "lsof did not finish");
  return [...new Set(result.stdout.split("\n").filter(Boolean).map(Number))];
}

test("a closed port refuses a connect, nothing listens there, and this process keeps holding it", async () => {
  const port = await closedPort();
  const refused = await new Promise((done) => {
    const socket = connect(port, "127.0.0.1");
    socket.once("connect", () => {
      socket.destroy();
      done("connected");
    });
    socket.once("error", (error) => done(error.code));
  });
  assert.equal(refused, "ECONNREFUSED");
  assert.deepEqual(holders(port, "LISTEN"), []);
  assert.deepEqual(holders(port, "ESTABLISHED"), [process.pid], `port ${port} is not held`);
});

test("a refusing port stays held with no listener until it is freed, and then nothing holds it", async () => {
  const { port, free } = await refusingPort();
  assert.deepEqual(holders(port, "LISTEN"), []);
  assert.deepEqual(holders(port, "ESTABLISHED"), [process.pid], `port ${port} is not held`);
  free();
  await new Promise((done) => setTimeout(done, 200));
  assert.deepEqual(holders(port, "ESTABLISHED"), []);
});

test("a process that took a closed port still ends on its own", () => {
  const harness = new URL("./e2e-harness.mjs", import.meta.url).href;
  const script = `const { closedPort } = await import(${JSON.stringify(harness)}); await closedPort();`;
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", script], { encoding: "utf8", timeout: 20000, killSignal: "SIGKILL" });
  assert.equal(result.signal, null, "the process still ran at the limit");
  assert.equal(result.status, 0, result.stderr);
});

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
