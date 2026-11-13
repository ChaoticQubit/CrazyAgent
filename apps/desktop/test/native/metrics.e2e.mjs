import assert from "node:assert/strict";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, value } from "./e2e-harness.mjs";
import { TASK, USAGE_MS, handOff, onEngine, pricesOff, tileParts, turnsOf } from "./codex-task-engine.mjs";

const CACHE_LINE = "87% of everything read; billed far lower, not in Tokens";
const AGENT_HEAD = ["Agent", "Tokens", "Input", "Cache write", "Output", "Cache read", "Turns", "graphify", "ctx_*", "Skills", "Share of tokens"];
const RUN_HEAD = ["Run", "Tokens", "Input", "Cache write", "Output", "Cache read", "Agent", "Ended", "Result", "Share of tokens"];
const PARTS = ["1.5K", "1.2K", "0", "300", "8K"];

/** Reads the turns the engine counted for the project's Codex sub-agents. Inputs: the engine
 * root and the project. Output: the sum, 0 before any agent exists. */
function codexTurns(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return store.prepare("SELECT COALESCE(SUM(turns),0) AS turns FROM team_agents WHERE project=? AND harness='codex' AND role<>'master'").get(project).turns;
  } finally {
    store.close();
  }
}

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
  await onEngine(t, "ca-metrics-", async ({ project, runtime, me, page, site, bridge }) => {
    await pricesOff(bridge);
    assert.equal(await handOff(me, page, project, runtime), "done", "the Codex sub-agent finished the task");
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/metrics` });
    let shown = null;
    for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
      shown = await metricsShown(page).catch(() => null);
      const { parts, runs } = tileParts(shown?.tiles);
      const codex = shown?.rows?.find((row) => row[0].endsWith(" · Codex"));
      if (turnsOf(parts, runs) && JSON.stringify(codex?.slice(1, 6)) === JSON.stringify(PARTS)) break;
      assert.ok(Date.now() < deadline, `the Metrics tiles never added up one stand-in turn per run with the Codex agent's turn in: ${JSON.stringify(shown)}`);
      await page.send("Page.reload", { ignoreCache: true });
    }
    await me.capture("metrics-tab");
    assert.equal(shown.tiles[1][2], CACHE_LINE);
    assert.match(shown.line, /^This project only · Updated .+ · provider-reported usage per turn$/);
    assert.equal(shown.picker, false, "the project tab has no project picker");
    assert.deepEqual(shown.tiles.map((tile) => tile[0]), ["Tokens", "Re-read from cache", "Runs", "Tasks", "Merges", "Lines"], "six tiles (mockup note 9)");
    assert.deepEqual(shown.tiles[4].slice(1), ["0", "0 into main, 0 undone"], "nothing was merged yet");
    assert.deepEqual(shown.tiles[3].slice(1), ["1 done", "0 waiting, 0 failed"], "the finished task counts as done");
    assert.equal(shown.chart, "Tokens per day: input, cache write and output", "the tab opens on 7 days, one bar a day");
    assert.ok(shown.bars >= 1, "the chart draws a bar for today");
    assert.deepEqual(shown.head, AGENT_HEAD, "By agent shows the headline, the four parts, the turns and the tool counts");
    const agent = shown.rows.find((row) => row[0].endsWith(" · Codex"));
    assert.ok(agent, `the Codex agent has a row with the headline Task 13 counts: ${JSON.stringify(shown.rows)}`);
    assert.deepEqual(agent.slice(1, 6), PARTS);
    const turns = codexTurns(runtime, project);
    assert.ok(turns >= 1, `the engine counted the Codex agent's finished turn: ${turns}`);
    assert.equal(agent[6], String(turns), `the Codex row's Turns is the engine's count: ${JSON.stringify(agent)}`);
    const onboarding = shown.rows.find((row) => row[0] === "Onboarding");
    assert.ok(onboarding, `onboarding's own runs have their row: ${JSON.stringify(shown.rows)}`);
    assert.equal(onboarding[6], "", `onboarding's row shows no turns: ${JSON.stringify(onboarding)}`);
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

test("/usage in a project's console opens that project's own Metrics tab, not every project's page (DEC-132 item 1)", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-metrics-usage-command-", async ({ project, me, page, site, bridge }) => {
    await pricesOff(bridge);
    const receipt = await value(bridge, "desktop_team_master_live_terminal", { id: project, on: false, generation: 90 });
    assert.equal(receipt.outcome, "applied", `Live terminal turns Off: ${JSON.stringify(receipt)}`);
    for (const deadline = Date.now() + USAGE_MS; (await value(bridge, "desktop_team_status", { id: project })).master_live_terminal !== false; await pause(POLL_MS * 4)) {
      assert.ok(Date.now() < deadline, "the engine never kept Live terminal Off");
    }
    const prompt = 'textarea[aria-label="Message the master"]';
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/conversation` });
    await page.send("Page.reload");
    for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
      if (await page.evaluate(`document.querySelector(${JSON.stringify(prompt)}) !== null`).catch(() => false)) break;
      assert.ok(Date.now() < deadline, "the project's console never showed its prompt");
    }
    await me.type(prompt, "/usage");
    await me.waitFor("the /usage command in the menu", (text) => text.includes("Open this project's Metrics"));
    await me.enter();
    let shown = null;
    for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
      shown = await metricsShown(page).catch(() => null);
      if (/^This project only · /.test(shown?.line ?? "")) break;
      assert.ok(Date.now() < deadline, `/usage never opened the project's Metrics tab: ${JSON.stringify(shown)}`);
    }
    await me.capture("metrics-usage-command");
    assert.equal(await page.evaluate("location.hash"), `#/projects/${encodeURIComponent(project)}/metrics`);
    assert.equal(shown.picker, false, "the project's own tab has no project picker");
  });
});
