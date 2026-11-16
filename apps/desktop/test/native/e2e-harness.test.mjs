import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { closedPort, flushed, freeLowPort, refusingPort, start, stop } from "./e2e-harness.mjs";

/** The processes lsof names for a local TCP port in one state, either end of a connection
 * included. Input: the port and the TCP state. Output: the distinct pids. */
function holders(port, state) {
  const result = spawnSync("/usr/sbin/lsof", ["-nP", `-iTCP:${port}`, `-sTCP:${state}`, "-t"], { encoding: "utf8", timeout: 10000, killSignal: "SIGKILL" });
  assert.equal(result.signal, null, "lsof did not finish");
  return [...new Set(result.stdout.split("\n").filter(Boolean).map(Number))];
}

test("a low free port skips a port another listener of this user holds on a wildcard address or ::1", async () => {
  for (const host of ["0.0.0.0", "::", "::1"]) {
    const before = await freeLowPort();
    const taken = before === 28999 ? 20000 : before + 1;
    const holder = createServer();
    await new Promise((done, fail) => {
      holder.once("error", fail);
      holder.listen(taken, host, done);
    });
    const after = await freeLowPort();
    await new Promise((done) => holder.close(done));
    assert.notEqual(after, taken, `${host} port ${taken} is held`);
  }
});

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

test("once a child is stopped and its output files are flushed, the lines it wrote as it stopped are in its .err and .out files", async () => {
  const evidence = mkdtempSync(join(tmpdir(), "ca-flush-"));
  try {
    const script = "process.stdout.write('ready\\n'); process.on('SIGTERM', () => { process.stderr.write('marker-err\\n'); process.stdout.write('marker-out\\n'); process.exit(0); }); setInterval(() => {}, 1000);";
    const { child } = await start(process.execPath, ["-e", script], { PATH: process.env.PATH }, evidence, "marker", /^ready$/m);
    await stop(child);
    assert.equal(await flushed(child), true, "both output files closed in time");
    assert.match(readFileSync(join(evidence, "marker.err"), "utf8"), /marker-err/u);
    assert.match(readFileSync(join(evidence, "marker.out"), "utf8"), /marker-out/u);
    assert.equal(await flushed(null), true, "a child the harness never started has nothing to wait for");
  } finally {
    rmSync(evidence, { recursive: true, force: true });
  }
});
