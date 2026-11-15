import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const launchPage = await bundle(new URL("./fixtures/launch-reads.mjs", import.meta.url).pathname);
const HOME_READS = ["desktop_pool_summaries", "desktop_harness_catalog", "desktop_learning_progress"];
const UPDATES_READS = ["desktop_update_settings", "desktop_engine_versions"];
const count = (asked, command) => asked.filter((item) => item === command).length;
const again = (before, after) => [...new Set(after.filter((command) => before.includes(command)))];

test("Home's launch reads are not made again when the project list lands after the profile, at launch and after a reconnect", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(launchPage);
    const launched = await evaluate(`Fixture.launch("home", true)`);
    const listed = await evaluate("Fixture.release()");
    const retried = await evaluate("Fixture.retry()");
    const relisted = await evaluate("Fixture.release()");
    return { launched, listed, retried, relisted };
  });
  assert.equal(seen.launched.held, 1, "the project list still waits while Home shows");
  for (const command of HOME_READS) assert.equal(count(seen.launched.asked, command), 1, `Home reads ${command} once at launch`);
  assert.deepEqual(again(seen.launched.asked, seen.listed.asked), [], "the project list landing makes none of Home's launch reads again");
  assert.ok(seen.listed.asked.includes("desktop_team_status"), "Home reads the listed project's work, so the list landed");
  assert.equal(seen.retried.held, 1, "the project list waits again after the reconnect");
  for (const command of HOME_READS) assert.equal(count(seen.retried.asked, command), 1, `Home reads ${command} once after a reconnect`);
  assert.deepEqual(again(seen.retried.asked, seen.relisted.asked), [], "the project list landing after a reconnect makes none of Home's reads again");
});

test("Updates, kept on screen from before the engine answers until the owner has no profile yet, reads again once when the engine runs", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(launchPage);
    const launched = await evaluate(`Fixture.launch("updates", false)`);
    const listed = await evaluate("Fixture.release()");
    return { launched, listed };
  });
  assert.equal(seen.launched.held, 1, "the project list waits while Updates shows");
  const asked = [...seen.launched.asked, ...seen.listed.asked];
  for (const command of UPDATES_READS) assert.equal(count(asked, command), 2, `${command}: once while connecting, once after the engine runs`);
});
