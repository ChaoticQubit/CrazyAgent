import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { terminalServers } from "./e2e-harness.mjs";
import { DATA, SIDECAR, STAGED, home, lockedVersion, ownerEngine, scratchService, versionOf } from "./engine-swap.wdio.conf.mjs";

const SWAP_MS = 240000;
const LINE = "CrazyAgent's engine was updated to match this app.";

/** Reads what the owner sees: the toast line, whether the app runs (first-run Welcome or the
 * shell, not the launch skeleton or a full-window engine state), and the page heading.
 * Output: that view. */
function view() {
  return browser.execute(() => ({
    toast: document.querySelector(".toast[role=status]")?.textContent?.trim() ?? null,
    running: document.querySelector("[aria-busy=true]") === null && document.querySelector("main.full-state") === null && document.querySelector("main#main") !== null,
    heading: document.querySelector("main h1")?.textContent ?? null,
  }));
}

/** Whether a process runs. Input: its pid. Output: the answer. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (failure) {
    return failure.code === "EPERM";
  }
}

describe("An engine older than the app is replaced at launch on its own (DEC-134 addendum 1)", () => {
  it("the app swaps the older engine once, runs on this build's engine, keeps the terminal servers and says so in one line", async () => {
    const staged = JSON.parse(readFileSync(STAGED, "utf8"));
    assert.ok(Number.isInteger(staged.oldPid) && staged.oldPid > 0, "the older engine ran before the app started");
    let seen = { toast: null, running: false, heading: null };
    let told = false;
    await browser.waitUntil(async () => {
      seen = await view();
      told ||= seen.toast === LINE;
      return told && seen.running;
    },{ timeout: SWAP_MS, interval: 100, timeoutMsg: `the app never ran on the swapped engine with its line: ${JSON.stringify(seen)}` });
    assert.notEqual(seen.heading, "CrazyAgent's engine needs a restart", "the restart view never shows for an older engine");
    assert.deepEqual(ownerEngine(), staged.owner, "the owner's own login service and engine are untouched: the app ran on the scratch root");
    const now = scratchService();
    assert.equal(now.label, staged.label, "the same scratch service runs the engine");
    assert.ok(now.pid !== null && now.pid !== staged.oldPid, `a new engine process runs: ${JSON.stringify(now)}`);
    assert.equal(alive(staged.oldPid), false, "the older engine process is gone");
    assert.equal(lockedVersion(), staged.newer, "the running engine is this build's version");
    assert.equal(versionOf(SIDECAR), staged.newer);
    const definition = readFileSync(join(home, "Library", "LaunchAgents", `${staged.label}.plist`), "utf8");
    assert.ok(definition.includes(join(staged.bundle, "Contents", "MacOS", "crazyagent")), "the service runs the bundle's engine");
    assert.ok(definition.includes(`<key>HOME</key>\n\t\t<string>${home}</string>`), "the service gives its engine the scratch HOME, never the owner's");
    const after = terminalServers(DATA);
    for (const pid of staged.terminals) assert.ok(after.includes(pid), `terminal server ${pid} outlived the swap: ${after}`);
    const log = readFileSync(join(DATA, "logs", "engine.log"), "utf8");
    assert.equal(log.split("\n").filter((line) => line === "ready").length, 2, "one older start and one swap: the app replaced the engine once");
  });
});
