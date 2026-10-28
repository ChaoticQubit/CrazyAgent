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
    assert.deepEqual(model.commentTarget([set({ state })], "/work/agents/builder/README.md"), { id: ID, path: "README.md", agent: "Ada (@builder)" }, state);
  }
  assert.deepEqual(model.commentTarget([set()], "/work/agents/builder/src/shop.ts"), { id: ID, path: "src/shop.ts", agent: "Ada (@builder)" });
  const second = "c".repeat(32);
  const sets = [set({ worktree: "/work/agents/other", files: [file({ path: "x.md" })] }), set({ id: second })];
  assert.deepEqual(model.commentTarget(sets, "/work/agents/builder/README.md"), { id: second, path: "README.md", agent: "Ada (@builder)" });
});

test("no comment goes to a closed or unread set, a master conversation's set, a deleted or unchanged file, or a look-alike folder", () => {
  const readme = "/work/agents/builder/README.md";
  assert.equal(model.commentTarget([set({ role: "master", agent: "Master" })], readme), null);
  assert.equal(model.commentTarget([set({ state: "accepted" })], readme), null);
  assert.equal(model.commentTarget([set({ state: "discarded", restorable_until: Date.now() + 60_000 })], readme), null);
  assert.equal(model.commentTarget([set({ problem: "changes_too_large", files: [] })], readme), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder/old.txt"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder/package.json"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builder2/README.md"), null);
  assert.equal(model.commentTarget([set()], "/work/agents/builderREADME.md"), null);
  assert.equal(model.commentTarget([], readme), null);
});

test("a comment on the agent side of a diff, the recorded head's text, goes to that set's agent on the same terms", () => {
  assert.deepEqual(model.headCommentTarget([set()], `/${ID}/README.md`), { id: ID, path: "README.md", agent: "Ada (@builder)" });
  assert.deepEqual(model.headCommentTarget([set()], `/${ID}/src/shop.ts`), { id: ID, path: "src/shop.ts", agent: "Ada (@builder)" });
  assert.equal(model.headCommentTarget([set()], `/${ID}/old.txt`), null);
  assert.equal(model.headCommentTarget([set()], `/${ID}/package.json`), null);
  assert.equal(model.headCommentTarget([set({ role: "master", agent: "Master" })], `/${ID}/README.md`), null);
  assert.equal(model.headCommentTarget([set({ state: "accepted" })], `/${ID}/README.md`), null);
  assert.equal(model.headCommentTarget([set()], `/${"c".repeat(32)}/README.md`), null);
  assert.equal(model.headCommentTarget([set()], `/${ID}/../README.md`), null);
  assert.equal(model.headCommentTarget([set()], "/README.md"), null);
});

test("every IDE write the engine could not confirm in time reads as a check-first sentence, never a plain retry", () => {
  const bridge = readFileSync(new URL("../../../crates/crazyagent/src/ide/bridge.rs", import.meta.url), "utf8");
  const codes = [...new Set(bridge.match(/"ide_[a-z_]+_outcome_unknown"/g).map((quoted) => quoted.slice(1, -1)))].sort();
  const source = readFileSync(new URL("../src/scm-model.ts", import.meta.url), "utf8");
  const table = source.match(/const UNCONFIRMED: Record<string, string> = \{\n([\s\S]*?)\n\};/)?.[1] ?? "";
  const sentences = [...table.matchAll(/^ {2}([a-z_]+): "/gm)].map((match) => match[1]).sort();
  assert.ok(codes.includes("ide_git_outcome_unknown") && codes.length >= 6, codes.join(" "));
  assert.deepEqual(sentences, codes, "every outcome-unknown code the bridge answers has one sentence, and no sentence names a code it never answers");
  for (const code of codes) {
    const sentence = model.unconfirmed(code);
    assert.match(sentence, /^CrazyAgent couldn't confirm /, code);
    assert.doesNotMatch(sentence, /\(|Try again\./, code);
  }
  assert.equal(model.commentRefusal("ide_comment_outcome_unknown"), model.unconfirmed("ide_comment_outcome_unknown"));
  assert.equal(model.unconfirmed("changes_outcome_unknown"), undefined);
  assert.equal(model.unconfirmed("ide_request_timeout"), undefined);
});

test("a refused comment reads as the desktop's sentence for the team codes, and as Changes words otherwise", () => {
  const desktop = readFileSync(new URL("../../desktop/src/api-projects.ts", import.meta.url), "utf8");
  const sentences = Object.fromEntries([...desktop.matchAll(/^ {2}([a-z_]+): "([^"]+)",$/gm)].map((match) => [match[1], match[2]]));
  for (const code of ["team_conversation_full", "team_message_invalid", "project_setup_missing", "team_agent_not_found"]) {
    assert.equal(model.commentRefusal(code), sentences[code], code);
  }
  assert.equal(model.commentRefusal("changes_closed"), "These changes were already accepted or discarded, so the agent can't act on this comment.");
  assert.equal(model.commentRefusal("changes_not_found"), model.refusal("changes_not_found"));
  assert.equal(model.commentRefusal("made_up_code"), "Something went wrong. Try again. (made_up_code)");
  for (const code of ["ide_comment_master_unavailable", "ide_comment_too_long", "team_agent_not_running", "team_agent_unreachable"]) {
    assert.doesNotMatch(model.commentRefusal(code), /Something went wrong/, code);
  }
});

test("a sent comment names its agent as one plain line, and the byte bound matches the engine's", () => {
  assert.equal(model.commentSent("Ada (@builder)"), "Sent to Ada (@builder) with its file and line.");
  assert.equal(model.commentSent("Ada\u202e\nx"), "Sent to Ada\ufffd\ufffdx with its file and line.");
  const bridge = readFileSync(new URL("../../../crates/crazyagent/src/ide/bridge.rs", import.meta.url), "utf8");
  assert.equal(Number(bridge.match(/const COMMENT_BYTES_MAX: usize = (\d+);/)[1]), model.COMMENT_BYTES_MAX);
  assert.equal(model.commentTooLong("x".repeat(model.COMMENT_BYTES_MAX)), false);
  assert.equal(model.commentTooLong("x".repeat(model.COMMENT_BYTES_MAX + 1)), true);
  assert.equal(model.commentTooLong("\u00e9".repeat(model.COMMENT_BYTES_MAX / 2)), false);
  assert.equal(model.commentTooLong("\u00e9".repeat(model.COMMENT_BYTES_MAX / 2 + 1)), true);
});
