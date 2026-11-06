import assert from "node:assert/strict";
import test from "node:test";
import { revealPlace } from "../out/reveal-path.js";

const project = "/home/owner/fieldnotes";
const worktree = `${project}/.crazyagent/worktrees/agent-3`;

test("a revealed file opens in the deepest workspace folder that holds it", () => {
  const folders = [project, `${project}/.crazyagent/worktrees/agent-1`, worktree];
  assert.deepEqual(revealPlace(folders, `${project}/context/brief.md`), {
    index: 0,
    parts: ["context", "brief.md"],
  });
  assert.deepEqual(revealPlace(folders, `${worktree}/src/app.rs`), { index: 2, parts: ["src", "app.rs"] });
  assert.deepEqual(revealPlace(folders, worktree), { index: 2, parts: [] });
  assert.deepEqual(revealPlace([`${project}/`], `${project}/notes.md`), { index: 0, parts: ["notes.md"] });
});

test("a path outside every workspace folder or with a dot part is not shown", () => {
  const folders = [project, worktree];
  for (const path of [
    "/home/owner/other/notes.md",
    `${project}-copy/notes.md`,
    `${project}/../secrets.txt`,
    `${project}/./notes.md`,
    `${project}//notes.md`,
    "relative/notes.md",
    "",
  ]) {
    assert.equal(revealPlace(folders, path), undefined, path);
  }
  assert.equal(revealPlace([], `${project}/notes.md`), undefined);
});
