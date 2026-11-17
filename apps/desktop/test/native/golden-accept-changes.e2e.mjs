import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { value } from "./e2e-harness.mjs";
import { finishGolden, git, onPage, onboard, open, ownerRepo, startGolden, step, stopGolden, waitUntil } from "./golden-flow.mjs";

const FLOW = "accept-changes";
const STAND_IN = fileURLToPath(new URL("../../../../crates/crazyagent/tests/fixtures/golden/codex", import.meta.url));
const IDENTITY = ["Golden Owner", "golden@example.invalid"];
const TASK = "Write golden.md with one line about apples.";
const LINE = "Apples are red.\n";
const SET_MS = 120000;
const ACCEPT = `[...document.querySelectorAll("button")].find((button) => button.textContent.trim() === "Accept")?.disabled ?? null`;
const STEPS = [
  "start the engine and the app",
  "make the repository a project",
  "hand the task to Codex",
  "wait for the change set",
  "read the diff",
  "accept the change set",
  "find the change on main",
];
const run = {};

/** Reads one change set of the project as the Changes tab does.
 * Inputs: the project id and a check on the set.
 * Output: the first set the check accepts, or undefined. */
async function changeSet(project, check) {
  return (await value(run.bridge, "desktop_changes", { projectId: project })).find(check);
}

before(async () => {
  assert.equal(statSync(STAND_IN).mode & 0o777, 0o755, "the Codex stand-in is kept as 0755");
  await startGolden(run, FLOW, { codex: STAND_IN });
});

after(() => stopGolden(run));

test("golden accept a change set: the owner reads a Codex sub-agent's change set in the Changes tab and accepts it as one commit of their own on main", { timeout: 600000 }, async () => {
  const repo = join(run.root, "repos", "fruit-stand");
  const start = ownerRepo(repo, { identity: IDENTITY });
  const project = await onboard(run, repo, 3);
  step(run, STEPS[1]);
  const handed = await value(run.bridge, "desktop_team_delegate", { id: project, harness: "codex", prompt: TASK, generation: 6 });
  assert.equal(handed.outcome, "applied", `the task is handed off: ${JSON.stringify(handed)}`);
  step(run, STEPS[2]);
  const set = await waitUntil("the agent's change set", () => changeSet(project, (change) => change.state === "pending"), (found) => found !== undefined, SET_MS);
  assert.equal(set.agent.kind, "sub", "a sub-agent's turn opened it");
  step(run, STEPS[3]);
  await onPage(run, FLOW, async (page, me) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    await open(run, page, `/projects/${encodeURIComponent(project)}/changes`);
    await me.waitFor("the agent's change set in the list", (text) => /Ready to review/.test(text));
    await me.follow(".ch-row");
    await me.waitFor("the change set's files and diff", (text) => text.includes("Added golden.md") && text.includes("Apples are red."));
    const diff = await page.evaluate("document.querySelector('pre.ch-diff')?.textContent ?? null");
    assert.ok(diff?.includes("+Apples are red."), "the diff is the engine's text");
    step(run, STEPS[4]);
    await waitUntil("Accept to be offered once the diff's end was on screen", () => page.evaluate(ACCEPT), (held) => held === false);
    await me.click("Accept");
    await me.waitFor("the accept question", (text) => /Accept these changes\?/.test(text));
    await me.click("Accept", "dialog button");
    const accepted = await me.waitFor("the landed change set", (text) => /Accepted as [0-9a-f]{7} on main\./.test(text));
    await me.capture("golden-accept-changes-accepted");
    const landed = git(repo, "rev-parse", "HEAD");
    assert.ok(accepted.includes(`Accepted as ${landed.slice(0, 7)} on main.`), "the note names the commit on the owner's branch");
    step(run, STEPS[5]);
  });
  const landed = git(repo, "rev-parse", "HEAD");
  assert.equal(git(repo, "rev-parse", `${landed}^`), start, "one squash commit on top of the owner's branch");
  assert.equal(git(repo, "log", "-1", "--format=%an <%ae>"), `${IDENTITY[0]} <${IDENTITY[1]}>`, "the owner's own Git identity authored it");
  assert.equal(git(repo, "status", "--porcelain"), "", "the owner's checkout matches the commit");
  const written = join(repo, "golden.md");
  assert.equal(existsSync(written) ? readFileSync(written, "utf8") : null, LINE, "the agent's file is on main");
  const settled = await changeSet(project, (change) => change.id === set.id);
  assert.equal(settled?.state, "accepted", "the set reads accepted");
  step(run, STEPS[6]);
  const record = finishGolden(run);
  assert.deepEqual([record?.steps.map((done) => done.step), record?.outcome], [STEPS, "passed"], "the run record names each step in order");
});
