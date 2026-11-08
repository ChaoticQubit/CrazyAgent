import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { RemoveDialogView, deleteReady, lossLines, removeChoices } = await import("../src/screens/project/remove-dialog.tsx");

const preview = (patch = {}) => ({
  name: "Gone app",
  folders: ["/Users/owner/CrazyAgent/gone-app"],
  can_delete: true,
  running: ["Builder 2"],
  open_change_sets: 1,
  losses: [{ id: 3, label: "Tester 3", code: "cleanup_worktree_dirty" }],
  ...patch,
});
const idle = () => {};
const view = (props) => renderToStaticMarkup(createElement(RemoveDialogView, { pending: false, onChoice: idle, onTyped: idle, typed: "", ...props }));

test("the remove dialog preselects Remove from dashboard and offers Delete everything only inside the projects home", () => {
  assert.deepEqual(removeChoices(preview()), ["dashboard", "everything"]);
  assert.deepEqual(removeChoices(preview({ can_delete: false })), ["dashboard"]);
  assert.deepEqual(removeChoices(null), ["dashboard"]);
  const html = view({ preview: preview(), choice: "dashboard" });
  assert.match(html, /Remove from dashboard \(keep files\)/);
  assert.match(html, /<input(?=[^>]*value="dashboard")(?=[^>]*checked="")[^>]*>/);
  assert.match(html, /Delete everything/);
  assert.match(html, /<input(?=[^>]*value="everything")[^>]*>/);
  assert.doesNotMatch(html, /<input(?=[^>]*value="everything")(?=[^>]*checked="")[^>]*>/);
  const outside = view({ preview: preview({ can_delete: false }), choice: "dashboard" });
  assert.match(outside, /Remove from dashboard \(keep files\)/);
  assert.doesNotMatch(outside, /Delete everything/, "a project outside the projects home has no Delete option");
});

test("Delete everything lists what is lost and stays disabled until the exact project name is typed", () => {
  assert.deepEqual(lossLines(preview()), [
    "Builder 2 is running and will be stopped",
    "1 change set not accepted or discarded",
    "Tester 3 has uncommitted edits",
  ]);
  assert.deepEqual(lossLines(preview({ running: [], open_change_sets: 0, losses: [{ id: 4, label: "Agent 4", code: "cleanup_worktree_unreadable" }] })), [
    "Agent 4 may have uncommitted edits",
  ]);
  const html = view({ preview: preview(), choice: "everything" });
  for (const line of lossLines(preview())) assert.ok(html.includes(line), line);
  assert.match(html, /\/Users\/owner\/CrazyAgent\/gone-app/);
  assert.match(html, /<input[^>]*aria-label="Type the project name"/);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Delete everything<\/button>/);
  assert.equal(deleteReady(preview(), ""), false);
  assert.equal(deleteReady(preview(), "Gone app "), false, "a trailing space is not the name");
  assert.equal(deleteReady(preview(), "gone app"), false, "case counts");
  assert.equal(deleteReady(preview(), "Gone app"), true);
  assert.equal(deleteReady(preview({ can_delete: false }), "Gone app"), false);
  const ready = view({ preview: preview(), choice: "everything", typed: "Gone app" });
  assert.doesNotMatch(ready, /<button[^>]*disabled=""[^>]*>Delete everything<\/button>/);
  assert.match(ready, /<button[^>]*>Delete everything<\/button>/);
});

test("when the engine refuses Delete everything the dialog says why and offers Remove only", () => {
  const why = "Service A uses a folder of this project, so deleting it would delete that project's files too. Remove it from the dashboard instead.";
  const html = view({ preview: preview({ can_delete: false, blocked: why }), choice: "everything" });
  assert.match(html, /Service A uses a folder of this project/);
  assert.doesNotMatch(html, /<input(?=[^>]*value="everything")[^>]*>/);
  assert.doesNotMatch(html, /aria-label="Type the project name"/);
  assert.match(html, /<button[^>]*>Remove project<\/button>/);
  assert.doesNotMatch(view({ preview: preview(), choice: "dashboard" }), /Service A uses a folder/);
});

test("a refused Delete everything reads as its own plain sentence, never the generic one", async () => {
  const { failureCopy } = await import("../src/api-projects.ts");
  for (const code of [
    "delete_shared_folder",
    "delete_research_running",
    "delete_platform_unqualified",
    "delete_outside_home",
    "delete_name_mismatch",
    "delete_agents_running",
    "delete_trash_partial",
    "delete_unavailable",
    "home_trash_failed",
    "home_move_in_progress",
    "project_delete_interrupted",
  ]) {
    assert.doesNotMatch(failureCopy(code), /Something went wrong/, code);
  }
});

test("at the app's smallest and default windows the Delete everything dialog fits its frame, and the top choice, the whole loss list and the Delete button stay in view while the name is typed (AL20)", async () => {
  const { bundle, withPage } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  const quiet = preview({ name: "Trash Me", running: [], open_change_sets: 0, losses: [],
    folders: ["/private/var/folders/_c/xkqxv9g95_10s31wrrbg0ts80000gn/T/ca-rm-4dVIN1/volume/projects/trash-me"] });
  for (const [width, height] of [[760, 560], [1180, 800]]) {
    for (const [label, shown] of [["no loss", quiet], ["three losses", preview()]]) {
      const fit = await withPage(async ({ evaluate, send }) => {
        await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
        await evaluate(fixture);
        await evaluate(`Fixture.mount(${JSON.stringify(shown)})`);
        await evaluate("Fixture.frames()");
        assert.equal(await evaluate("Fixture.focusName()"), true, "the name box is shown");
        await evaluate("Fixture.frames()");
        return evaluate("Fixture.fit()");
      });
      const seen = `${width}x${height} ${label}: ${JSON.stringify(fit)}`;
      assert.equal(fit.window, height, seen);
      assert.ok(fit.dialog.scroll <= fit.dialog.client && fit.dialog.top === 0, `the frame does not scroll: ${seen}`);
      assert.ok(fit.body.scroll <= fit.body.client && fit.body.top === 0, `the body fits the frame: ${seen}`);
      assert.ok(fit.cardTop >= 0 && fit.cardBottom <= fit.frame, `the top choice is in view: ${seen}`);
      if (label === "no loss") assert.ok(fit.details.scroll <= fit.details.client, `one folder and no loss need no scroll at all: ${seen}`);
      assert.equal(fit.remove, true, `the Delete everything button is in view: ${seen}`);
      if (label === "three losses") assert.equal(fit.losses, true, `the whole loss list is in view and no box clips it: ${seen}`);
    }
  }
});
