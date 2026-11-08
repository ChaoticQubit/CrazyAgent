import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overview-activity.mjs", import.meta.url).pathname);

const LOGIN = "a".repeat(32);
const item = (id, kind, text, change_set, undone = false) => ({ id, at_ms: Date.UTC(2026, 10, 7, 8, id), kind, actor: kind === "merged_into_main" ? "Master" : "Owner", goal: "Build the service", text, change_set, undone });
const ITEMS = [
  item(4, "lead_merged", "Master merged change set #4", null),
  item(3, "accepted", "Accepted the changes of Ada: Add search", "c".repeat(32)),
  item(2, "merged_into_main", "The master merged its changes into main: Fix the logout", "b".repeat(32), true),
  item(1, "merged_into_main", "The master merged its changes into main: Fix the login", LOGIN),
];
const read = (items) => ({ kind: "ok", value: { project_id: "alpha", items, more: false } });
const LONG = 60_000;

/** Mounts Recent activity with these events, the ones above unless given, and no poll of
 * its own, then runs steps. Inputs: the steps, as source that returns an object, and the
 * events. Output: what the steps returned. */
function steps(source, items = ITEMS) {
  return withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { await Fixture.mount("alpha", ${LONG}); const first = await Fixture.reply(${JSON.stringify(read(items))}); ${source} })()`);
  });
}

test("a master's merge into main has Undo merge, and an undone merge, an Accept or a lead's merge has none", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const timeline = await Fixture.click("[role=radio]", "Timeline");
    return { first, timeline };
  `);
  const login = [{ text: "Undo merge", row: "The master merged its changes into main: Fix the login", disabled: false }];
  assert.deepEqual(seen.first.buttons.filter((button) => button.text === "Undo merge"), login);
  assert.equal(seen.timeline.missed, null);
  assert.deepEqual(seen.timeline.buttons.filter((button) => button.text === "Undo merge"), login, "the Timeline offers the same Undo");
});

