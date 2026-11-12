import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { dayLabel, offsetMinutes } = await import("../src/screens/metrics/usage-model.tsx");

const fixture = await bundle(new URL("./fixtures/app-metrics-page.mjs", import.meta.url).pathname);
const CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/screens/metrics/metrics.css", import.meta.url), "utf8"),
].join("\n");
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const NOW = Date.now();
const windowOf = (used_percent, hours) => ({ used_percent, resets_at_ms: NOW + hours * HOUR });
const LIMITS = {
  account: "acct-a", plan: "Max", source: "poll", observed_ms: NOW - 120_000, failure: null, five_hour: windowOf(84, 2), weekly: windowOf(12, 72),
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, five_hour_left_out_label: null, weekly_left_out_label: null,
};
const USED = { input: 900, output: 300, cache_read: 100, cache_write: 200 };
const DAYS = Array.from({ length: 30 }, (_, index) => ({ day_ms: NOW - (29 - index) * DAY, tokens: USED }));
const REPORT = { since_ms: NOW - 6 * DAY, total: USED, runs: 1, projects: [], pools: [], accounts: [], agents: [], days: DAYS, lines: [], agent_lines: [], limits: [LIMITS], partial_homes: 0, money: { settings: { main_number: "tokens", prices: "off" }, shown: "tokens", as_of: null, cost_nanos: 0, unpriced_tokens: 0 } };
const SHIFT = -new Date(NOW).getTimezoneOffset() * 60_000;
const TODAY = Math.floor((NOW + SHIFT) / DAY) * DAY - SHIFT;
const SERIES = [{ start_ms: TODAY - 3 * DAY, tokens: { input: 900, output: 300, cache_read: 100, cache_write: 200 } }];
const METRICS = { running: 0, tasks_waiting: 0, tasks_done: 0, tasks_failed: 0, merges: 0, merges_into_main: 0, merges_undone: 0, bucket_ms: DAY, series: SERIES, models: [], activity: [], activity_since_ms: null, tools: [], tools_since_ms: null, runs: [], projects: [{ key: "alpha", tokens: USED, agents: 1, runs: 1, projects: ["alpha"], merges: 0 }], agents: [], accounts: [], harnesses: [] };
const ANSWERS = {
  desktop_usage_report: REPORT, desktop_usage_metrics: METRICS, desktop_usage_notices: "", desktop_usage_refresh: null, desktop_usage_set_reset: null,
  desktop_list_accounts: { accounts: [{ id: "acct-a", name: "Pool A, Claude 1" }], next: null }, desktop_pool_summaries: [], desktop_account_ranking: [],
  desktop_usage_extras: [{ account: "acct-a", extras: { harness: "Claude Code", adapter: "claude", may_break: true, resets: { count: 2, kinds: ["five_hour"], next_expiry_ms: null }, credits: "$12.00 of $50.00 used this month", spend: { kind: "in_app" }, not_reported: "not reported by Claude Code", resets_checked_ms: null, credits_checked_ms: null } }],
};

/** Opens the app Metrics page in Chrome. Inputs: the page, the theme and the window width.
 * Output: nothing. */
