import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS } from "./e2e-harness.mjs";
import { USAGE_MS, handOff, onEngine } from "./codex-task-engine.mjs";

const BOTH = [
  ["Tokens", "3K", "2.4K input, 0 cache write, 600 output"],
  ["Re-read from cache", "16K", "87% of everything read; billed far lower, not in Tokens"],
];
const ONE = [
  ["Tokens", "1.5K", "1.2K input, 0 cache write, 300 output"],
  ["Re-read from cache", "8K", "87% of everything read; billed far lower, not in Tokens"],
];
const PROJECT_HEAD = ["Project", "Tokens", "Input", "Cache write", "Output", "Cache read", "Agents", "Runs", "Share of tokens"];
const PARTS = ["1.5K", "1.2K", "0", "300", "8K"];

/** Reads what a Metrics page shows: the range line, each tile's lines, the project filter's
 * value and choices, and the breakdown's header and rows. Input: the page. Output: the
 * reading, its parts null while missing. */
function metricsShown(page) {
  return page.evaluate(`(() => {
    const words = (node) => (node.innerText ?? "").replace(/\\s+/g, " ").trim();
    const table = document.querySelector(".mx-table");
    const filter = document.getElementById("mx-project");
    return {
      heading: words(document.querySelector("h1") ?? document.createElement("span")),
      line: words(document.querySelector(".mx-count") ?? document.createElement("span")),
      tiles: [...document.querySelectorAll(".mx-tile")].map((tile) => [...tile.children].map(words)),
      filter: filter ? filter.value : null,
      choices: filter ? [...filter.options].map((option) => [option.value, option.text]) : [],
      head: table ? [...table.querySelectorAll("thead th")].map(words) : null,
      rows: table ? [...table.querySelectorAll("tbody tr")].map((row) => [...row.children].map(words)) : [],
      current: document.querySelector(".app-nav a[aria-current=page]")?.getAttribute("href") ?? null,
    };
  })()`);
}

/** Waits until the page's first two tiles show the wanted lines, reloading between reads
 * since usage rows land on the engine's own schedule. Inputs: the page, the wanted tiles and
 * whether to reload. Output: the last reading. */
async function tilesShow(page, wanted, reload) {
  for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
    const shown = await metricsShown(page).catch(() => null);
    if (JSON.stringify(shown?.tiles?.slice(0, 2)) === JSON.stringify(wanted)) return shown;
    assert.ok(Date.now() < deadline, `the Metrics tiles never showed ${JSON.stringify(wanted)}: ${JSON.stringify(shown)}`);
    if (reload) await page.send("Page.reload", { ignoreCache: true });
  }
}

/** Picks a project in the app Metrics page's filter as the owner does, the empty value being
 * every project. Inputs: the page and the value. Output: none. */
async function pick(page, id) {
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById("mx-project");
    if (!select || ![...select.options].some((option) => option.value === ${JSON.stringify(id)})) return false;
    select.focus();
    select.value = ${JSON.stringify(id)};
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${JSON.stringify(id)} in the project filter`);
}

test("the sidebar's Metrics page adds up every project, and its filter shows one project's own numbers, the same as its Metrics tab (OS-6, OS-7)", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-app-metrics-", async ({ projects, runtime, me, page, site }) => {
    assert.equal(projects.length, 2);
    const [first, second] = projects;
    assert.equal(await handOff(me, page, first, runtime), "done", "the first project's Codex sub-agent finished");
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(second)}/agents` });
    await me.waitFor("the second project's Hand off form", (text) => text.includes("Hand off"));
    assert.equal(await handOff(me, page, second, runtime), "done", "the second project's Codex sub-agent finished");

    await me.click("Metrics", ".app-nav a.nav-item");
    let shown = await tilesShow(page, BOTH, true);
    await me.capture("app-metrics-all");
    assert.equal(shown.heading, "Metrics");
    assert.equal(shown.current, "#/metrics", "the sidebar marks Metrics as the open page");
    assert.match(shown.line, /^2 projects · Updated .+ · provider-reported usage per turn$/);
    assert.equal(shown.filter, "", "the page opens on every project");
    assert.deepEqual(shown.choices.map((choice) => choice[0]).sort(), ["", first, second].sort());
    assert.equal(shown.choices[0][1], "All projects");
    assert.deepEqual(shown.head, PROJECT_HEAD, "the page opens By project");
    for (const id of projects) {
      const name = shown.choices.find((choice) => choice[0] === id)[1];
      assert.ok(shown.rows.some((row) => row[0] === name && JSON.stringify(row.slice(1, 6)) === JSON.stringify(PARTS)), `${name} has its own row: ${JSON.stringify(shown.rows)}`);
    }

    await pick(page, first);
    const picked = await tilesShow(page, ONE, false);
    const name = shown.choices.find((choice) => choice[0] === first)[1];
    assert.match(picked.line, new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} · Updated .+ · provider-reported usage per turn$`));
    assert.equal(picked.filter, first);
    await me.capture("app-metrics-one");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(first)}/metrics` });
    const tab = await tilesShow(page, ONE, true);
    assert.deepEqual(picked.tiles, tab.tiles, "the filtered page shows the same tiles as the project's own Metrics tab");

    await me.click("Metrics", ".app-nav a.nav-item");
    shown = await tilesShow(page, BOTH, true);
    await pick(page, first);
    await tilesShow(page, ONE, false);
    await pick(page, "");
    shown = await tilesShow(page, BOTH, false);
    assert.equal(shown.filter, "", "All projects is picked again");
    assert.match(shown.line, /^2 projects · /);
  }, 2);
});
