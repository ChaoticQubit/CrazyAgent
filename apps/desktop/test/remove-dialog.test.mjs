import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const dialog = await import("../src/screens/project/remove-dialog.tsx");
const { RemoveDialogView, deleteReady, lossLines, removeChoices } = dialog;
const NAME_BYTES_MAX = 255;

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

const losses = (count, code, from = 100) => Array.from({ length: count }, (_, index) => ({ id: from + index, label: `Agent ${from + index}`, code }));

test("past four loss lines the dialog groups them into counts and keeps every kind of loss", () => {
  const four = preview({ losses: losses(2, "cleanup_worktree_dirty") });
  assert.deepEqual(lossLines(four), ["Builder 2 is running and will be stopped", "1 change set not accepted or discarded", "Agent 100 has uncommitted edits", "Agent 101 has uncommitted edits"], "four lines stay as they are, with each agent's name");
  const crowded = preview({
    running: ["Builder 2", "Builder 3", "Builder 4"], open_change_sets: 2,
    losses: [...losses(2, "cleanup_worktree_dirty"), ...losses(1, "cleanup_worktree_unreadable", 200), ...losses(3, "cleanup_commits_unlanded", 300), ...losses(2, "cleanup_changes_open", 400)],
  });
  assert.deepEqual(lossLines(crowded), [
    "3 agents are running and will be stopped",
    "2 change sets not accepted or discarded",
    "3 agents have or may have uncommitted edits",
    "5 agents have work that never landed",
  ]);
  const one = preview({ running: ["Builder 2"], open_change_sets: 0, losses: [...losses(1, "cleanup_worktree_unreadable"), ...losses(3, "cleanup_refs_unlanded", 200)] });
  assert.deepEqual(lossLines(one), ["1 agent is running and will be stopped", "1 agent may have uncommitted edits", "3 agents have work that never landed"]);
  const twice = preview({ running: [], open_change_sets: 0, losses: [{ id: 1, label: "Ash", code: "cleanup_worktree_dirty" }, { id: 1, label: "Ash", code: "cleanup_commits_unlanded" }, { id: 1, label: "Ash", code: "cleanup_changes_open" }, ...losses(2, "cleanup_worktree_dirty")] });
  assert.deepEqual(lossLines(twice), ["3 agents have uncommitted edits", "1 agent has work that never landed"], "an agent with several losses counts once per kind");
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
    "delete_in_progress",
    "delete_not_recorded",
  ]) {
    assert.doesNotMatch(failureCopy(code), /Something went wrong/, code);
  }
  assert.match(failureCopy("delete_not_recorded"), /Nothing was deleted\..*restart CrazyAgent/);
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

test("at the smallest window a project with many agents, losses and folders keeps the frame still: only the folder list scrolls, and the keyboard can scroll it", async () => {
  const { bundle, withPage } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  const many = preview({
    name: "Trash Me", running: Array.from({ length: 14 }, (_, index) => `Builder ${index}`), open_change_sets: 3,
    losses: [...losses(30, "cleanup_worktree_dirty"), ...losses(10, "cleanup_commits_unlanded", 200)],
    folders: Array.from({ length: 40 }, (_, index) => `/Users/owner/CrazyAgent/trash-me/part-${index}`),
  });
  const fit = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 760, height: 560, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(many)})`);
    await evaluate("Fixture.frames()");
    assert.equal(await evaluate("Fixture.focusName()"), true, "the name box is shown");
    await evaluate("Fixture.frames()");
    return evaluate("Fixture.fit()");
  });
  const seen = JSON.stringify(fit);
  assert.ok(fit.dialog.scroll <= fit.dialog.client && fit.dialog.top === 0, `the frame does not scroll: ${seen}`);
  assert.ok(fit.body.scroll <= fit.body.client && fit.body.top === 0, `the body fits the frame: ${seen}`);
  assert.ok(fit.cardTop >= 0 && fit.cardBottom <= fit.frame, `the top choice is in view: ${seen}`);
  assert.equal(fit.losses, true, `the whole loss list is in view: ${seen}`);
  assert.equal(fit.name, true, `the name box is in view: ${seen}`);
  assert.equal(fit.remove, true, `the Delete everything button is in view: ${seen}`);
  assert.ok(fit.details.scroll > fit.details.client, `the folder list is what scrolls: ${seen}`);
  assert.equal(fit.detailsTab, 0, `the keyboard reaches the folder list: ${seen}`);
});

const paths = (count) => Array.from({ length: count }, (_, index) => `/Users/owner/CrazyAgent/trash-me/part-${index}`);
const fourLines = (patch = {}) => preview({
  name: "Trash Me", running: ["Builder 2"], open_change_sets: 1, folders: paths(2),
  losses: [{ id: 3, label: "Tester 3", code: "cleanup_worktree_dirty" }, { id: 4, label: "Tester 4", code: "cleanup_commits_unlanded" }], ...patch,
});
const SMALLEST = [[760, 560], [760, 528]];

async function measured(fixture, shown, [width, height]) {
  const { withPage } = await import("./browser.mjs");
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(shown)})`);
    await evaluate("Fixture.frames()");
    assert.equal(await evaluate("Fixture.focusName()"), true, "the name box is shown");
    await evaluate("Fixture.frames()");
    return { fit: await evaluate("Fixture.fit()"), folders: await evaluate("Fixture.folders()"), lines: await evaluate("Fixture.lossFit()") };
  });
}

