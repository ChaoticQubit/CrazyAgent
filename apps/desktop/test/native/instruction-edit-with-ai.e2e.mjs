import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, bridgeCall, value } from "./e2e-harness.mjs";
import { answering, onEngine } from "./codex-task-engine.mjs";

const RUN_MS = 120000;
const RULE = "- Write the failing test first.";
const APP = `# House rules\n\n${RULE}\n- Keep commits small.\n`;
const OWN = `# House rules\n\n${RULE}\n- Run make check before you finish.\n`;
const PROPOSAL = "# House rules\n\n- Run make check before you finish.\n";
const NEWER = `${OWN}- Ask before a force push.\n`;
const CODEX = answering(`Here is the cleaned file.\n\n~~~\n${PROPOSAL}~~~`);
const QUESTION = "Which pool runs this edit?";
const PROPOSED = "Proposed instructions";
const DOWN = "Can't reach this pool's model provider, so nothing was sent. Start it or pick another account, then try again.";
const POOL = "local-pool";

/** Saves instructions through the bridge the way the editor does. Inputs: the bridge, null
 * for the app text or the project id, and the text. Output: none. */
async function write(bridge, projectId, text) {
  const current = await value(bridge, "desktop_process_text", { projectId });
  const saved = await value(bridge, "desktop_save_process_text", { projectId, text, version: current.version, opened: null });
  assert.equal(saved.kind, "saved", `the ${projectId === null ? "app" : "project"} instructions save: ${JSON.stringify(saved)}`);
}

/** Reads the saved instructions text. Inputs: the bridge and null for the app text or the
 * project id. Output: the text. */
async function text(bridge, projectId) {
  return (await value(bridge, "desktop_process_text", { projectId })).text;
}

/** Opens a project's Process tab. Inputs: the page, the served app, the owner and the
 * project. Output: none. */
async function openProject(page, site, me, project) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/process` });
  await me.waitFor("the project's Delivery panel", (seen) => seen.includes("Lines the project file repeats"));
}

/** Opens Settings, Process, where the app instructions are edited. Inputs: the page, the
 * served app and the owner. Output: none. */
async function openApp(page, site, me) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/settings/process` });
  await me.waitFor("the app instructions editor", (seen) => seen.includes("Instructions every master and sub-agent gets, in every project."));
}

/** Says whether the editor on the page offers the Edit with AI button. Input: the page.
 * Output: true when a button with that label is drawn. */
function offered(page) {
  return page.evaluate(`[...document.querySelectorAll("button")].some((button) => (button.innerText ?? "").trim() === "Edit with AI")`);
}

/** Waits until the editor offers the Edit with AI button, or stops offering it. Inputs: the
 * page and which is expected. Output: none. */
async function offers(page, expected) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    if (await offered(page) === expected) return;
    assert.ok(Date.now() < deadline, `the Edit with AI button ${expected ? "never showed" : "stayed on the page"}`);
  }
}

/** Reads a switch as drawn. Inputs: the page and the switch's id. Output: true for On, false
 * for Off, null while it is not drawn. */
function switched(page, id) {
  return page.evaluate(`(() => { const one = document.getElementById(${JSON.stringify(id)}); return one ? one.getAttribute("aria-checked") === "true" : null; })()`);
}

/** Clicks a switch and waits until it is drawn in the wanted state with nothing saving.
 * Inputs: the owner, the page, the switch's id and the wanted state. Output: none. */
async function flip(me, page, id, wanted) {
  assert.equal(await switched(page, id), !wanted, `the ${id} switch starts ${wanted ? "Off" : "On"}`);
  await me.follow(`[id=${JSON.stringify(id)}]`);
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const now = await page.evaluate(`(() => { const one = document.getElementById(${JSON.stringify(id)}); return one ? [one.getAttribute("aria-checked") === "true", one.getAttribute("aria-disabled") === "true"] : null; })()`);
    if (now && now[0] === wanted && now[1] === false) return;
    assert.ok(Date.now() < deadline, `the ${id} switch never settled ${wanted ? "On" : "Off"}: ${JSON.stringify(now)}`);
  }
}

/** Reads the proposal's marked lines. Input: the page. Output: { removed, added } as the
 * lines' own text. */
function marked(page) {
  return page.evaluate(`(() => {
    const lines = (tag) => [...document.querySelectorAll(".pr-diff " + tag)].map((row) => [...row.childNodes].filter((node) => node.nodeType === 3).map((node) => node.textContent).join(""));
    return { removed: lines("del"), added: lines("ins") };
  })()`);
}

