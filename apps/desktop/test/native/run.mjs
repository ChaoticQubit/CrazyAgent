import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  accessSync, closeSync, constants, mkdirSync, mkdtempSync, openSync,
  readFileSync, statSync, writeFileSync, writeSync,
} from "node:fs";
import { createConnection } from "node:net";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setTimeout as pause } from "node:timers/promises";
import { endTerminals, freeLowPort } from "./e2e-harness.mjs";
import { chooseHome, clearWebStore, createRunDirectories, lockHome, refusedToolHome, runProjects } from "./run-layout.mjs";
import { prepareSpeedSide, speedSide } from "./speed-seed.mjs";

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
const suite = process.env.CRAZYAGENT_DESKTOP_TEST_SUITE;
assert(["model-access", "model-access-read-only", "native-auth", "pools-screen", "pools-spend", "first-run-guide", "first-run-skip-guide",
  "first-run-harnesses", "first-run-harnesses-node", "learning-replay-guide", "learning-replay-first-run", "refused-tool", "speed"].includes(suite),
  "closed native suite required");
const WEBDRIVER_SECONDS = ["pools-screen", "pools-spend", "first-run-harnesses-node", "refused-tool", "speed"].includes(suite) ? 900 : 300;
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

async function environment() {
  createRunDirectories(evidence);
  const { home, prepared } = suite === "refused-tool" ? refusedToolHome() : chooseHome(join(evidence, "home"));
  await lockHome(home, { label: `run.mjs ${suite} ${evidence}` });
  clearWebStore(home);
  return { projects: runProjects(home), env: {
    PATH: `${dirname(process.execPath)}:/usr/bin:/bin:/usr/sbin:/sbin`,
    HOME: home,
    ...(prepared ? { CFFIXED_USER_HOME: home } : {}),
    TMPDIR: join(evidence, "tmp"),
    LANG: "en_US.UTF-8",
    NO_COLOR: "1",
    CRAZYAGENT_DESKTOP_SOCKET: join(engineRoot, "e.sock"),
    CRAZYAGENT_DESKTOP_TEST_APP: app,
    CRAZYAGENT_DESKTOP_TEST_FIXTURE: fixture,
    CRAZYAGENT_DESKTOP_TEST_EVIDENCE: evidence,
    CRAZYAGENT_DESKTOP_TEST_SUITE: suite,
    ...(["pools-spend", "pools-screen"].includes(suite) ? { CRAZYAGENT_DESKTOP_TEST_USAGE_UPKEEP: "1" } : {}),
  } };
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
      assert.deepEqual(value, { ready: true });
      return value;
    }
    await pause(100);
  }
  throw new Error("fixture readiness timeout");
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

/** The engine's environment for one suite: the run's own, or for the Node suite a clean Mac's:
 * a Codex the app's npm put in the engine prefix `<engine root>/setup/harnesses`, the one folder
 * the resolver accepts here (it refuses PATH folders in a project or a temp folder), no Node on
 * PATH and an empty clean-Mac gate root; the suite's home is never written. The refused-tool suite
 * gets the system PATH and an empty gate root, so the only graphify is the one in its home.
 * Input: the run environment. Output: the engine environment. */
function engineEnvironment(env) {
  if (suite === "refused-tool") {
    return { ...env, PATH: "/usr/bin:/bin:/usr/sbin:/sbin", CRAZYAGENT_GATE_SYSTEM_ROOT: mkdtempSync(join(evidence, "gate-")) };
  }
  if (suite !== "first-run-harnesses-node") return env;
  const bin = join(engineRoot, "setup", "harnesses", "bin");
  mkdirSync(bin, { recursive: true, mode: 0o700 });
  writeFileSync(join(bin, "codex"), "#!/usr/bin/env node\n", { mode: 0o755 });
  const gate = mkdtempSync(join(evidence, "gate-"));
  return { ...env, PATH: "/usr/bin:/bin:/usr/sbin:/sbin", CRAZYAGENT_GATE_SYSTEM_ROOT: gate };
}

