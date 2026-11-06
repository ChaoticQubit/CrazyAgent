import assert from "node:assert/strict";
import test from "node:test";
import { EDIT_ANYWAY, agentOf, editAnywayQuestion, insideFolder } from "../out/worktree.js";

const worktree = "/Users/owner/app/.crazyagent/worktrees/agent-7";

test("an agent worktree folder names its team agent", () => {
  assert.equal(agentOf(worktree), "team-7");
  assert.equal(agentOf(`${worktree}/`), "team-7");
  assert.equal(agentOf("/Users/owner/app/.crazyagent/worktrees/agent-12345"), "team-12345");
  for (const folder of [
    "/Users/owner/app",
    "/Users/owner/app/.crazyagent/worktrees/agent-0",
    "/Users/owner/app/.crazyagent/worktrees/agent-07",
    "/Users/owner/app/.crazyagent/worktrees/agent-7x",
    "/Users/owner/app/.crazyagent/worktrees/agent-",
    "/Users/owner/app/worktrees/agent-7/src",
  ]) {
    assert.equal(agentOf(folder), null, folder);
  }
});

test("a file's path is taken relative to its worktree with forward slashes", () => {
  assert.equal(insideFolder(worktree, `${worktree}/README.md`), "README.md");
  assert.equal(insideFolder(worktree, `${worktree}/src/deep/lib.rs`), "src/deep/lib.rs");
  for (const file of [worktree, `${worktree}/`, "/Users/owner/app/README.md", "/Users/owner/app/.crazyagent/worktrees/agent-77/a"]) {
    assert.equal(insideFolder(worktree, file), null, file);
  }
});

test("the question names the agent running in the worktree", () => {
  assert.equal(editAnywayQuestion("Ada (@builder)"), "Ada (@builder) is running in this worktree. Edit anyway?");
  assert.equal(EDIT_ANYWAY, "Edit anyway");
});
