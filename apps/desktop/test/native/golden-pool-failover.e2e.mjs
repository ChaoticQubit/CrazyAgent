import assert from "node:assert/strict";
import { statSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { AGENTS_VIEW, value } from "./e2e-harness.mjs";
import { LIMITED, READY, finishGolden, onPage, onboard, open, ownerRepo, startGolden, step, stopGolden, waitUntil } from "./golden-flow.mjs";

const FLOW = "pool-failover";
const STAND_IN = fileURLToPath(new URL("../../../../crates/crazyagent/tests/fixtures/golden/codex", import.meta.url));
const TASK = "Write golden.md with one line about apples.";
const ANSWER = "Wrote golden.md.";
const TASK_MS = 180000;
const STEPS = [
  "start the engine and the app",
  "make the repository a project",
  "hand the task to Codex",
  "wait for the task to end",
  "see the next member run it",
  "find it run on the next member",
];
const run = {};

/** The label the Agents tab shows for an account, as the app builds it from the account list:
 * its name, with its id when another account shares the name, else the id itself.
 * Inputs: the listed accounts and the id. Output: the label. */
function accountLabel(accounts, id) {
  const account = accounts.find((item) => item.id === id);
  if (!account) return id;
  return accounts.filter((item) => item.name === account.name).length === 1 ? account.name : `${account.name} (${id})`;
}

before(async () => {
  assert.equal(statSync(STAND_IN).mode & 0o777, 0o755, "the Codex stand-in is kept as 0755");
  await startGolden(run, FLOW, { codex: STAND_IN, limitedFirst: true });
});

after(() => stopGolden(run));

test("golden pool failover: a Codex sub-agent's first pool member hits its rate limit, and the owner sees the task done on the next member", { timeout: 600000 }, async () => {
  const repo = join(run.root, "repos", "fruit-stand");
  ownerRepo(repo);
  const project = await onboard(run, repo, 3);
  step(run, STEPS[1]);
  const handed = await value(run.bridge, "desktop_team_delegate", { id: project, harness: "codex", prompt: TASK, generation: 6 });
  assert.equal(handed.outcome, "applied", `the task is handed off: ${JSON.stringify(handed)}`);
  step(run, STEPS[2]);
  const ended = (task) => task.prompt === TASK && !["queued", "needs_pool", "running"].includes(task.status);
  const status = await waitUntil("the task to end", () => value(run.bridge, "desktop_team_status", { id: project }), (read) => read.tasks.some(ended), TASK_MS);
  const task = status.tasks.find(ended);
  assert.deepEqual([task.status, task.attempts], ["done", 2], `the rate limit sent the task back once and the second try finished it: ${JSON.stringify(task)}`);
  step(run, STEPS[3]);
  const accounts = (await value(run.bridge, "desktop_list_accounts", { after: null, limit: 50 })).accounts;
  const ready = accountLabel(accounts, READY);
  assert.notEqual(ready, accountLabel(accounts, LIMITED), "the two pool members read apart");
  await onPage(run, FLOW, async (page, me) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    await open(run, page, `/projects/${encodeURIComponent(project)}/agents`);
    await me.click("Team", AGENTS_VIEW);
    await me.unfold("Agents and tasks");
    await me.click(null, '[role=radiogroup][aria-label="Show"] [role=radio]:last-child');
    const agents = `[...document.querySelectorAll("table.pj-table")].filter((table) => table.querySelector("caption")?.textContent === "Team agents").flatMap((table) => [...table.querySelectorAll("tbody tr")].map((row) => [...row.cells].map((cell) => cell.innerText.trim())))`;
    await waitUntil("the Team agents table to show the Codex agent on the next member", () => page.evaluate(agents), (rows) => rows.some((cells) => cells.includes(ready) && cells.some((cell) => cell.includes("Codex"))));
    const row = `[...document.querySelectorAll('ul[aria-label="Team tasks"] li.pj-task')].map((item) => item.innerText).find((text) => text.includes(${JSON.stringify(TASK)})) ?? null`;
    await waitUntil("the task row to say Done with the answer", () => page.evaluate(row), (text) => text !== null && text.includes("Done") && text.includes(ANSWER));
    await me.capture("golden-pool-failover-done");
    step(run, STEPS[4]);
  });
  const settled = await value(run.bridge, "desktop_team_status", { id: project });
  const finished = settled.tasks.find((item) => item.prompt === TASK);
  assert.ok((finished.result ?? "").includes(ANSWER), `the owner hears the answer: ${JSON.stringify(finished.result)}`);
  const agent = settled.agents.find((item) => item.id === finished.agent);
  assert.deepEqual([agent?.role, agent?.harness, agent?.account], ["sub", "codex", READY], `the second try ran on the next pool member: ${JSON.stringify(agent)}`);
  step(run, STEPS[5]);
  const record = finishGolden(run);
  assert.deepEqual([record?.steps.map((done) => done.step), record?.outcome], [STEPS, "passed"], "the run record names each step in order");
});