test("Delete everything says how many folders go to the Trash, and at the smallest windows at least two of them show whole above four loss lines", async () => {
  const { bundle } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  assert.match(view({ preview: preview(), choice: "everything" }), /Moves to the Trash \(1 folder\)/);
  for (const size of SMALLEST) {
    for (const count of [2, 40]) {
      const { fit, folders } = await measured(fixture, fourLines({ folders: paths(count) }), size);
      const seen = `${size.join("x")} ${count} folders: ${JSON.stringify({ fit, folders })}`;
      assert.equal(folders.heading, `Moves to the Trash (${count} folders)`, seen);
      assert.equal(folders.headingSeen, true, `the count is in view: ${seen}`);
      assert.ok(folders.whole >= 2, `two folders show whole: ${seen}`);
      assert.ok(fit.dialog.scroll <= fit.dialog.client, `the frame does not scroll: ${seen}`);
      assert.equal(fit.remove, true, `the Delete everything button is in view: ${seen}`);
    }
  }
});

test("long names keep every loss line and folder row on one line, keep the words saying what is lost in view and leave the dialog its fit", async () => {
  const { bundle } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  const unbroken = "x".repeat(128);
  const spaced = "word ".repeat(26).trim();
  const longPath = `/Users/owner/CrazyAgent/${"p".repeat(200)}`;
  for (const size of SMALLEST) {
    const short = await measured(fixture, fourLines(), size);
    for (const [label, name] of [[unbroken, "Trash Me"], [spaced, "Trash Me"], ["Builder 2", "n".repeat(64)]]) {
      const long = fourLines({ name, running: [label], folders: [longPath, ...paths(1)],
        losses: [{ id: 3, label, code: "cleanup_worktree_dirty" }, { id: 4, label, code: "cleanup_commits_unlanded" }] });
      const { fit, folders, lines } = await measured(fixture, long, size);
      const seen = `${size.join("x")} ${label.slice(0, 8)} ${name.slice(0, 8)}: ${JSON.stringify({ fit, folders, lines })}`;
      for (const [index, line] of lines.entries()) {
        assert.equal(line.height, short.lines[index].height, `a loss line stays one line: ${seen}`);
        assert.equal(line.words, true, `what is lost stays in view: ${seen}`);
        assert.equal(line.title, line.text, `the hover text has the whole line: ${seen}`);
      }
      assert.ok(lines[0].text.startsWith(`${label} is running`), `the line keeps the whole name for a screen reader: ${seen}`);
      assert.equal(folders.tallest, short.folders.tallest, `a folder row stays one line: ${seen}`);
      assert.ok(folders.belowHeading && folders.aboveLosses, `the list sits clear of its neighbours: ${seen}`);
      assert.ok(folders.whole >= 1, `a folder shows whole: ${seen}`);
      assert.ok(fit.dialog.scroll <= fit.dialog.client, `the frame does not scroll: ${seen}`);
      assert.equal(fit.name, true, `the name box is in view: ${seen}`);
      assert.equal(fit.remove, true, `the Delete everything button is in view: ${seen}`);
    }
  }
});

