import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS } from "./e2e-harness.mjs";
import { USAGE_MS, handOff, onEngine, pricesOff, tileParts, turnsOf } from "./codex-task-engine.mjs";

const CACHE_LINE = "87% of everything read; billed far lower, not in Tokens";
const PROJECT_HEAD = ["Project", "Tokens", "Input", "Cache write", "Output", "Cache read", "Agents", "Runs", "Merges", "Share of tokens"];

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

/** Says whether an every-project reading has both projects' sub-agent turns in: each project
 * row lists two agents, Onboarding and its Codex sub-agent, and is one stand-in turn per run,
 * and the tiles add up those runs. Input: a reading. Output: true when it has. */
function everyProject(shown) {
  const rows = (shown?.rows ?? []).filter((row) => row[0] !== "Total");
  const { parts, runs } = tileParts(shown?.tiles);
  return rows.length === 2 && rows.every((row) => row[6] === "2" && turnsOf(row.slice(1, 6), Number(row[7])))
    && turnsOf(parts, runs) && runs === rows.reduce((sum, row) => sum + Number(row[7]), 0);
}

/** Makes the check that a reading's tiles are one project's runs, one stand-in turn each.
 * Input: that project's run count. Output: the check, taking a reading. */
function oneProject(runs) {
  return (shown) => {
    const tiles = tileParts(shown?.tiles);
    return tiles.runs === runs && turnsOf(tiles.parts, tiles.runs);
  };
}

/** Waits until a reading of the page passes the check, reloading between reads since usage
 * rows land on the engine's own schedule. Inputs: the page, the check, what it waits for and
 * whether to reload. Output: the last reading. */
async function tilesShow(page, ready, what, reload) {
  for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
    const shown = await metricsShown(page).catch(() => null);
    if (ready(shown)) return shown;
    assert.ok(Date.now() < deadline, `the Metrics page never showed ${what}: ${JSON.stringify(shown)}`);
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
  await onEngine(t, "ca-app-metrics-", async ({ projects, runtime, me, page, site, bridge }) => {
    await pricesOff(bridge);
    assert.equal(projects.length, 2);
    const [first, second] = projects;
    assert.equal(await handOff(me, page, first, runtime), "done", "the first project's Codex sub-agent finished");
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(second)}/agents` });
    await me.unfold("Agents and tasks");
    await me.waitFor("the second project's Hand off form", (text) => text.includes("Hand off"));
    assert.equal(await handOff(me, page, second, runtime), "done", "the second project's Codex sub-agent finished");

    await me.click("Metrics", ".app-nav a.nav-item");
    let shown = await tilesShow(page, everyProject, "both projects added up", true);
    await me.capture("app-metrics-all");
    assert.equal(shown.tiles[1][2], CACHE_LINE);
    assert.deepEqual(shown.tiles.map((tile) => tile[0]), ["Tokens", "Re-read from cache", "Runs", "Tasks", "Merges", "Lines"], "the same six tiles as the project tab (mockup note 13)");
    assert.deepEqual(shown.tiles[4].slice(1), ["0", "0 into main, 0 undone"], "nothing was merged yet");
    assert.equal(shown.heading, "Metrics");
    assert.equal(shown.current, "#/metrics", "the sidebar marks Metrics as the open page");
    assert.match(shown.line, /^2 projects · Updated .+ · provider-reported usage per turn$/);
    assert.equal(shown.filter, "", "the page opens on every project");
    assert.deepEqual(shown.choices.map((choice) => choice[0]).sort(), ["", first, second].sort());
    assert.equal(shown.choices[0][1], "All projects");
    assert.deepEqual(shown.head, PROJECT_HEAD, "the page opens By project");
    for (const id of projects) {
      const name = shown.choices.find((choice) => choice[0] === id)[1];
      assert.ok(shown.rows.some((row) => row[0] === name), `${name} has its own row: ${JSON.stringify(shown.rows)}`);
    }

    const name = shown.choices.find((choice) => choice[0] === first)[1];
    const one = oneProject(Number(shown.rows.find((row) => row[0] === name)[7]));
    await pick(page, first);
    const picked = await tilesShow(page, one, `${name}'s own runs`, false);
    assert.match(picked.line, new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} · Updated .+ · provider-reported usage per turn$`));
    assert.equal(picked.filter, first);
    await me.capture("app-metrics-one");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(first)}/metrics` });
    const tab = await tilesShow(page, one, `${name}'s Metrics tab`, true);
    assert.deepEqual(picked.tiles, tab.tiles, "the filtered page shows the same tiles as the project's own Metrics tab");

    await me.click("Metrics", ".app-nav a.nav-item");
    shown = await tilesShow(page, everyProject, "both projects added up", true);
    await pick(page, first);
    await tilesShow(page, one, `${name}'s own runs`, false);
    await pick(page, "");
    shown = await tilesShow(page, everyProject, "both projects added up again", false);
    assert.equal(shown.filter, "", "All projects is picked again");
    assert.match(shown.line, /^2 projects · /);
  }, 2);
});