/** Reads the pool question as drawn. Input: the page. Output: the pools offered, the one
 * picked (null for none) and whether Propose can be clicked. */
function poolQuestion(page) {
  return page.evaluate(`(() => {
    const radios = [...document.querySelectorAll('input[type=radio][name="pr-ai-pool"]')];
    const propose = [...document.querySelectorAll("button")].find((button) => (button.innerText ?? "").trim() === "Propose");
    return { pools: radios.map((radio) => radio.value), picked: radios.find((radio) => radio.checked)?.value ?? null, propose: propose ? propose.disabled !== true : null };
  })()`);
}

/** Asks the engine for a proposal the way the page does and requires a refusal. Inputs: the
 * bridge, null for the app text or the project id, and the pool. Output: the refusal's code. */
async function refused(bridge, projectId, pool) {
  const answer = await bridgeCall(bridge, "desktop_process_propose", { projectId, pool });
  assert.ok("failure" in answer, `the engine refuses the proposal: ${JSON.stringify(answer)}`);
  return answer.failure.code;
}

test("Edit with AI: On, a project's editor proposes on the project's pool with no question; Discard leaves the file and Accept saves it", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-on-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    assert.equal((await value(bridge, "desktop_instruction_settings", { projectId: project })).settings.edit_with_ai, true, "a project the owner never changed has Edit with AI on");
    await openProject(page, site, me, project);
    await offers(page, true);

    await me.click("Edit with AI", "button");
    const shown = await me.waitFor("the proposal", (seen) => seen.includes(PROPOSED), RUN_MS);
    await me.capture("project-proposal");
    assert.ok(!shown.includes(QUESTION), "a project's editor never asks for a pool");
    assert.ok(shown.includes("1 line removed, 0 added. Nothing changes until you accept."), `the change count: ${shown.slice(0, 2000)}`);
    assert.deepEqual(await marked(page), { removed: [RULE], added: [] }, "the line the app instructions already hold is the one marked as removed");
    assert.equal(await text(bridge, project), OWN, "a proposal changes nothing by itself");

    await me.click("Discard", "button");
    await me.waitFor("the editor without the proposal", (seen) => !seen.includes(PROPOSED));
    assert.equal(await text(bridge, project), OWN, "Discard leaves the file as it was");
    assert.equal(await text(bridge, null), APP, "the app instructions are never touched by a project's edit");

    await me.click("Edit with AI", "button");
    await me.waitFor("the second proposal", (seen) => seen.includes(PROPOSED), RUN_MS);
    await me.click("Accept", "button");
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
      if (await text(bridge, project) === PROPOSAL) break;
      assert.ok(Date.now() < deadline, "Accept never saved the proposed text");
    }
    await me.waitFor("the editor after Accept", (seen) => !seen.includes(PROPOSED));
    await me.capture("project-accepted");
    assert.equal(await text(bridge, null), APP, "Accept saves the project's file only");
  }, 1, CODEX);
});

test("Edit with AI: a pool whose model provider refuses connections says nothing was sent and to start it, and the file stays", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-down-", async ({ project, me, page, site, bridge, provider }) => {
    await write(bridge, project, OWN);
    const closed = new Promise((done) => provider.server.close(done));
    provider.server.closeAllConnections();
    await closed;
    assert.equal(await refused(bridge, project, null), "instructions_cleanup_provider_unreachable", "the engine names the stopped provider before any run");
    await openProject(page, site, me, project);
    await offers(page, true);
    await me.click("Edit with AI", "button");
    const shown = await me.waitFor("the stopped provider line", (seen) => seen.includes(DOWN), STEP_MS);
    await me.capture("project-provider-down");
    assert.ok(!shown.includes(PROPOSED), "no proposal shows");
    assert.equal(await text(bridge, project), OWN, "the file stays as it was");
  }, 1, CODEX);
});

test("Edit with AI: Off on a project hides its button and the engine refuses; the app editor keeps its own", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-project-off-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    await openProject(page, site, me, project);
    await offers(page, true);

    await flip(me, page, "agent-edit-with-ai", false);
    await offers(page, false);
    await me.capture("project-off");
    assert.equal((await value(bridge, "desktop_instruction_settings", { projectId: project })).settings.edit_with_ai, false, "the engine stores the project's Off");
    assert.equal(await refused(bridge, project, null), "instructions_cleanup_off", "Off stops the run in the engine, not only on the page");
    assert.equal((await value(bridge, "desktop_app_instruction_settings")).edit_with_ai, true, "the project's Off leaves the app-wide choice alone");

    await openApp(page, site, me);
    await offers(page, true);
    await openProject(page, site, me, project);
    await offers(page, false);

    await flip(me, page, "agent-edit-with-ai", true);
    await offers(page, true);
    assert.equal(await text(bridge, project), OWN, "changing the setting never changes the file");
  }, 1, CODEX);
});

