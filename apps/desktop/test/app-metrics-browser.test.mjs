import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/app-metrics-page.mjs", import.meta.url).pathname);
const CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/screens/settings/usage.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/screens/metrics/metrics.css", import.meta.url), "utf8"),
].join("\n");
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const NOW = Date.now();
const none = { input: 0, output: 0, cache_read: 0, cache_write: 0 };
const windowOf = (used_percent, hours) => ({ used_percent, resets_at_ms: NOW + hours * HOUR });
const LIMITS = {
  account: "acct-a", plan: "Max", source: "poll", observed_ms: NOW - 120_000, failure: null, five_hour: windowOf(84, 2), weekly: windowOf(12, 72),
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, five_hour_left_out_label: null, weekly_left_out_label: null,
};
const REPORT = { since_ms: NOW - 6 * DAY, total: none, runs: 0, projects: [], pools: [], accounts: [], agents: [], days: [], lines: [], agent_lines: [], limits: [LIMITS], partial_homes: 0 };
const METRICS = { running: 0, tasks_waiting: 0, tasks_done: 0, tasks_failed: 0, merges: 0, merges_into_main: 0, merges_undone: 0, bucket_ms: DAY, series: [], runs: [], projects: [], agents: [], accounts: [], harnesses: [] };
const ANSWERS = {
  desktop_usage_report: REPORT, desktop_usage_metrics: METRICS, desktop_usage_refresh: null, desktop_usage_set_reset: null,
  desktop_list_accounts: { accounts: [{ id: "acct-a", name: "Pool A, Claude 1" }], next: null }, desktop_pool_summaries: [], desktop_account_ranking: [],
};

/** Opens the app Metrics page in Chrome. Inputs: the page, the theme and the window width.
 * Output: nothing. */
async function open(page, theme = "light", width = 1440) {
  await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.evaluate(fixture);
  await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
  await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, ${JSON.stringify(ANSWERS)})`);
}

/** Clicks an element with the real mouse. Inputs: the page, its selector and its words.
 * Output: nothing. */
async function clickOn(page, selector, words = null) {
  const box = await page.evaluate(`Fixture.boxOf(${JSON.stringify(selector)}, ${JSON.stringify(words)})`);
  assert.ok(box, `nothing to click at ${selector} ${words}`);
  for (const type of ["mousePressed", "mouseReleased"]) {
    await page.send("Input.dispatchMouseEvent", { type, x: box.x, y: box.y, button: "left", clickCount: 1 });
  }
  await page.evaluate("Fixture.settle(120)");
}

test("Check limits now and a reset time work with a real mouse, and the page reads again", async () => {
  await withPage(async (page) => {
    await open(page);
    const reads = await page.evaluate(`Fixture.sentTo("desktop_usage_report").length`);
    await clickOn(page, "button", "Check limits now");
    assert.equal(await page.evaluate(`Fixture.sentTo("desktop_usage_refresh").length`), 1);
    assert.ok(await page.evaluate(`Fixture.sentTo("desktop_usage_report").length`) > reads, "the page reads again after the check");
    assert.deepEqual(await page.evaluate("Fixture.shownToasts()"), ["Checking limits now. They update here within a minute."]);
    await clickOn(page, "section.usage-limits button", "Set reset time");
    await page.evaluate(`Fixture.setField("usage-reset-acct-a-five_hour", "2099-01-02T03:04")`);
    await clickOn(page, "section.usage-limits button", "Save");
    const saved = await page.evaluate(`Fixture.sentTo("desktop_usage_set_reset")`);
    assert.equal(saved.length, 1);
    assert.equal(saved[0].account, "acct-a");
    assert.equal(saved[0].windowKind, "five_hour");
    assert.deepEqual(await page.evaluate("Fixture.shownToasts()"), ["Checking limits now. They update here within a minute.", "Reset time saved."]);
  });
});

test("the Account limits panel does not overflow the window at 760 px", async () => {
  await withPage(async (page) => {
    await open(page, "light", 760);
    assert.equal(await page.evaluate("document.querySelector('section.usage-limits') !== null"), true);
    assert.equal(await page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), true);
  });
});
