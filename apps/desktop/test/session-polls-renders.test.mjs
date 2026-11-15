import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { keptRead } = await import("../src/state.ts");
const sessionPage = await bundle(new URL("./fixtures/session-polls-renders.mjs", import.meta.url).pathname);

const ROUNDS = 6;
const FOLDERS = [["notes", "found"], ["site", "onboarding"]];
const reads = (asked, command) => asked.filter((item) => item === command).length;

test("a read equal to the one shown keeps the shown value, and any change gives the new one", () => {
  const shown = { kind: "ok", value: [{ name: "notes", state: "found" }] };
  assert.equal(keptRead(shown, { kind: "ok", value: [{ name: "notes", state: "found" }] }), shown);
  const changed = { kind: "ok", value: [{ name: "notes", state: "onboarding" }] };
  assert.equal(keptRead(shown, changed), changed);
  const failed = { kind: "error", code: "x", message: "m" };
  assert.equal(keptRead(shown, failed), failed);
  assert.equal(keptRead(null, null), null);
  const job = { id: "j", state: { state: "copying", copied: 1, checked: 2 } };
  assert.equal(keptRead(job, { id: "j", state: { state: "copying", copied: 1, checked: 2 } }), job);
  const moved = { id: "j", state: { state: "copying", copied: 2, checked: 2 } };
  assert.equal(keptRead(job, moved), moved);
});

test("the root session's found-folder and intake polls render the app's tree only when a read changed, at one read each a round", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(sessionPage);
    const started = await evaluate(`Fixture.start(${JSON.stringify(FOLDERS)}, "job-1")`);
    const same = await evaluate(`Fixture.looks(${JSON.stringify(FOLDERS)}, "job-1", ${ROUNDS})`);
    const added = await evaluate(`Fixture.looks(${JSON.stringify([...FOLDERS, ["api", "found"]])}, "job-1", 1)`);
    const settled = await evaluate(`Fixture.looks(${JSON.stringify([...FOLDERS, ["api", "found"]])}, "job-1", ${ROUNDS})`);
    const dismissed = await evaluate(`Fixture.looks(${JSON.stringify([...FOLDERS, ["api", "found"]])}, null, 1)`);
    return { started, same, added, settled, dismissed };
  });
  assert.equal(seen.started.armed, 2, "the found-folder poll and the intake poll are armed");
  assert.equal(seen.same.renders, seen.started.renders, `${ROUNDS} rounds that read the same folders and job render nothing`);
  assert.equal(seen.added.renders, seen.same.renders + 1, "a new folder renders once");
  assert.equal(seen.settled.renders, seen.added.renders, "the same folders again render nothing");
  assert.equal(seen.dismissed.renders, seen.settled.renders + 1, "the job going away renders once");
  for (const command of ["desktop_found_projects", "desktop_intake_status"]) {
    assert.equal(reads(seen.same.asked, command) - reads(seen.started.asked, command), ROUNDS, `${command} is read once a round`);
  }
});