test("Edit with AI: Off for the app instructions hides the app editor's button and the engine refuses; a project keeps its own", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-app-off-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    assert.equal((await value(bridge, "desktop_app_instruction_settings")).edit_with_ai, true, "Edit with AI for the app instructions starts on");
    await openApp(page, site, me);
    await offers(page, true);

    await flip(me, page, "process-edit-with-ai", false);
    await offers(page, false);
    await me.capture("app-off");
    assert.equal((await value(bridge, "desktop_app_instruction_settings")).edit_with_ai, false, "the engine stores the app-wide Off");
    assert.equal(await refused(bridge, null, POOL), "instructions_cleanup_off", "Off stops the run in the engine, not only on the page");
    assert.equal((await value(bridge, "desktop_instruction_settings", { projectId: project })).settings.edit_with_ai, true, "the app-wide Off leaves the project's choice alone");

    await openProject(page, site, me, project);
    await offers(page, true);
    await openApp(page, site, me);
    await offers(page, false);

    await flip(me, page, "process-edit-with-ai", true);
    await offers(page, true);
    assert.equal(await text(bridge, null), APP, "changing the setting never changes the file");
  }, 1, CODEX);
});

test("Edit with AI: accepting a proposal after another window saved shows the conflict view and keeps the newer text", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-stale-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    await openProject(page, site, me, project);
    await offers(page, true);
    await me.click("Edit with AI", "button");
    await me.waitFor("the proposal", (seen) => seen.includes(PROPOSED), RUN_MS);

    await write(bridge, project, NEWER);
    await me.click("Accept", "button");
    const conflict = await me.waitFor("the conflict view", (seen) => seen.includes("Keep mine") && seen.includes("Saved now:"));
    await me.capture("stale-conflict");
    assert.ok(conflict.includes("- Ask before a force push."), "the conflict view shows the text the other window saved");
    assert.equal(await text(bridge, project), NEWER, "a stale Accept overwrites nothing");

    await me.click("Reload", "button");
    await me.waitFor("the editor without the conflict", (seen) => !seen.includes("Keep mine"));
    assert.equal(await text(bridge, project), NEWER, "Reload keeps the newer text");
  }, 1, CODEX);
});

test("Edit with AI: the app editor asks which pool runs the edit on every click and stores no pick", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-edit-ai-app-pool-", async ({ me, page, site, bridge }) => {
    await write(bridge, null, APP);
    assert.equal(await refused(bridge, null, null), "instructions_cleanup_pool_needed", "the engine runs no app edit without a picked pool");
    await openApp(page, site, me);
    await offers(page, true);

    await me.click("Edit with AI", "button");
    await me.waitFor("the pool question", (seen) => seen.includes(QUESTION));
    await me.capture("app-pool-question");
    assert.deepEqual(await poolQuestion(page), { pools: [POOL], picked: null, propose: false }, "the first click asks, with no pool picked and nothing to propose with");
    await me.follow(`input[type=radio][name="pr-ai-pool"][value=${JSON.stringify(POOL)}]`);
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
      const asked = await poolQuestion(page);
      if (asked.picked === POOL && asked.propose === true) break;
      assert.ok(Date.now() < deadline, `the picked pool never enabled Propose: ${JSON.stringify(asked)}`);
    }
    await me.click("Propose", "button");
    const shown = await me.waitFor("the app proposal", (seen) => seen.includes(PROPOSED), RUN_MS);
    assert.ok(!shown.includes(QUESTION), "the question leaves once the pool is picked");
    await me.click("Discard", "button");
    await me.waitFor("the editor without the proposal", (seen) => !seen.includes(PROPOSED));
    assert.equal(await text(bridge, null), APP, "Discard leaves the app instructions as they were");

    await me.click("Edit with AI", "button");
    await me.waitFor("the pool question again", (seen) => seen.includes(QUESTION));
    assert.deepEqual(await poolQuestion(page), { pools: [POOL], picked: null, propose: false }, "the second click asks again; the first pick was not kept");
    await me.click("Cancel", "button");
    await me.waitFor("the editor without the question", (seen) => !seen.includes(QUESTION));
    assert.equal(await refused(bridge, null, null), "instructions_cleanup_pool_needed", "the engine kept no pool either");
    assert.equal(await text(bridge, null), APP, "Cancel leaves the app instructions as they were");
  }, 1, CODEX);
});
