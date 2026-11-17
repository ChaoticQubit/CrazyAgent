import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { AGENTS_VIEW, value } from "./e2e-harness.mjs";
import { finishGolden, onPage, onboard, open, ownerRepo, startGolden, step, stopGolden, waitUntil } from "./golden-flow.mjs";

const FLOW = "run-team";
const STAND_IN = fileURLToPath(new URL("../../../../crates/crazyagent/tests/fixtures/golden/codex", import.meta.url));
const TASK = "Write golden.md with one line about apples.";
const ANSWER = "Wrote golden.md.";
const LINE = "Apples are red.\n";
const TASK_MS = 120000;
const STEPS = [
  "start the engine and the app",
  "make the repository a project",
  "hand the task to Codex",
  "wait for the task to end",
  "see the task done",
  "find the answer and the file",
];
const run = {};

/** Reads the project's team status until the golden task has ended, as the Agents tab polls.
 * Input: the project id.
 * Output: the status, with the ended task. */
function taskEnded(project) {
  const ended = (task) => task.prompt === TASK && !["queued", "needs_pool", "running"].includes(task.status);
  return waitUntil("the task to end", () => value(run.bridge, "desktop_team_status", { id: project }), (status) => status.tasks.some(ended), TASK_MS);
}

before(async () => {
  assert.equal(statSync(STAND_IN).mode & 0o777, 0o755, "the Codex stand-in is kept as 0755");
  await startGolden(run, FLOW, { codex: STAND_IN });
});

after(() => stopGolden(run));

test("golden run a team: the owner hands a task to a Codex sub-agent from the Agents tab, the agent runs it in its own checkout, and the owner sees it done with the agent's answer", { timeout: 600000 }, async () => {
  const repo = join(run.root, "repos", "fruit-stand");
  ownerRepo(repo);
  const project = await onboard(run, repo, 3);
  step(run, STEPS[1]);
  await onPage(run, FLOW, async (page, me) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    await open(run, page, `/projects/${encodeURIComponent(project)}/agents`);
    await me.click("Team", AGENTS_VIEW);
    await me.unfold("Agents and tasks");
    await me.waitFor("the Hand off form", (text) => text.includes("Hand off"));
    const picked = await page.evaluate(`(() => {
      const select = document.getElementById(${JSON.stringify(`tg-hand-to-${project}`)});
      if (!select || select.disabled || ![...select.options].some((option) => option.value === "codex")) return false;
      select.focus();
      select.value = "codex";
      select.dispatchEvent(new Event("input", { bubbles: true }));
      select.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
    assert.ok(picked, "Codex is in the Send to list");
    await me.type(`[id="tg-hand-text-${project}"]`, TASK);
    await me.click("Hand off", "form.tg-hand button");
    await me.waitFor("the hand-off note", (text) => text.includes("Task handed off."));
    step(run, STEPS[2]);
    await taskEnded(project);
    step(run, STEPS[3]);
    await me.click(null, '[role=radiogroup][aria-label="Show"] [role=radio]:last-child');
    const row = `[...document.querySelectorAll('ul[aria-label="Team tasks"] li.pj-task')].map((item) => item.innerText).find((text) => text.includes(${JSON.stringify(TASK)})) ?? null`;
    await waitUntil("the task row to say Done with the answer", () => page.evaluate(row), (text) => text !== null && text.includes("Done") && text.includes(ANSWER));
    await me.capture("golden-run-team-done");
    step(run, STEPS[4]);
  });
  const status = await value(run.bridge, "desktop_team_status", { id: project });
  const task = status.tasks.find((item) => item.prompt === TASK);
  assert.deepEqual([task.status, task.harness], ["done", "codex"], JSON.stringify(task));
  assert.ok((task.result ?? "").includes(ANSWER), `the owner hears the answer: ${JSON.stringify(task.result)}`);
  const agent = status.agents.find((item) => item.id === task.agent);
  assert.deepEqual([agent?.role, agent?.harness], ["sub", "codex"], JSON.stringify(agent));
  assert.ok(typeof agent.worktree === "string", "the agent works in its own checkout");
  const written = join(agent.worktree, "golden.md");
  assert.equal(existsSync(written) ? readFileSync(written, "utf8") : null, LINE, "the agent wrote its file in its own checkout");
  step(run, STEPS[5]);
  const record = finishGolden(run);
  assert.deepEqual([record?.steps.map((done) => done.step), record?.outcome], [STEPS, "passed"], "the run record names each step in order");
});
