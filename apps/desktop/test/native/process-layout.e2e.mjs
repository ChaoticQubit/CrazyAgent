import assert from "node:assert/strict";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { TASK, handOff, onEngine } from "./codex-task-engine.mjs";

const FOLLOW_HEAD = ["Agent", "Graph before edits", "Context tools", "Skills used", "Flags"];

/** Reads the team agent that ran the owner's task and its status. Inputs: the engine root
 * and the project. Output: { agent, status }, or null before the task has an agent. */
function taskAgent(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const row = store.prepare("SELECT a.id AS agent, a.status AS status FROM team_tasks t JOIN team_agents a ON a.id=t.agent WHERE t.project=? AND t.prompt=?").get(project, TASK);
    return row ? { agent: Number(row.agent), status: row.status } : null;
  } finally {
    store.close();
  }
}

/** Reads how the project's Process page is laid out: the main column's and the side column's
 * panel titles, the instructions view picked, whether the editor shows, the follow table's
 * header and agent names, and the finished button. Input: the page. Output: the reading. */
function processShown(page) {
  return page.evaluate(`(() => {
    const words = (node) => (node?.innerText ?? "").replace(/\\s+/g, " ").trim();
    const layout = document.querySelector(".pr");
    const main = layout?.firstElementChild;
    const side = layout?.querySelector("aside.pr-side");
    const table = document.querySelector(".pr-table");
    return {
      main: main ? [...main.querySelectorAll(":scope > section.panel .panel-head h2")].map(words) : null,
      side: side ? [...side.querySelectorAll("h2, h3")].map(words) : null,
      picked: words(document.querySelector('[aria-label="How the instructions show"] [aria-checked="true"]')),
      editor: document.getElementById("process-project") !== null,
      head: table ? [...table.querySelectorAll("thead th")].map(words) : null,
      agents: table ? [...table.querySelectorAll("tbody .pr-agent")].map(words) : [],
      finished: words([...document.querySelectorAll(".pr-link")].find((node) => /finished/.test(node.innerText ?? ""))),
      counts: document.body.innerText.includes("Workflow adoption") || document.querySelector(".adoption-table") !== null,
    };
  })()`);
}

test("the project Process page groups instructions and who follows them beside Delivery and How a task runs, folds finished agents and sends counts to Metrics (OS-6)", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-process-layout-", async ({ project, runtime, me, page, site, bridge }) => {
    assert.equal(await handOff(me, page, project, runtime), "done", "the Codex sub-agent finished the task");
    const ran = taskAgent(runtime, project);
    assert.ok(ran, "the task names the agent that ran it");
    const receipt = await value(bridge, "desktop_team_stop_agent", { id: project, agent: ran.agent, generation: 6 });
    assert.equal(receipt.outcome, "applied", `the owner stops the finished agent: ${JSON.stringify(receipt)}`);
    for (const deadline = Date.now() + STEP_MS; taskAgent(runtime, project)?.status !== "stopped"; await pause(POLL_MS * 5)) {
      assert.ok(Date.now() < deadline, `the agent never stopped: ${JSON.stringify(taskAgent(runtime, project))}`);
    }

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/process` });
    await me.waitFor("the Process page's follow table", (text) => text.includes("Are agents following it"));
    let shown = await processShown(page);
    await me.capture("process-layout");
    assert.deepEqual(shown.main, ["Instructions", "Are agents following it"], "the main column holds the instructions, then who follows them");
    assert.deepEqual(shown.side, ["Delivery", "How a task runs"], "the side column holds delivery and how a task runs");
    assert.equal(shown.picked, "As agents read it", "the instructions open as agents read them");
    assert.equal(shown.editor, false, "the editor waits behind Edit");
    assert.deepEqual(shown.head, FOLLOW_HEAD);
    assert.equal(shown.counts, false, "the counts table moved to Metrics");
    assert.equal(shown.finished, "Show finished (1)", "the stopped agent waits behind Show finished");
    const before = shown.agents.length;

    await me.click("Show finished (1)", ".pr-link");
    await me.waitFor("the finished agent's row", (text) => text.includes("Hide finished"));
    shown = await processShown(page);
    assert.equal(shown.agents.length, before + 1, "Show finished adds the stopped agent's row");

    await me.click("Edit", "[role=radio]");
    await me.waitFor("the editor", (text) => text.includes("Use app default"));
    assert.equal((await processShown(page)).editor, true, "Edit shows the project's own text");

    await me.click("Open Metrics", "button");
    for (const deadline = Date.now() + STEP_MS; !(await page.evaluate("location.hash")).endsWith("/metrics"); await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "Open Metrics never opened the Metrics tab");
    }
  });
});