async function open(page, theme = "light", width = 1440, answers = ANSWERS) {
  await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.evaluate(fixture);
  await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
  await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, ${JSON.stringify(answers)})`);
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

/** Presses a key on the focused element with the real keyboard. Inputs: the page, the key's
 * name and its code. Output: nothing. */
async function key(page, name, code) {
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: name, code: name, windowsVirtualKeyCode: code, ...(name === "Enter" && type === "keyDown" ? { text: "\r" } : {}) });
  await page.evaluate("Fixture.settle(120)");
}

const ratioHelpers = `
  const luminance = (color) => {
    const [r, g, b] = color.match(/[\\d.]+/g).slice(0, 3).map((part) => {
      const value = Number(part) / 255;
      return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const ratio = (a, b) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
`;

const contrast = `(() => {
  ${ratioHelpers}
  const surface = luminance(getComputedStyle(document.querySelector(".mx-lead")).backgroundColor);
  return ["in", "cw", "out"].map((name) => ratio(luminance(getComputedStyle(document.querySelector(".mx-chart ." + name)).fill), surface));
})()`;

for (const theme of ["light", "dark"]) {
  test(`every chart series stands off the panel in ${theme}, and no two share a color`, async () => {
    await withPage(async (page) => {
      await open(page, theme);
      const ratios = await page.evaluate(contrast);
      assert.equal(ratios.length, 3);
      for (const ratio of ratios) assert.ok(ratio >= 3, `series contrast ${ratio} in ${theme}`);
      const fills = await page.evaluate(`["in", "cw", "out"].map((name) => getComputedStyle(document.querySelector(".mx-chart ." + name)).fill)`);
      assert.equal(new Set(fills).size, 3, JSON.stringify(fills));
    });
  });
}

test("real keys walk the Range to All, and Enter opens the chart's table", async () => {
  await withPage(async (page) => {
    await open(page);
    await page.evaluate(`document.querySelector('[aria-label="Range"] [aria-checked="true"]').focus()`);
    await key(page, "ArrowRight", 39);
    await key(page, "ArrowRight", 39);
    assert.equal(await page.evaluate(`document.querySelector('[aria-label="Range"] [aria-checked="true"]').textContent`), "All");
    assert.equal((await page.evaluate(`Fixture.sentTo("desktop_usage_report")`)).at(-1).sinceMs, null);
    await page.evaluate(`document.querySelector(".mx-lead details summary").focus()`);
    await key(page, "Enter", 13);
    assert.equal(await page.evaluate(`document.querySelector(".mx-lead details").open`), true);
  });
});

test("the readout says the highest bar, and the bar under the real mouse", async () => {
  await withPage(async (page) => {
    await open(page);
    const readout = () => page.evaluate(`document.querySelector(".mx-readout").textContent`);
    assert.match(await readout(), /^Highest, .*: 1\.4K$/);
    const box = await page.evaluate(`Fixture.boxOf(".mx-chart .hit", null)`);
    assert.ok(box, "the chart has a place to point at");
    await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: box.x, y: box.y });
    await page.evaluate("Fixture.settle(120)");
    assert.doesNotMatch(await readout(), /^Highest/);
  });
});

test("a pointer resting on a bar survives a shorter range: the page stays and the readout drops it", async () => {
  await withPage(async (page) => {
    await open(page);
    const target = await page.evaluate(`(() => {
      const hits = [...document.querySelectorAll(".mx-chart .hit")];
      const last = hits.at(-1);
      last.scrollIntoView({ block: "center" });
      const box = last.getBoundingClientRect();
      return { count: hits.length, x: box.left + box.width / 2, y: box.top + box.height / 2 };
    })()`);
    assert.equal(target.count, 7, "seven days draw seven bars");
    await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: target.x, y: target.y });
    await page.evaluate("Fixture.settle(120)");
    assert.match(await page.evaluate(`document.querySelector(".mx-readout").textContent`), /^[A-Z][a-z]{2} \d+: 0$/);
    await page.evaluate(`document.querySelector('[aria-label="Range"] [aria-checked="true"]').focus()`);
    await key(page, "ArrowLeft", 37);
    assert.equal(await page.evaluate(`document.querySelector(".mx-lead") !== null`), true, "the page is still there");
    assert.equal(await page.evaluate(`document.querySelectorAll(".mx-chart .hit").length`), 1);
    assert.equal(await page.evaluate(`document.querySelector(".mx-readout").textContent`), "No tokens in this range");
  });
});

for (const width of [1440, 760]) {
  test(`the lead panel does not overflow at ${width} px`, async () => {
    await withPage(async (page) => {
      await open(page, "light", width);
      assert.equal(await page.evaluate("document.querySelector('.mx-lead') !== null"), true);
      assert.equal(await page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), true);
    });
  });
}

test("Enter opens a panel's All columns and a real click shows every day", async () => {
  await withPage(async (page) => {
    await open(page);
    await page.evaluate(`document.querySelector(".mx-panels details summary").focus()`);
    await key(page, "Enter", 13);
    assert.equal(await page.evaluate(`document.querySelector(".mx-panels details").open`), true);
    const newest = dayLabel(DAYS.at(-1).day_ms, offsetMinutes(new Date(NOW)));
    const firstDay = () => page.evaluate(`document.querySelector(".mx-days tbody th").textContent`);
    assert.equal(await firstDay(), newest);
    await clickOn(page, "button", "Show all 30 days");
    assert.equal(await page.evaluate(`document.querySelectorAll(".mx-days tbody tr").length`), 30);
    assert.equal(await firstDay(), newest);
    await clickOn(page, "button", "Show last 7 days");
    assert.equal(await page.evaluate(`document.querySelectorAll(".mx-days tbody tr").length`), 7);
  });
});

for (const theme of ["light", "dark"]) {
  test(`the panels' bars and words are readable in ${theme}`, async () => {
    await withPage(async (page) => {
      await open(page, theme);
      const seen = await page.evaluate(`(() => {
        ${ratioHelpers}
        const row = document.querySelector(".mx-panels .mx-row");
        const panel = getComputedStyle(row.closest(".panel")).backgroundColor;
        const against = (selector) => ratio(luminance(getComputedStyle(row.querySelector(selector)).color), luminance(panel));
        return { bar: getComputedStyle(row.querySelector(".mx-bar i")).backgroundColor, panel, name: against(".mx-row-name"), side: against(".mx-row-side") };
      })()`);
      assert.notEqual(seen.bar, seen.panel, JSON.stringify(seen));
      assert.ok(seen.name >= 4.5, JSON.stringify(seen));
      assert.ok(seen.side >= 4.5, JSON.stringify(seen));
    });
  });
}

