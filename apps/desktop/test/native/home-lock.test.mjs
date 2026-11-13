import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
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