test("Undo asks first, naming the branch, and Keep it changes nothing", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const clicked = await Fixture.click(".pj-activity-row button", "Undo merge");
    if (clicked.missed) return { first, clicked };
    const kept = await Fixture.click("dialog[open] button", "Keep it");
    return { first, clicked, kept };
  `);
  assert.equal(seen.clicked.missed, null, "the merge row has an Undo merge button");
  assert.match(seen.clicked.dialog ?? "", /Undo this merge on main\?/, "the confirm names the branch");
  assert.match(seen.clicked.dialog ?? "", /one new commit on main that reverts this merge\. Later commits stay\./);
  assert.deepEqual(seen.clicked.undos, [], "nothing is asked of the engine before the owner confirms");
  assert.equal(seen.kept.missed, null, "the confirm has Keep it");
  assert.equal(seen.kept.dialog, null, "Keep it closes the confirm");
  assert.deepEqual(seen.kept.undos, [], "Keep it undoes nothing");
});

test("Undo names the branch of a merge row that names its change set by number (AL2)", { timeout: 60_000 }, async () => {
  const numbered = [item(1, "merged_into_main", "Master merged change set #7 (Master: Merge the docs into the guide, 2 files) into release.1. It closed the changes of Ada (@docs) · Codex", LOGIN)];
  const seen = await steps(`
    const clicked = await Fixture.click(".pj-activity-row button", "Undo merge");
    return { first, clicked };
  `, numbered);
  assert.equal(seen.clicked.missed, null, "the merge row has an Undo merge button");
  assert.match(seen.clicked.dialog ?? "", /Undo this merge on release\.1\?/, "the confirm names the branch the row says the set merged into");
  assert.match(seen.clicked.dialog ?? "", /one new commit on release\.1 that reverts this merge\./);
});

test("Undo reverts the merge through the engine, says so and reads the activity again", { timeout: 60_000 }, async () => {
  const undone = [item(5, "undone", "Undid the changes of Master on main: Fix the login", LOGIN), ...ITEMS.slice(0, 3), { ...ITEMS[3], undone: true }];
  const seen = await steps(`
    const asked = await Fixture.click(".pj-activity-row button", "Undo merge");
    if (asked.missed) return { first, clicked: asked };
    const clicked = await Fixture.click("dialog[open] button", "Undo merge");
    if (clicked.missed) return { first, clicked };
    const done = await Fixture.answerUndo({ kind: "ok", value: { kind: "landed", commit: "${"f".repeat(40)}", branch: "main" } });
    const again = await Fixture.reply(${JSON.stringify(read(undone))});
    return { first, clicked, done, again };
  `);
  assert.equal(seen.clicked.missed, null, "the merge row has an Undo merge button and the confirm has Undo merge");
  assert.deepEqual(seen.clicked.undos, [LOGIN], "Undo merge asks the engine to undo that merge's change set");
  assert.deepEqual(seen.clicked.buttons.filter((button) => button.text === "Undo merge").map((button) => button.disabled), [true], "Undo merge waits for the engine");
  assert.equal(seen.done.note, "Undid the merge with one new commit on main. Later commits stay.");
  assert.equal(seen.done.dialog, null, "the confirm closes once the engine answers");
  assert.equal(seen.done.waiting, 1, "the activity is read again at once");
  assert.deepEqual(seen.again.buttons.filter((button) => button.text === "Undo merge"), [], "an undone merge has no Undo merge");
  assert.match(seen.again.text, /Fix the login Undone/);
});

test("an Undo that conflicts names the files and changes nothing, and a refusal reads as a sentence", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.click(".pj-activity-row button", "Undo merge");
    const clicked = await Fixture.click("dialog[open] button", "Undo merge");
    if (clicked.missed) return { first, clicked };
    const conflict = await Fixture.answerUndo({ kind: "ok", value: { kind: "conflict", files: ["README.md", "src/login.rs"] } });
    await Fixture.click(".pj-activity-row button", "Undo merge");
    await Fixture.click("dialog[open] button", "Undo merge");
    const refused = await Fixture.answerUndo({ kind: "error", code: "changes_undo_unavailable", message: "x" });
    return { first, clicked, conflict, refused };
  `);
  assert.equal(seen.clicked.missed, null, "the merge row has an Undo merge button and the confirm has Undo merge");
  assert.equal(seen.conflict.note, "Couldn't undo the merge: README.md, src/login.rs changed on its branch since, so nothing changed.");
  assert.equal(seen.conflict.waiting, 0, "nothing changed, so nothing is read again");
  assert.deepEqual(seen.conflict.buttons.filter((button) => button.text === "Undo merge").map((button) => button.disabled), [false], "Undo merge can be tried again");
  assert.equal(seen.refused.note, "This merge can't be undone. CrazyAgent undoes only a merge it committed itself, and only once.");
});

const APP_CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
].join("\n");

test("in the real Overview column the merge row's Undo merge is a small button at the row's right, on the line of its words", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 768, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    return evaluate(`(async () => {
      await Fixture.mount("alpha", ${LONG});
      Fixture.inOverview(${JSON.stringify(APP_CSS)});
      await Fixture.reply(${JSON.stringify(read(ITEMS))});
      const button = document.querySelector(".pj-activity-row button");
      const row = button?.closest(".pj-activity-row");
      const words = row?.querySelector(".pj-activity-text");
      if (!button || !row || !words) return null;
      const [b, r, w] = [button, row, words].map((part) => part.getBoundingClientRect());
      return { text: button.textContent, small: button.classList.contains("btn") && button.classList.contains("small"),
        right: Math.round(r.right - b.right), beside: b.left >= w.right - 0.5, line: Math.round(b.top - w.top) };
    })()`);
  });
  assert.ok(seen, "a merge row with a button and its words");
  assert.equal(seen.text, "Undo merge");
  assert.equal(seen.small, true, "a small button, not a text link");
  assert.ok(seen.beside, "the button sits right of the row's words, not under the time");
  assert.ok(seen.right <= 16, `the button sits at the row's right edge, ${seen.right} px in`);
  assert.ok(Math.abs(seen.line) <= 6, `the button starts on the line of the row's words, ${seen.line} px apart`);
});
