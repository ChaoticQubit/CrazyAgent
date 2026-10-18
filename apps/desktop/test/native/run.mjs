import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  accessSync, closeSync, constants, mkdirSync, mkdtempSync, openSync,
  readFileSync, statSync, writeFileSync, writeSync,
} from "node:fs";
import { createConnection, createServer } from "node:net";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as pause } from "node:timers/promises";

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const workRoot = resolve(appRoot, "../../.local-state/desktop-execution");
const rawRoot = join(workRoot, "native-runs");
mkdirSync(rawRoot, { recursive: true, mode: 0o700 });
const evidence = mkdtempSync(join(rawRoot, "run-"));
const engineRoot = mkdtempSync("/tmp/cad-");
const owned = [];
const started = new Date().toISOString();
const began = performance.now();
const app = process.env.CRAZYAGENT_DESKTOP_TEST_APP;
const fixture = process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE;
let interrupted = false;

function interrupt() {
  interrupted = true;
  for (const entry of owned) {
    if (entry.name === "engine" && !entry.child.stdin.destroyed) {
      entry.child.stdin.end("stop\n");
    } else {
      signalGroup(entry, "SIGTERM");
    }
  }
}

process.once("SIGINT", interrupt);
process.once("SIGTERM", interrupt);

function binary(path) {
  assert(path && isAbsolute(path), "explicit absolute built binary required");
  assert(statSync(path).isFile(), "built binary must be a regular file");
  accessSync(path, constants.X_OK);
  return { path, bytes: statSync(path).size,
    sha256: createHash("sha256").update(readFileSync(path)).digest("hex") };
}

function environment() {
  for (const name of ["home", "tmp"]) mkdirSync(join(evidence, name), { mode: 0o700 });
  return {
    PATH: `${dirname(process.execPath)}:/usr/bin:/bin:/usr/sbin:/sbin`,
    HOME: join(evidence, "home"),
    TMPDIR: join(evidence, "tmp"),
    LANG: "en_US.UTF-8",
    NO_COLOR: "1",
    CRAZYAGENT_DESKTOP_SOCKET: join(engineRoot, "e.sock"),
    CRAZYAGENT_DESKTOP_TEST_APP: app,
    CRAZYAGENT_DESKTOP_TEST_FIXTURE: fixture,
    CRAZYAGENT_DESKTOP_TEST_EVIDENCE: evidence,
  };
}

function signalGroup(entry, signal) {
  if (!entry.child.pid) return;
  try { process.kill(-entry.child.pid, signal); }
  catch (error) { if (error.code !== "ESRCH") throw error; }
}

function groupExists(entry) {
  if (!entry.child.pid) return false;
  try { process.kill(-entry.child.pid, 0); return true; }
  catch (error) { if (error.code === "ESRCH") return false; throw error; }
}

function launch(name, executable, args, env, seconds) {
  const stdout = join(evidence, `${name}.stdout`);
  const stderr = join(evidence, `${name}.stderr`);
  const files = [openSync(stdout, "wx", 0o600), openSync(stderr, "wx", 0o600)];
  const child = spawn(executable, args, {
    cwd: appRoot, env, detached: true, stdio: ["pipe", "pipe", "pipe"],
  });
  child.stdin.on("error", () => {});
  const entry = { name, child, stdout, stderr, argv: [executable, ...args],
    started: new Date().toISOString(), bytes: [0, 0], reason: null, result: null };
  owned.push(entry);
  function fail(reason) {
    entry.reason ??= reason;
    signalGroup(entry, "SIGKILL");
  }
  for (const [index, stream] of [child.stdout, child.stderr].entries()) {
    stream.on("data", (chunk) => {
      entry.bytes[index] += chunk.length;
      if (entry.bytes[index] > 4 * 1024 * 1024) { fail("4MiB stream bound"); return; }
      try {
        let offset = 0;
        while (offset < chunk.length) {
          const count = writeSync(files[index], chunk, offset, chunk.length - offset);
          assert(count > 0, "raw capture write made no progress");
          offset += count;
        }
      }
      catch { fail("raw capture write failed"); }
    });
  }
  const timer = setTimeout(() => fail(`${seconds}-second bound`), seconds * 1000);
  child.on("error", () => { entry.reason ??= "spawn failed"; });
  entry.done = new Promise((done) => child.on("close", (code, signal) => {
    clearTimeout(timer);
    files.forEach(closeSync);
    entry.result = { code, signal, ended: new Date().toISOString() };
    done(entry.result);
  }));
  return entry;
}

