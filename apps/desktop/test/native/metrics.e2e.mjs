import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS } from "./e2e-harness.mjs";
import { TASK, USAGE_MS, handOff, onEngine } from "./codex-task-engine.mjs";

const TILES = [
  ["Tokens", "1.5K", "1.2K input, 0 cache write, 300 output"],
  ["Re-read from cache", "8K", "87% of everything read; billed far lower, not in Tokens"],
];
const AGENT_HEAD = ["Agent", "Tokens", "Input", "Cache write", "Output", "Cache read", "graphify", "ctx_*", "Skills", "Share of tokens"];
const RUN_HEAD = ["Run", "Tokens", "Input", "Cache write", "Output", "Cache read", "Agent", "Ended", "Result", "Share of tokens"];
const PARTS = ["1.5K", "1.2K", "0", "300", "8K"];

/** Reads what the project's Metrics tab shows: the range line, each tile's lines, the chart's
 * label, the breakdown's header and rows, and whether a project picker is on the page.
 * Input: the page. Output: the reading, its parts null while missing. */
function metricsShown(page) {
  return page.evaluate(`(() => {
    const words = (node) => (node.innerText ?? "").replace(/\\s+/g, " ").trim();
    const table = document.querySelector(".mx-table");
    return {
      line: words(document.querySelector(".mx-count") ?? document.createElement("span")),
      tiles: [...document.querySelectorAll(".mx-tile")].map((tile) => [...tile.children].map(words)),
      chart: document.querySelector("svg.mx-chart")?.getAttribute("aria-label") ?? null,
      bars: document.querySelectorAll("svg.mx-chart rect.in").length,
      head: table ? [...table.querySelectorAll("thead th")].map(words) : null,
      rows: table ? [...table.querySelectorAll("tbody tr")].map((row) => [...row.children].map(words)) : [],
      picker: document.querySelector("#pj-tab-panel select") !== null || document.body.innerText.includes("All projects"),
    };
  })()`);
}

test("the project Metrics tab shows this project's tokens with the headline from Task 13, every part, its runs and tasks, and no project picker (OS-6, OS-7)", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-metrics-", async ({ project, runtime, me, page, site }) => {
    assert.equal(await handOff(me, page, project, runtime), "done", "the Codex sub-agent finished the task");
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/metrics` });
    let shown = null;
    for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
      shown = await metricsShown(page).catch(() => null);
      if (JSON.stringify(shown?.tiles?.slice(0, 2)) === JSON.stringify(TILES)) break;
      assert.ok(Date.now() < deadline, `the Metrics tiles never showed ${JSON.stringify(TILES)}: ${JSON.stringify(shown)}`);
      await page.send("Page.reload", { ignoreCache: true });
    }
    await me.capture("metrics-tab");
    assert.match(shown.line, /^This project only · Updated .+ · provider-reported usage per turn$/);
    assert.equal(shown.picker, false, "the project tab has no project picker");
    assert.deepEqual(shown.tiles.map((tile) => tile[0]), ["Tokens", "Re-read from cache", "Runs", "Tasks", "Lines"]);
    assert.deepEqual(shown.tiles[3].slice(1), ["1 done", "0 waiting, 0 failed"], "the finished task counts as done");
    assert.equal(shown.chart, "Tokens per two hours: input, cache write and output");
    assert.ok(shown.bars >= 1, "the chart draws a bar for today");
    assert.deepEqual(shown.head, AGENT_HEAD, "By agent shows the headline, the four parts and the tool counts, and no Turns");
    const agent = shown.rows.find((row) => row[1] === "1.5K");
    assert.ok(agent, `the Codex agent has a row with the headline Task 13 counts: ${JSON.stringify(shown.rows)}`);
    assert.deepEqual(agent.slice(1, 6), PARTS);
    const total = shown.rows.at(-1);
    assert.equal(total[0], "Total");
    assert.match(total.at(-1), /^cache read 87% of all read$/);

    await me.click("By run", "[role=radio]");
    await me.waitFor("the By run table", (text) => text.includes("Ended"));
    shown = await metricsShown(page);
    assert.deepEqual(shown.head, RUN_HEAD);
    const run = shown.rows.find((row) => row[0].endsWith(TASK));
    assert.ok(run, `the run is titled by its task: ${JSON.stringify(shown.rows)}`);
    assert.match(run[0], /^#\d+ /);
    assert.deepEqual(run.slice(1, 6), PARTS);
    assert.equal(run[8], "Done");
    const words = await me.seen();
    assert.ok(!words.includes("9.5K") && !words.includes("9.2K"), "no number adds the cache re-reads into the tokens");
  });
});