test("words about usage not reported leave every row's name its width at 1100 px", async () => {
  await withPage(async (page) => {
    const silent = (key, tokens) => ({ key, tokens, runs: 2, last_ms: NOW - HOUR, not_reported_by: "Gemini CLI" });
    const accounts = [silent("acct-a", USED), silent("acct-z", { input: 0, output: 0, cache_read: 0, cache_write: 0 })];
    await open(page, "light", 1100, { ...ANSWERS, desktop_usage_report: { ...REPORT, accounts } });
    await page.evaluate(`document.body.style.paddingLeft = "220px"`);
    const widths = await page.evaluate(`[...document.querySelectorAll('[aria-label="By account"] .mx-row-name')].map((name) => name.getBoundingClientRect().width)`);
    assert.equal(widths.length, 2);
    for (const width of widths) assert.ok(width >= 60, `a name is ${width} px wide`);
    assert.equal(await page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), true);
  });
});

test("the project Metrics tab asks the engine for that project's report and metrics only", async () => {
  await withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mountProject(${JSON.stringify(CSS)}, ${JSON.stringify(ANSWERS)}, "alpha")`);
    const reports = await page.evaluate(`Fixture.sentTo("desktop_usage_report")`);
    const metrics = await page.evaluate(`Fixture.sentTo("desktop_usage_metrics")`);
    assert.ok(reports.length > 0, "the tab read its report");
    assert.ok(metrics.length > 0, "the tab read its metrics");
    assert.deepEqual([...new Set([...reports, ...metrics].map((sent) => sent.projectId))], ["alpha"]);
    assert.equal(await page.evaluate(`document.querySelectorAll(".usage-limits").length`), 0, "the tab shows no account limits");
  });
});

test("picking a project on the app page reads that project, hides Account limits and sends Shown with the other money choices", async () => {
  await withPage(async (page) => {
    await open(page, "light", 1440, { ...ANSWERS, desktop_usage_set_money: null });
    assert.equal(await page.evaluate(`document.querySelectorAll(".usage-limits").length`), 1, "every project shows the account limits");
    await page.evaluate(`Fixture.pick("mx-project", "alpha")`);
    const reports = await page.evaluate(`Fixture.sentTo("desktop_usage_report")`);
    const metrics = await page.evaluate(`Fixture.sentTo("desktop_usage_metrics")`);
    assert.equal(reports.at(0).projectId, null);
    assert.equal(reports.at(-1).projectId, "alpha");
    assert.equal(metrics.at(-1).projectId, "alpha");
    assert.equal(await page.evaluate(`document.querySelectorAll(".usage-limits").length`), 0, "a picked project hides the account limits");
    assert.match(await page.evaluate(`document.getElementById("mount").textContent`), /Limits belong to accounts, not projects\./);
    await clickOn(page, "[role=radio]", "Shown");
    assert.deepEqual(await page.evaluate(`Fixture.sentTo("desktop_usage_set_money")`), [{ settings: { main_number: "tokens", prices: "off", project_limits: "shown" } }]);
  });
});

const textContrast = (selector) => `(() => {
  ${ratioHelpers}
  const node = document.querySelector(${JSON.stringify(selector)});
  let back = node;
  while (back && getComputedStyle(back).backgroundColor.match(/[\\d.]+/g)?.[3] === "0") back = back.parentElement;
  return ratio(luminance(getComputedStyle(node).color), luminance(getComputedStyle(back ?? document.body).backgroundColor));
})()`;

for (const theme of ["light", "dark"]) {
  test(`the Account limits card ends with the free resets and credits line, read from what is stored, at 4.5 to 1 in ${theme}`, async () => {
    await withPage(async (page) => {
      await open(page, theme);
      const line = await page.evaluate(`document.querySelector("section.usage-limits .usage-extras")?.textContent ?? ""`);
      assert.equal(line, "2 free resets (5-hour) (may break) \u00b7 Credits: $12.00 of $50.00 used this month");
      const asks = await page.evaluate(`Fixture.sentTo("desktop_usage_extras")`);
      assert.ok(asks.length >= 1, JSON.stringify(asks));
      assert.ok(asks.every((args) => args.detail === false), "Metrics never asks for the detail rows");
      const ratio = await page.evaluate(textContrast("section.usage-limits .usage-extras"));
      assert.ok(ratio >= 4.5, `free resets line contrast ${ratio} in ${theme}`);
    });
  });
}
