import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { value } from "./e2e-harness.mjs";
import { POOL, finishGolden, hashUntil, onPage, open, startGolden, step, stopGolden, waitUntil } from "./golden-flow.mjs";

const FLOW = "add-project";
const NAME = "Tide Gauge";
const ABOUT = "Reads buoy sensors and raises alerts.";
const OWN_BRANCH = 'input[name=np-access][value="own_branch"]';
const STEPS = [
  "start the engine and the app",
  "open New project",
  "fill Basics",
  "pass Documents",
  "turn research and planning off",
  "keep the pool",
  "choose Own branch",
  "open the new project",
  "find the project set up",
];
const run = {};

/** Reads one bridge value until the check accepts it, as the app's follow-ups land after the
 * page moves on. Inputs: what is awaited, the command, its arguments and the check. Output:
 * the value accepted. */
function bridgeUntil(what, cmd, args, done) {
  return waitUntil(what, () => value(run.bridge, cmd, args), done);
}

/** Reads the projects the golden pool serves from a pools page.
 * Input: the page.
 * Output: their ids, or null when the pool is not on it. */
function served(page) {
  const pool = page.items.find((record) => record.kind === "pool" && record.pool.id === POOL);
  return pool ? pool.pool.projects : null;
}

before(() => startGolden(run, FLOW));

after(() => stopGolden(run));

test("golden add a project: New project makes a project the engine lists, set up in the projects folder, served by its pool, at Own branch, with its job dismissed", { timeout: 300000 }, async () => {
  let id = null;
  await onPage(run, FLOW, async (page, me) => {
    await open(run, page, "/projects/new");
    await me.waitFor("New Project's Basics", (text) => text.includes("Basics") && text.includes("Project name"));
    step(run, STEPS[1]);
    await me.type("#np-name", NAME);
    await me.type("#np-about", ABOUT);
    await me.follow("#np-publish");
    assert.equal(await page.evaluate(`document.querySelector("#np-publish")?.getAttribute("aria-checked") ?? null`), "false", "publishing on GitHub is off");
    await me.click("Next", "button");
    await me.waitFor("the Documents step", (text) => text.includes("Choose files"));
    step(run, STEPS[2]);
    await me.click("Next", "button");
    await me.waitFor("the Work step", (text) => text.includes("What should agents start on?"));
    step(run, STEPS[3]);
    const on = `[...document.querySelectorAll("button[role=switch]")].filter((item) => item.getAttribute("aria-checked") === "true").map((item) => item.id)`;
    for (const switched of await page.evaluate(on)) await me.follow(`#${switched}`);
    assert.deepEqual(await page.evaluate(on), [], "no research agent and no roadmap planning start with the project");
    await me.click("Next", "button");
    await me.waitFor("the Pool step", (text) => text.includes(POOL));
    step(run, STEPS[4]);
    assert.equal(await page.evaluate(`document.querySelector('input[name=np-pool][value="${POOL}"]')?.checked ?? null`), true, "the golden pool is chosen");
    await me.click("Next", "button");
    await me.waitFor("the Team step", (text) => text.includes("Team") && text.includes("Create project"));
    step(run, STEPS[5]);
    assert.equal(await page.evaluate(`document.querySelector('${OWN_BRANCH}')?.disabled ?? null`), false, "New project offers Own branch");
    await me.follow(OWN_BRANCH);
    assert.equal(await page.evaluate(`document.querySelector('${OWN_BRANCH}')?.checked ?? null`), true, "Own branch is chosen");
    step(run, STEPS[6]);
    await me.click("Create project", "button");
    const opened = await hashUntil(page, "the new project's overview", (now) => /^#\/projects\/[^/?]+\/overview/.test(now));
    id = decodeURIComponent(opened.split("/")[2]);
    await me.waitFor("the overview naming the project", (text) => text.includes(NAME));
    await me.capture("golden-add-project-overview");
    step(run, STEPS[7]);
  });
  const listed = (await value(run.bridge, "desktop_project_list")).items.map((item) => [item.id, item.name]);
  assert.deepEqual(listed, [[id, NAME]], "Home lists the new project");
  const setup = await value(run.bridge, "desktop_project_setup", { id });
  assert.deepEqual([setup.name, setup.about, setup.pool], [NAME, ABOUT, POOL], "the project's setup is recorded as filled in");
  assert.ok(setup.folder.startsWith(`${run.projects}/`), "the project's folder is made in the projects folder");
  await bridgeUntil("the pool to serve the project", "desktop_model_access_page", { collection: "pools", next: false }, (page) => JSON.stringify(served(page)) === JSON.stringify([id]));
  await bridgeUntil("Own branch to be saved", "desktop_project_agent_policy", { id }, (view) => view.access === "own_branch");
  assert.equal(await bridgeUntil("the job to be dismissed", "desktop_intake_status", {}, (job) => job === null), null);
  assert.ok(run.log.calls.includes("desktop_setup_project_job"), "Create project starts the engine's job");
  step(run, STEPS[8]);
  const record = finishGolden(run);
  assert.deepEqual([record?.steps.map((done) => done.step), record?.outcome], [STEPS, "passed"], "the run record names each step in order");
});