test("the keyboard goes from the name box to the folder list and scrolls it with real key presses", async () => {
  const { bundle, withPage } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  const seen = await withPage(async ({ evaluate, send }) => {
    const press = async (key, code, modifiers = 0) => {
      for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key, code: key, windowsVirtualKeyCode: code, modifiers });
    };
    await send("Emulation.setDeviceMetricsOverride", { width: 760, height: 560, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(fourLines({ folders: paths(40) }))})`);
    await evaluate("Fixture.frames()");
    assert.equal(await evaluate("Fixture.focusName()"), true, "the name box is shown");
    await press("Tab", 9, 8);
    const reached = await evaluate("Fixture.listState()");
    await press("ArrowDown", 40);
    await press("ArrowDown", 40);
    await press("PageDown", 34);
    await evaluate("new Promise((done) => setTimeout(done, 400))");
    return { reached, scrolled: await evaluate("Fixture.listState()") };
  });
  assert.deepEqual(seen.reached, { focused: true, top: 0 }, "Shift+Tab from the name box lands on the list");
  assert.equal(seen.scrolled.focused, true, "the list keeps the focus");
  assert.ok(seen.scrolled.top > 0, `the keys scrolled the list: ${JSON.stringify(seen)}`);
});

test("a project name up to the 255-byte bound is cut to one line with an ellipsis in the dialog title, and the Type to confirm label shows the whole name to type, wrapping within a bounded height (UI2-R2 markup, r1 L1)", async () => {
  const { readFileSync } = await import("node:fs");
  assert.equal(typeof dialog.removeTitle, "function", "the remove dialog draws its own title");
  for (const name of ["n".repeat(NAME_BYTES_MAX), "word ".repeat(51).trim()]) {
    const clip = new RegExp(`<span class="pj-remove-clip" title="${name}">${name}</span>`, "u");
    const title = renderToStaticMarkup(createElement("h2", null, dialog.removeTitle(name)));
    assert.match(title, clip, "the title cuts the name and keeps it whole in the hover");
    assert.match(title, /from CrazyAgent\?<\/h2>$/u, "the end of the question stays in view");
    const label = view({ preview: preview({ name }), choice: "everything" }).match(/<span class="pj-label pj-remove-name">.*? to confirm<\/span>/u)?.[0] ?? "";
    assert.equal(label, `<span class="pj-label pj-remove-name">Type <span class="pj-mono">${name}</span> to confirm</span>`, "the label shows the whole name the owner must type");
  }
  const css = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
  assert.match(css, /\.pj-remove-clip \{[^}]*display: inline-block;[^}]*max-width: [^;]+;[^}]*overflow: hidden;[^}]*text-overflow: ellipsis;[^}]*white-space: nowrap;/u);
  assert.match(css, /\.pj-remove-name \{[^}]*overflow-wrap: anywhere;[^}]*max-height: \d+px;[^}]*overflow-y: auto;/u, "the whole name wraps and its height stays bounded, so Delete stays in the frame");
  const shell = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
  assert.match(shell, /\.dialog-head h2 \{[^}]*min-width: 0;/u, "a long title cannot push the close button out");
  const settings = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(settings, /<Dialog open=\{confirming\} title=\{removeTitle\(displayText\(data\.name\)\)\}/u, "the project settings dialog uses that title");
});

test("at the smallest windows a project name at the 255-byte bound, spaced or unbroken, leaves the dialog its fit with the name box and Delete in view (UI2-R2)", async () => {
  const { bundle } = await import("./browser.mjs");
  const fixture = await bundle(new URL("./fixtures/remove-dialog-fit.mjs", import.meta.url).pathname);
  for (const size of SMALLEST) {
    for (const name of ["n".repeat(NAME_BYTES_MAX), "word ".repeat(51).trim()]) {
      const { fit } = await measured(fixture, fourLines({ name }), size);
      const seen = `${size.join("x")} ${name.slice(0, 8)}: ${JSON.stringify(fit)}`;
      assert.ok(fit.dialog.scroll <= fit.dialog.client && fit.dialog.top === 0, `the frame does not scroll: ${seen}`);
      assert.ok(fit.wide <= fit.frameWidth, `nothing pushes the frame sideways: ${seen}`);
      assert.equal(fit.titleLines, 1, `the title stays one line: ${seen}`);
      assert.equal(fit.name, true, `the name box is in view: ${seen}`);
      assert.equal(fit.remove, true, `the Delete everything button is in view: ${seen}`);
    }
  }
});