/** Starts the speed suite's real engine binary on the side's kept state, as the owner's
 * launchd engine runs before the app opens, and points the app at its socket. Inputs: the run
 * environment and the side (from speedSide). Output: the owned engine entry. */
function speedEngine(env, side) {
  assert(side.engine && side.runtime, "a speed side with an engine and a runtime");
  env.CRAZYAGENT_DESKTOP_SOCKET = join(side.runtime, "engine.sock");
  const entry = launch("engine", side.engine,
    ["engine", side.runtime, join(side.runtime, "global.toml"), join(side.runtime, "project.toml"),
      "engine.sock", "--writable-settings"], env, WEBDRIVER_SECONDS + 30);
  entry.spawnedMs = performance.now();
  return entry;
}

/** Waits for the real engine's ready line in its captured stdout and records how long its
 * start took (row T2). Input: the owned engine entry. Output: the start time in ms. */
async function engineReady(entry) {
  const deadline = performance.now() + 60000;
  while (performance.now() < deadline) {
    assert(!interrupted, "native test runner interrupted");
    assert(!entry.result, "engine exited before readiness");
    const bytes = readFileSync(entry.stdout);
    assert(bytes.length <= 64 * 1024, "engine readiness output bound");
    if (/^ready$/m.test(bytes.toString("utf8"))) return performance.now() - entry.spawnedMs;
    await pause(5);
  }
  throw new Error("engine readiness timeout");
}

async function execute() {
  const binaries = [binary(app), binary(fixture)];
  const { env, projects } = await environment();
  const port = await freeLowPort();
  const side = suite === "speed" ? await speedSide(app) : null;
  const engine = side ? speedEngine(env, side) : launch("engine", fixture,
    [suite === "model-access-read-only" ? "serve" : "serve-model-access",
      engineRoot, projects], engineEnvironment(env), WEBDRIVER_SECONDS + 30);
  let failure = null;
  let code = 1;
  try {
    if (side) {
      const engineStartMs = await engineReady(engine);
      const prepared = await prepareSpeedSide(side, env, evidence);
      writeFileSync(join(evidence, "speed-engine.json"), JSON.stringify({ engine_start_ms: engineStartMs, ...prepared }, null, 1), { flag: "wx", mode: 0o600 });
      env.SPEED_ENGINE_PID = String(engine.child.pid);
    } else {
      await ready(engine);
    }
    env.CRAZYAGENT_DESKTOP_TEST_PORT = String(port);
    const test = launch("webdriver", process.execPath,
      ["--import", pathToFileURL(join(appRoot, "test/native/private-log-preload.mjs")).href,
        join(appRoot, "node_modules/@wdio/cli/bin/wdio.js"), "run",
        suite === "native-auth" ? "test/native/native-auth.wdio.conf.mjs"
          : suite === "speed" ? "test/native/speed.wdio.conf.mjs"
            : "test/native/model-access.wdio.conf.mjs"],
      env, WEBDRIVER_SECONDS);
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
      if (side) signalGroup(engine, "SIGTERM");
      else engine.child.stdin.end("stop\n");
      await Promise.race([engine.done, pause(8000)]);
    }
    try { await stopOwned(engine); } catch (error) { failure ??= String(error.message); }
    if (side) {
      try { await endTerminals(side.runtime); } catch (error) { failure ??= String(error.message); }
    }
  }
  if (interrupted) failure ??= "native test runner interrupted";
  const engineEnded = engine.result?.code === 0 || (side !== null && engine.result?.signal === "SIGTERM");
  if (engine.reason || !engineEnded) failure ??= "fixture cleanup was not successful";
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
  console.error(String(error.message));
  console.error(join(evidence, "setup-failure.json"));
  process.exitCode = 1;
}
