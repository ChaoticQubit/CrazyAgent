import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const model = await import("../src/scm-model.ts");

const ID = "a".repeat(32);
const file = (extra = {}) => ({ path: "README.md", old_path: null, status: "modified", kind: "text", marks: [], ...extra });
const set = (extra = {}) => ({
  id: ID, agent: "Ada (@builder)", role: "sub", title: "Name the shop for gardens", state: "pending", waiting: false,
  head: "b".repeat(40), message: "Name the shop for gardens\n", worktree: "/work/agents/builder/",
  files: [file(), file({ path: "src/shop.ts", status: "added" }), file({ path: "old.txt", status: "deleted" })],
  restorable_until: null, problem: null, ...extra,
});

test("a comment targets the change set whose agent worktree holds the file, for every open state", () => {
  for (const state of ["pending", "conflict", "updating"]) {
    assert.deepEqual(model.commentTarget([set({ state })], "/work/agents/builder/README.md"), { id: ID, path: "README.md" }, state);
  }
  assert.deepEqual(model.commentTarget([set()], "/work/agents/builder/src/shop.ts"), { id: ID, path: "src/shop.ts" });
  const second = "c".repeat(32);
  const sets = [set({ worktree: "/work/agents/other", files: [file({ path: "x.md" })] }), set({ id: second })];
  assert.deepEqual(model.commentTarget(sets, "/work/agents/builder/README.md"), { id: second, path: "README.md" });
});

test("no comment goes to a closed or unread set, a deleted or unchanged file, or a look-alike folder", () => {
  const readme = "/work/agents/builder/README.md";
  assert.equal(model.commentTarget([set({ state: "accepted" })], readme), null);
  assert.equal(model.commentTarget([set({ state: "discarded", restorable_until: Date.now() + 60_000 })], readme), null);
  assert.equal(model.commentTarget([set({ problem: "changes_too_large", files: [] })], readme), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder/old.txt"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder/package.json"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder2/README.md"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builderREADME.md"), null);
  assert.equal(model.commentTarget([], readme), null);
});

test("a refused comment reads as the desktop's sentence for the team codes, and as Changes words otherwise", () => {
  const desktop = readFileSync(new URL("../../desktop/src/api-projects.ts", import.meta.url), "utf8");
  const sentences = Object.fromEntries([...desktop.matchAll(/^ {2}([a-z_]+): "([^"]+)",$/gm)].map((match) => [match[1], match[2]]));
  for (const code of ["team_conversation_full", "team_message_invalid", "project_setup_missing"]) {
    assert.equal(model.commentRefusal(code), sentences[code], code);
  }
  assert.equal(model.commentRefusal("changes_closed"), "These changes were already accepted or discarded, so the agent can't act on this comment.");
  assert.equal(model.commentRefusal("changes_not_found"), model.refusal("changes_not_found"));
  assert.equal(model.commentRefusal("made_up_code"), "Something went wrong. Try again. (made_up_code)");
});
