import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import fs, { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { lockHome } from "./run-layout.mjs";

const LOCK = ".crazyagent-native-run.lock";
const source = (name) => readFileSync(new URL(name, import.meta.url), "utf8");
const holder = (home) => JSON.parse(readFileSync(join(home, LOCK), "utf8"));

const FIRST_SUITE = `
import { lockHome } from ${JSON.stringify(new URL("./run-layout.mjs", import.meta.url).href)};
const lock = await lockHome(process.argv[1], { label: "first suite", waitMs: 0 });
process.stdout.write("held\\n");
process.stdin.setEncoding("utf8");
process.stdin.on("data", (line) => {
  if (line.startsWith("release")) {
    lock.release();
    process.stdout.write("released\\n");
  }
  if (line.startsWith("exit")) process.exit(0);
});
`;

/** Waits until a first suite's process prints a word. Inputs: the process and the word.
 * Output: nothing, or a refusal if the process ends first. */
function said(child, word) {
  return new Promise((resolve, reject) => {
    let seen = "";
    const ended = () => reject(new Error(`the first suite ended before it said ${word}`));
    const read = (chunk) => {
      seen += chunk;
      if (!seen.includes(word)) return;
      child.stdout.off("data", read);
      child.off("exit", ended);
      resolve();
    };
    child.stdout.on("data", read);
    child.once("exit", ended);
  });
}

/** Runs one check on a scratch home, with the first suites it starts in their own processes,
 * each holding the home's lock until told to release it or exit, or killed. Every one left is
 * killed and the home removed after. Input: the check, given the home and the starter.
 * Output: nothing. */
async function withHome(check) {
  const home = mkdtempSync(join(tmpdir(), "ca-home-lock-"));
  const children = [];
  const firstSuite = async () => {
    const child = spawn(process.execPath, ["--input-type=module", "-e", FIRST_SUITE, home], { stdio: ["pipe", "pipe", "inherit"] });
    children.push(child);
    child.stdout.setEncoding("utf8");
    await said(child, "held");
    return child;
  };
  try {
    await check(home, firstSuite);
  } finally {
    for (const child of children) if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    rmSync(home, { recursive: true, force: true });
  }
}

test("a second suite on a held home waits its bound, then fails naming the holder's pid and start, and never takes the home", async () => {
  await withHome(async (home, firstSuite) => {
    const first = await firstSuite();
    const held = holder(home);
    const began = Date.now();
    await assert.rejects(lockHome(home, { label: "second suite", waitMs: 1000, pollMs: 50 }), (error) => {
      assert.ok(error.message.includes(`pid ${first.pid} (first suite), started ${held.started}`), error.message);
      assert.ok(error.message.includes(home), error.message);
      return true;
    });
    assert.ok(Date.now() - began >= 1000, "it waited its bound");
    assert.equal(holder(home).pid, first.pid, "the home stays the first suite's");
  });
});

test("a second suite waiting on a held home gets it once the first releases it", async () => {
  await withHome(async (home, firstSuite) => {
    const first = await firstSuite();
    let taken = false;
    const second = lockHome(home, { label: "second suite", waitMs: 60000, pollMs: 50 }).then((lock) => {
      taken = true;
      return lock;
    });
    await pause(300);
    assert.equal(taken, false, "the second suite waits while the first holds the home");
    first.stdin.write("release\n");
    await said(first, "released");
    const lock = await second;
    assert.equal(holder(home).pid, process.pid);
    lock.release();
    assert.equal(existsSync(join(home, LOCK)), false);
  });
});

test("a first suite that exits without releasing leaves no lock behind", async () => {
  await withHome(async (home, firstSuite) => {
    const first = await firstSuite();
    assert.equal(holder(home).pid, first.pid);
    first.stdin.write("exit\n");
    await once(first, "exit");
    assert.equal(existsSync(join(home, LOCK)), false);
    const lock = await lockHome(home, { label: "second suite", waitMs: 0 });
    lock.release();
  });
});

test("the lock of a first suite that was killed is taken over at once", async () => {
  await withHome(async (home, firstSuite) => {
    const first = await firstSuite();
    first.kill("SIGKILL");
    await once(first, "exit");
    assert.equal(holder(home).pid, first.pid, "a killed suite leaves its lock");
    const lock = await lockHome(home, { label: "second suite", waitMs: 0 });
    assert.equal(holder(home).pid, process.pid);
    assert.equal(holder(home).label, "second suite");
    lock.release();
    lock.release();
    assert.equal(existsSync(join(home, LOCK)), false);
  });
});

test("both native runners lock their home before they change it; the wdio runner stops on a held home and frees it last", () => {
  const runner = source("./run.mjs");
  assert.ok(runner.includes("await lockHome(home, { label: `run.mjs"), "run.mjs takes the lock");
  assert.ok(runner.indexOf("lockHome(home") < runner.indexOf("clearWebStore(home)"), "run.mjs locks before it clears the web store");
  assert.match(runner, /console\.error\(String\(error\.message\)\)/, "a held home's message reaches the terminal");
  const conf = source("./master-switch.wdio.conf.mjs");
  const prepare = conf.slice(conf.indexOf("onPrepare:"), conf.indexOf("before:"));
  assert.ok(prepare.includes("homeLock = await lockHome(home"), "onPrepare takes the lock");
  assert.ok(prepare.indexOf("lockHome(home") < prepare.indexOf("clearWebStore(home)"), "onPrepare locks before it clears the web store");
  assert.match(prepare, /throw new SevereServiceError\(/, "a held home stops the wdio run, which logs a plain hook error and goes on");
  const complete = conf.slice(conf.indexOf("onComplete:"));
  assert.ok(complete.indexOf("homeLock?.release()") > complete.indexOf("stopTerminals()"), "the home is freed after the engine and terminals stop");
});

/** A lock as lockHome writes it, for a holder this test plays. Inputs: the pid and label.
 * Output: the lock's text. */
function lockText(pid, label) {
  return JSON.stringify({ pid, started: new Date().toISOString(), label, token: randomBytes(16).toString("hex") });
}

/** A pid that no process has: a child that already ended. Output: the pid. */
function deadPid() {
  const ended = spawnSync(process.execPath, ["-e", ""]);
  assert.equal(ended.status, 0);
  return ended.pid;
}

/** Runs a check with some node:fs calls of run-layout.mjs replaced, then puts them back.
 * Inputs: the replacements by name and the check. Output: the check's result. */
async function withFs(replacements, check) {
  const kept = Object.fromEntries(Object.keys(replacements).map((name) => [name, fs[name]]));
  Object.assign(fs, replacements);
  syncBuiltinESMExports();
  try {
    return await check();
  } finally {
    Object.assign(fs, kept);
    syncBuiltinESMExports();
  }
}

test("a taker that read a dead holder never removes the live lock a faster taker put in its place, while a third taker waits on an empty home", async () => {
  await withHome(async (home) => {
    const lock = join(home, LOCK);
    const dead = lockText(deadPid(), "killed suite");
    const faster = lockText(process.pid, "faster taker");
    const third = lockText(process.pid, "third taker");
    writeFileSync(lock, dead, { flag: "wx", mode: 0o600 });
    const { readFileSync: read, renameSync: rename, rmSync: remove, unlinkSync: unlink } = fs;
    let swapped = false;
    let thirdTook = false;
    const thirdTakes = () => {
      if (!swapped || thirdTook || existsSync(lock)) return;
      thirdTook = true;
      writeFileSync(lock, third, { flag: "wx", mode: 0o600 });
    };
    const after = (call) => function (...args) {
      try {
        return call.apply(this, args);
      } finally {
        thirdTakes();
      }
    };
    await withFs({
      readFileSync(path, ...rest) {
        const text = read.call(this, path, ...rest);
        if (path === lock && !swapped && text === dead) {
          swapped = true;
          remove(lock);
          writeFileSync(lock, faster, { flag: "wx", mode: 0o600 });
        }
        return text;
      },
      renameSync: after(rename),
      rmSync: after(remove),
      unlinkSync: after(unlink),
    }, async () => {
      await assert.rejects(lockHome(home, { label: "slow taker", waitMs: 0 }), /in use by pid/);
    });
    assert.equal(swapped, true, "the faster taker took the home after the slow taker read the dead holder");
    assert.equal(readFileSync(lock, "utf8"), faster, "the faster taker's live lock stays, so no two suites share the home");
    assert.equal(thirdTook, false, "the home is never left empty for a third taker while the faster one holds it");
    assert.deepEqual(readdirSync(home), [LOCK], "no takeover or draft file is left");
  });
});

test("a takeover lock left by a taker that died is refused, naming it, and the dead holder's lock stays", async () => {
  await withHome(async (home) => {
    const lock = join(home, LOCK);
    const dead = lockText(deadPid(), "killed suite");
    writeFileSync(lock, dead, { flag: "wx", mode: 0o600 });
    writeFileSync(`${lock}.takeover`, lockText(deadPid(), "killed taker"), { flag: "wx", mode: 0o600 });
    await assert.rejects(lockHome(home, { label: "second suite", waitMs: 0 }), (error) => {
      assert.ok(error.message.includes(`${lock}.takeover`), error.message);
      return true;
    });
    assert.equal(readFileSync(lock, "utf8"), dead, "no taker removes the dead holder's lock while a takeover may be half done");
  });
});

test("the five confs built on master-switch keep its hooks, so each locks the home it gives the app and frees it", async () => {
  const scratch = mkdtempSync(join(tmpdir(), "ca-home-lock-conf-"));
  const names = ["WD_APP", "MS_ENGINE", "MS_ROOT", "MS_EVIDENCE", "WD_PORT", "CRAZYAGENT_E2E_HOME"];
  const kept = Object.fromEntries(names.map((name) => [name, process.env[name]]));
  try {
    Object.assign(process.env, { WD_APP: join(scratch, "app"), MS_ENGINE: join(scratch, "engine"), MS_ROOT: join(scratch, "root"), MS_EVIDENCE: join(scratch, "evidence"), WD_PORT: "4555" });
    delete process.env.CRAZYAGENT_E2E_HOME;
    const base = await import("./master-switch.wdio.conf.mjs");
    assert.equal(base.home, join(scratch, "root", "home"));
    for (const name of ["done-quiet", "ide-window", "live-master-account", "live-terminal", "master-idle-stop"]) {
      const { config } = await import(`./${name}.wdio.conf.mjs`);
      assert.equal(config.onPrepare, base.config.onPrepare, `${name} runs master-switch's onPrepare, which locks the home`);
      assert.equal(config.onComplete, base.config.onComplete, `${name} runs master-switch's onComplete, which frees it`);
      assert.equal(config.services[0][1].env.HOME, base.home, `${name} gives the app the home master-switch locks`);
    }
  } finally {
    for (const name of names) {
      if (kept[name] === undefined) delete process.env[name];
      else process.env[name] = kept[name];
    }
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("v1-flow locks the HOME its app and engine inherit before it starts the engine, stops on a held home and frees it last", () => {
  const conf = source("./v1-flow.wdio.conf.mjs");
  assert.match(conf, /import \{ SevereServiceError \} from "webdriverio";/);
  assert.match(conf, /const home = process\.env\.HOME;/);
  assert.match(conf, /\["HOME", home\]/, "HOME must be an absolute path");
  const prepare = conf.slice(conf.indexOf("onPrepare:"), conf.indexOf("before:"));
  assert.ok(prepare.includes("homeLock = await lockHome(home, { label: `wdio v1-flow"), "onPrepare takes the lock");
  assert.ok(prepare.indexOf("lockHome(home") < prepare.indexOf("startEngine()"), "the home is locked before the engine starts");
  assert.match(prepare, /throw new SevereServiceError\(/, "a held home stops the wdio run");
  const complete = conf.slice(conf.indexOf("onComplete:"));
  assert.ok(complete.indexOf("homeLock?.release()") > complete.indexOf("stopEngine()"), "the home is freed after the engine stops");
});
