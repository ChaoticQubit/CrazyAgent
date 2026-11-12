import assert from "node:assert/strict";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { onEngine } from "./codex-task-engine.mjs";

const OLDER = 30;
const ASKED = "E2E agents ship on Fridays.";
const OLDEST = "E2E note 01 is the oldest.";
const AGENT = "Ada (@lead-a)";

/** Puts the owner's older notes and one memory an agent asked to keep in the engine's own
 * memory table, the rows its remember tool writes under Ask me first, the waiting one newest.
 * Inputs: the engine root and the project. Output: none. */
function seedNotes(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const insert = store.prepare("INSERT INTO memory_library(id, project, origin, text) VALUES (?, ?, ?, ?)");
    for (let index = 1; index <= OLDER; index += 1) {
      const label = String(index).padStart(2, "0");
      insert.run(`memory-e2e-${label}`, project, JSON.stringify({ kind: "written" }), index === 1 ? OLDEST : `E2E note ${label}.`);
    }
    insert.run("memory-e2e-asked", project, JSON.stringify({ kind: "requested", agent: AGENT }), ASKED);
  } finally {
    store.close();
  }
}

/** Reads one Your notes row by its exact text: whether it shows, its words and whether it
 * offers Approve. Inputs: the page and the note text. Output: { found, words, approve }. */
function noteRow(page, text) {
  return page.evaluate(`(() => {
    const row = [...document.querySelectorAll("li.pj-memory")].find((item) => item.querySelector(".pj-note-text")?.innerText.trim() === ${JSON.stringify(text)});
    if (!row) return { found: false, words: "", approve: false };
    return { found: true, words: row.innerText.replace(/\\s+/g, " ").trim(), approve: [...row.querySelectorAll("button")].some((button) => button.innerText.trim() === "Approve") };
  })()`);
}

/** Waits until a reading of the page holds. Inputs: what is awaited, the reading and its
 * check. Output: the last reading. */
async function until(what, read, check) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const reading = await read();
    if (check(reading)) return reading;
    assert.ok(Date.now() < deadline, `${what}: ${JSON.stringify(reading)}`);
  }
}

test("a memory an agent asked to keep is approved in Context, Project memory, Show older pages in place and Memory settings opens Settings, Memory", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-project-memory-", async ({ project, runtime, me, page, site, bridge }) => {
    seedNotes(runtime, project);
    const first = await value(bridge, "desktop_memories", { project, search: null, before: null });
    assert.ok(first.items.some((memory) => memory.text === ASKED && memory.origin === "requested"), "the engine lists the agent's memory as waiting");
    assert.notEqual(first.next, null, "the notes run past one page");
    assert.ok(!first.items.some((memory) => memory.text === OLDEST), "the oldest note is on a later page");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/context` });
    await me.click("Project memory", "button.pj-titem");
    const shown = await me.waitFor("the waiting count in Project memory", (text) => text.includes("1 memory waits for your approval."));
    assert.ok(!shown.includes("Settings, Memory"), "the project never sends the owner to Settings, Memory");
    const waiting = await noteRow(page, ASKED);
    assert.ok(waiting.found && waiting.approve, `the agent's memory offers Approve in its row: ${JSON.stringify(waiting)}`);
    assert.match(waiting.words, /Needs approval/u);
    await me.capture("project-memory-waiting");

    await me.click("Approve", "li.pj-memory button");
    await until("the approved memory never read Saved", () => noteRow(page, ASKED), (row) => row.found && !row.approve && /\bSaved\b/u.test(row.words));
    await me.waitFor("the approval toast", (text) => text.includes("Memory approved. Agents get it from their next launch."));
    await me.waitFor("the waiting count gone", (text) => !/memor(?:y waits|ies wait) for your approval/u.test(text));
    const approved = await value(bridge, "desktop_memories", { project, search: null, before: null });
    assert.equal(approved.items.find((memory) => memory.text === ASKED)?.origin, "agent", "the engine keeps the memory for agents");
    await me.capture("project-memory-approved");

    assert.equal((await noteRow(page, OLDEST)).found, false, "the oldest note waits behind Show older");
    for (let pages = 0; !(await noteRow(page, OLDEST)).found; pages += 1) {
      assert.ok(pages < 3, "Show older never reached the oldest note");
      await me.click("Show older", "button");
      await until("Show older never finished loading", () => page.evaluate("document.querySelector('section[aria-labelledby=pj-mem-notes] [aria-busy=true]') !== null"), (loading) => !loading);
    }
    assert.ok((await noteRow(page, ASKED)).found, "the newest notes stay above the older ones");
    assert.ok((await page.evaluate("location.hash")).endsWith("/context"), "Show older pages in place");
    await me.capture("project-memory-older");

    await me.click("Memory settings", "button");
    await until("Memory settings never opened Settings, Memory", () => page.evaluate("location.hash"), (hash) => hash.endsWith("/settings/memory"));
    await me.waitFor("the agent writes setting in Settings, Memory", (text) => text.includes("When an agent saves a memory"));
  }));