async function ready(entry) {
  const deadline = performance.now() + 15000;
  while (performance.now() < deadline) {
    assert(!interrupted, "native test runner interrupted");
    assert(!entry.result, "fixture exited before readiness");
    const bytes = readFileSync(entry.stdout);
    assert(bytes.length <= 4096, "fixture readiness output bound");
    const text = bytes.toString("utf8");
    if (text.includes("\n")) {
      const value = JSON.parse(text.slice(0, text.indexOf("\n")));
      assert.equal(value.ready, true);
      assert.match(value.baseline_id, /^[A-Za-z0-9._:-]{1,128}$/);
      return value;
    }
    await pause(100);
  }
  throw new Error("fixture readiness timeout");
}

async function unusedPort() {
  const server = createServer();
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", accept);
  });
  const port = server.address().port;
  await new Promise((accept, reject) => server.close((error) => error ? reject(error) : accept()));
  return port;
}

async function portOpen(port) {
  return new Promise((accept, reject) => {
    const socket = createConnection({ port, host: "127.0.0.1" });
    let settled = false;
    function finish(value) {
      if (settled) return;
      settled = true;
      socket.destroy();
      accept(value);
    }
    socket.setTimeout(500, () => {
      socket.destroy();
      reject(new Error("driver port cleanup check timed out"));
    });
    socket.once("connect", () => finish(true));
    socket.once("error", (error) => {
      if (error.code === "ECONNREFUSED") finish(false);
      else { socket.destroy(); reject(error); }
    });
  });
}

async function stoppedWithin(entry, milliseconds) {
  const deadline = performance.now() + milliseconds;
  while (performance.now() < deadline) {
    if (!groupExists(entry)) { await entry.done; return true; }
    await pause(100);
  }
  return false;
}

async function stopOwned(entry) {
  if (await stoppedWithin(entry, 200)) return;
  signalGroup(entry, "SIGTERM");
  if (await stoppedWithin(entry, 3000)) return;
  signalGroup(entry, "SIGKILL");
  assert(await stoppedWithin(entry, 3000), "owned process group did not disappear");
}

function captured(entry) {
  return {
    name: entry.name, argv: entry.argv, pid: entry.child.pid,
    started: entry.started, result: entry.result, reason: entry.reason,
    group_absent: !groupExists(entry),
    streams: [entry.stdout, entry.stderr].map((path) => ({
      path, bytes: statSync(path).size,
      sha256: createHash("sha256").update(readFileSync(path)).digest("hex"),
    })),
  };
}

async function execute() {
  const binaries = [binary(app), binary(fixture)];
  const env = environment();
  const port = await unusedPort();
  const engine = launch("engine", fixture, ["serve", engineRoot], env, 150);
  let failure = null;
  let code = 1;
  try {
    const state = await ready(engine);
    env.CRAZYAGENT_DESKTOP_TEST_BASELINE = state.baseline_id;
    env.CRAZYAGENT_DESKTOP_TEST_PORT = String(port);
    const test = launch("webdriver", process.execPath,
      [join(appRoot, "node_modules/@wdio/cli/bin/wdio.js"), "run", "wdio.conf.mjs"], env, 120);
    const result = await test.done;
    assert(!test.reason && result.code !== null, "native test process did not exit normally");
    code = result.code;
  } catch (error) { failure = String(error.message); }
  finally {
    for (const entry of owned.filter((entry) => entry !== engine).reverse()) {
      try { await stopOwned(entry); } catch (error) { failure ??= String(error.message); }
    }
    if (!engine.result) {
      engine.child.stdin.on("error", () => {});
      engine.child.stdin.end("stop\n");
      await Promise.race([engine.done, pause(8000)]);
    }
    try { await stopOwned(engine); } catch (error) { failure ??= String(error.message); }
  }
  if (interrupted) failure ??= "native test runner interrupted";
  if (engine.reason || engine.result?.code !== 0) failure ??= "fixture cleanup was not successful";
  if (await portOpen(port)) failure ??= "native driver port remains reachable";
  const report = { started, ended: new Date().toISOString(), elapsed_ms: performance.now() - began,
    binaries, engine_root: engineRoot, port, env, failure, test_exit: code,
    owned: owned.map(captured) };
  writeFileSync(join(evidence, "completion.json"), JSON.stringify(report, null, 2),
    { flag: "wx", mode: 0o600 });
  console.log(join(evidence, "completion.json"));
  return failure ? 1 : code;
}

try { process.exitCode = await execute(); }
catch (error) {
  for (const entry of owned.reverse()) await stopOwned(entry);
  writeFileSync(join(evidence, "setup-failure.json"), JSON.stringify({
    started, ended: new Date().toISOString(), failure: String(error.message),
  }), { flag: "wx", mode: 0o600 });
  console.error(join(evidence, "setup-failure.json"));
  process.exitCode = 1;
}
