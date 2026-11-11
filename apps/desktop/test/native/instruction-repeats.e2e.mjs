import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { onEngine } from "./codex-task-engine.mjs";

const RULE = "- Write the failing test first.";
const APP = `# House rules\n\n${RULE}\n- Keep commits small.\n`;
const OWN = `# House rules\n\n${RULE}\n- Run make check before you finish.\n`;
const ROW = "Lines the project file repeats";

/** Counts how often a line stands in a text as a whole line. Inputs: the text and the line.
 * Output: the count. */
function times(text, line) {
  return text.split("\n").filter((each) => each.trim() === line).length;
}

/** Saves instructions through the bridge the way the editor does. Inputs: the bridge, null
 * for the app text or the project id, and the text. Output: none. */
async function write(bridge, projectId, text) {
  const current = await value(bridge, "desktop_process_text", { projectId });
  const saved = await value(bridge, "desktop_save_process_text", { projectId, text, version: current.version, opened: null });
  assert.equal(saved.kind, "saved", `the ${projectId === null ? "app" : "project"} instructions save: ${JSON.stringify(saved)}`);
}

/** Reads the repeated lines row of the Delivery panel: the picked choice and the note beside
 * it. Input: the page. Output: { picked, note }, null while the row is not drawn. */
function repeatedRow(page) {
  return page.evaluate(`(() => {
    const group = document.querySelector('[role=radiogroup][aria-label=${JSON.stringify(ROW)}]');
    if (!group) return null;
    return {
      picked: (group.querySelector('[aria-checked="true"]')?.textContent ?? "").trim(),
      note: (document.getElementById("agent-repeated-note")?.textContent ?? "").trim(),
    };
  })()`);
}

/** Waits until the repeated lines row shows this choice as picked and the engine stores it.
 * Inputs: the page, the bridge, the project and the stored value with its label. Output: the
 * row as shown. */
async function settled(page, bridge, project, stored, label) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const row = await repeatedRow(page);
    const view = await value(bridge, "desktop_instruction_settings", { projectId: project });
    if (row?.picked === label && view.settings.repeated === stored) return row;
    assert.ok(Date.now() < deadline, `the row never settled on ${label}: ${JSON.stringify({ row, settings: view.settings })}`);
  }
}

test("Lines the project file repeats: Leave out gives agents the repeated line once", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-repeats-leave-out-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    const view = await value(bridge, "desktop_instruction_settings", { projectId: project });
    assert.equal(view.settings.repeated, "leave_out", "a project the owner never changed leaves repeated lines out");
    assert.equal(view.repeats, 1, "one project line repeats the app text; the shared heading is not counted");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/process` });
    await me.waitFor("the Delivery panel's repeated lines row", (text) => text.includes(ROW));
    const row = await settled(page, bridge, project, "leave_out", "Leave out");
    await me.capture("repeats-leave-out");
    assert.equal(row.note, "1 line of this project's file repeats the app instructions and is left out for agents. Only a line under the same headings counts. Code blocks, tables and rules always stay.");

    const given = await value(bridge, "desktop_process_given", { projectId: project });
    assert.equal(times(given, RULE), 1, `agents get the repeated line once: ${given}`);
    assert.equal(times(given, "# House rules"), 2, "a repeated heading stays in both parts");
    assert.equal(times(given, "- Run make check before you finish."), 1, "the project's own line stays");
    assert.equal(times(given, "- Keep commits small."), 1, "the app's own line stays");
    assert.equal((await value(bridge, "desktop_process_text", { projectId: project })).text, OWN, "the project's file itself is not changed");
  });
});

test("Lines the project file repeats: Keep gives agents the repeated line twice", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-repeats-keep-", async ({ project, me, page, site, bridge }) => {
    await write(bridge, null, APP);
    await write(bridge, project, OWN);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/process` });
    await me.waitFor("the Delivery panel's repeated lines row", (text) => text.includes(ROW));
    await settled(page, bridge, project, "leave_out", "Leave out");

    await me.click("Keep", `[role=radiogroup][aria-label=${JSON.stringify(ROW)}] [role=radio]`);
    const row = await settled(page, bridge, project, "keep", "Keep");
    await me.capture("repeats-keep");
    assert.equal(row.note, "1 line of this project's file repeats the app instructions, and agents get it twice.");
    const given = await value(bridge, "desktop_process_given", { projectId: project });
    assert.equal(times(given, RULE), 2, `agents get the repeated line from both files: ${given}`);
    assert.equal(times(given, "- Run make check before you finish."), 1);

    await me.click("Leave out", `[role=radiogroup][aria-label=${JSON.stringify(ROW)}] [role=radio]`);
    await settled(page, bridge, project, "leave_out", "Leave out");
    assert.equal(times(await value(bridge, "desktop_process_given", { projectId: project }), RULE), 1, "back on Leave out the line is given once again");
  });
});

test("Lines the project file repeats: Leave out keeps a rule that stands under another heading", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-repeats-sections-", async ({ project, bridge }) => {
    await write(bridge, null, "## Always\n\n- Use npm.\n");
    await write(bridge, project, "## Never\n\n- Use npm.\n\n## Always\n\n- Use npm.\n- Run make check before you finish.\n");
    const view = await value(bridge, "desktop_instruction_settings", { projectId: project });
    assert.equal(view.settings.repeated, "leave_out");
    assert.equal(view.repeats, 1, "only the line under the same heading counts as repeated");
    const given = await value(bridge, "desktop_process_given", { projectId: project });
    assert.equal(times(given, "- Use npm."), 2, `the rule under Never stays beside the app's rule under Always: ${given}`);
    const never = given.indexOf("## Never");
    assert.ok(never > 0 && given.indexOf("- Use npm.", never) < given.indexOf("## Always", never), "the Never section still holds its rule");
    assert.equal(times(given, "- Run make check before you finish."), 1);
  });
});
