import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { RELAUNCHED, appBundle, command, listener, stopRelaunched } from "./trial-app.mjs";
import { relaunch } from "./trial-kit.mjs";

/** Starts a real process that sleeps until it is stopped. Output: the child and a promise of
 * its exit signal. */
function sleeper() {
  const child = spawn("/bin/sleep", ["600"], { stdio: "ignore" });
  const ended = new Promise((resolve) => child.once("exit", (code, signal) => resolve(signal ?? code)));
  return { child, ended };
}

/** A new run folder. Output: its real path. */
function folder() {
  return realpathSync(mkdtempSync(join(tmpdir(), "ca-trial-app-")));
}

test("appBundle finds the .app bundle open -a starts and refuses a bare binary", () => {
  assert.equal(appBundle("/Applications/CrazyAgent.app"), "/Applications/CrazyAgent.app");
  assert.equal(appBundle("/Applications/CrazyAgent.app/Contents/MacOS/crazyagent-desktop"), "/Applications/CrazyAgent.app");
  assert.throws(() => appBundle("/repo/target/debug/crazyagent-desktop"), /relaunch needs the app as a \.app bundle/);
  assert.throws(() => appBundle("CrazyAgent.app"), /absolute/);
});

test("listener names the process that listens on a local port, and none once it closes", async () => {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  assert.equal(listener(port), process.pid);
  await new Promise((resolve) => server.close(resolve));
  assert.equal(listener(port), null);
  assert.throws(() => listener(0), /a TCP port/);
});

test("stopRelaunched quits the recorded app only while that pid still runs the recorded binary", async (t) => {
  const evidence = folder();
  t.after(() => rmSync(evidence, { recursive: true, force: true }));
  assert.equal(await stopRelaunched(evidence), null);

  const other = sleeper();
  t.after(() => other.child.kill("SIGKILL"));
  assert.equal(command(other.child.pid), "/bin/sleep");
  writeFileSync(join(evidence, RELAUNCHED), JSON.stringify({ pid: other.child.pid, binary: "/Applications/CrazyAgent.app/Contents/MacOS/crazyagent-desktop" }));
  assert.match(await stopRelaunched(evidence), /already ended/);
  assert.equal(command(other.child.pid), "/bin/sleep", "a pid that now runs another binary is left alone");

  const app = sleeper();
  writeFileSync(join(evidence, RELAUNCHED), JSON.stringify({ pid: app.child.pid, binary: command(app.child.pid) }));
  assert.match(await stopRelaunched(evidence), new RegExp(`stopped the relaunched app ${app.child.pid}`));
  assert.equal(await app.ended, "SIGTERM");
});

test("relaunch runs in a live run only, so a fixture run never opens an app that looks for this machine's engine", async () => {
  await assert.rejects(relaunch(), /relaunch runs in a live run only/);
});
