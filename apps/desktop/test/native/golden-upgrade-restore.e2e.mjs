import assert from "node:assert/strict";
import { lstatSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { finishGolden, measure, startRecord, step, waitUntil, writeRecord } from "./golden-run.mjs";
import { DATA, EVIDENCE, LAUNCHED, STAGED, bytes, enginePids, home, launchd, ownerEngine, scratchService } from "./golden-upgrade-restore.wdio.conf.mjs";

const FLOW = "upgrade-restore";
const BUTTON = "Bring back 2.4 data";
const UPGRADE_MS = 240000;
const RESTORE_MS = 180000;
const STEPS = [
  "start the app on the 2.4.2 data",
  "open Restore 2.4",
  "bring back the 2.4 data",
  "find the 2.4 data back",
];
const LINES = `[...document.querySelectorAll(".set-body p[role=status]")].map((line) => line.textContent).join("\\n")`;
const run = {};

/** Calls one desktop command through the app's own bridge.
 * Inputs: the command and its arguments.
 * Output: its answer; throws with the failure the app returned. */
async function invoke(command, args = {}) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: JSON.stringify(failure) }));
  }, command, args);
  if (answer.failed !== undefined) throw new Error(`${command} failed: ${answer.failed}`);
  return answer.value;
}

/** What runs on the scratch engine root: launchd's view of the scratch service and every
 * `engine --home` process on it, a detached one included.
 * Input: none. Output: { running, pid, engines }. */
function engineState() {
  const service = scratchService();
  return { running: service.running, pid: service.pid, engines: enginePids() };
}

/** Opens a hash route of the app and waits until the page reads it.
 * Input: the route, such as "#/settings/restore".
 * Output: none. */
async function go(route) {
  await browser.execute((route) => { window.location.hash = route; }, route);
  await waitUntil(`the app to open ${route}`, () => browser.execute(() => window.location.hash), (hash) => hash === route);
}

describe("golden upgrade and restore", () => {
  after(() => {
    if (run.evidence && !run.finished && run.steps.length > 0) writeRecord(run, "failed");
  });

  it("the owner's first 2.5 start updates the 2.4.2 data, and Restore 2.4 brings it back with the engine left stopped", async () => {
    startRecord(run, FLOW, EVIDENCE);
    const staged = JSON.parse(readFileSync(STAGED, "utf8"));
    const launched = JSON.parse(readFileSync(LAUNCHED, "utf8")).at;
    assert.ok(Number.isSafeInteger(launched) && launched <= Date.now(), "the conf timed the app's launch");
    await waitUntil("the app's first start to update the 2.4.2 data", async () => {
      const status = await invoke("desktop_status");
      const restore = await invoke("desktop_restore_status");
      return status.connected === true && restore.until_ms !== null;
    }, (ready) => ready, UPGRADE_MS);
    measure(run, "upgrade_start_ms", Date.now() - launched);
    measure(run, "disk_growth_bytes", Math.max(0, bytes(DATA) - staged.seeded));
    const started = engineState();
    assert.ok(started.pid !== null && started.engines.includes(started.pid), `the scratch login service runs the engine, and the stopped checks see it: ${JSON.stringify(started)}`);
    const definition = readFileSync(join(home, "Library", "LaunchAgents", `${staged.label}.plist`), "utf8");
    assert.ok(definition.includes(`<key>HOME</key>\n\t\t<string>${home}</string>`), "the scratch service gives its engine the scratch HOME, never the owner's");
    step(run, STEPS[0]);

    await go("#/settings/restore");
    await waitUntil("Restore 2.4 with its button", () => browser.execute(LINES), (text) => text.includes("You can bring back your 2.4 data until"));
    await (await $(`button=${BUTTON}`)).click();
    const dialog = await $("dialog[open]");
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /Bring back your 2\.4 data\?/);
    await browser.saveScreenshot(join(EVIDENCE, "golden-upgrade-restore-confirm.png"));
    step(run, STEPS[1]);

    await (await dialog.$(`button=${BUTTON}`)).click();
    const last = await waitUntil("the restore to end", () => invoke("desktop_restore_status"), (view) => view.last !== null && view.last.kind !== "pending", RESTORE_MS);
    assert.equal(last.last.kind, "restored", `the restore brought the 2.4 data back: ${JSON.stringify(last)}`);
    await waitUntil("the engine to stay stopped", engineState, (now) => !now.running && now.pid === null && now.engines.length === 0);
    step(run, STEPS[2]);

    await go("#/settings/sandbox");
    await go("#/settings/restore");
    const back = await waitUntil("Restore 2.4 to say the data is back", () => browser.execute(LINES), (text) => text.includes("Your 2.4 data is back."));
    assert.ok(!back.includes("You can bring back"), "no second restore is offered");
    await browser.saveScreenshot(join(EVIDENCE, "golden-upgrade-restore-back.png"));
    const copy = last.last.copy;
    assert.match(copy, /^replaced-/);
    assert.equal(lstatSync(join(DATA, "backups", copy)).mode & 0o777, 0o700, "the dated copy of the 2.5 data is private");
    assert.deepEqual(engineState(), { running: false, pid: null, engines: [] }, "no start, launchd or detached, brought 2.5 back on the restored data");
    assert.deepEqual(ownerEngine(), staged.owner, "the owner's engine is untouched");
    assert.equal(launchd(staged.label).pid, null, "launchd runs nothing under the scratch label");
    step(run, STEPS[3]);

    const record = finishGolden(run);
    assert.deepEqual([record?.steps.map((done) => done.step), record?.outcome], [STEPS, "passed"], "the run record names each step in order");
    const measures = record?.measures ?? {};
    assert.ok(measures.upgrade_start_ms > 0 && measures.disk_growth_bytes > 0, `the run record holds the upgrade start's time and disk growth: ${JSON.stringify(record)}`);
  });
});
