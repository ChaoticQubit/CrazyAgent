import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { appPaths, commitOutside, couldNotStart, git, inside, linkedWorktrees, outsideFolder, pathGap, readdFolder, scratchRepo, seedRepo, setTruth, stamp } from "./trial4-projects.mjs";

/** Makes a scratch folder and removes it after the body. Input: the body, given the folder. */
function inScratch(body) {
  const folder = realpathSync(mkdtempSync(join(tmpdir(), "ca-trial-4-test-")));
  try {
    return body(folder);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
}

/** Builds a repository whose main checkout stays on main, a land branch with one owner commit
 * and an agent branch forked from main that modifies, adds and renames a file. Input: the
 * folder. Output: the repo, the agent head and the fork point. */
function agentRepo(folder) {
  const repo = join(folder, "project");
  mkdirSync(repo);
  git(repo, ["init", "-q", "-b", "main"]);
  writeFileSync(join(repo, "keep.md"), "keep\n");
  writeFileSync(join(repo, "edit.md"), "one\n");
  writeFileSync(join(repo, "old.md"), "a file that moves\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-q", "-m", "seed"]);
  const fork = git(repo, ["rev-parse", "HEAD"]);
  git(repo, ["branch", "trial3-land"]);
  git(repo, ["switch", "-q", "-c", "agent"]);
  writeFileSync(join(repo, "edit.md"), "two\n");
  writeFileSync(join(repo, "new.md"), "added\n");
  git(repo, ["mv", "old.md", "moved.md"]);
  git(repo, ["commit", "-q", "-am", "agent work"]);
  git(repo, ["add", "new.md"]);
  git(repo, ["commit", "-q", "-m", "agent adds a file"]);
  const head = git(repo, ["rev-parse", "HEAD"]);
  git(repo, ["switch", "-q", "main"]);
  return { repo, head, fork };
}

test("the outside folder sits next to the projects folder and never inside it", () => {
  const minute = stamp(new Date("2026-11-03T13:45:09Z"));
  assert.equal(minute, "202611031345");
  assert.equal(outsideFolder("/Users/o/Projects/home", minute), "/Users/o/Projects/ca-trial-4-outside-202611031345");
  assert(!inside(outsideFolder("/Users/o/Projects/home", minute), "/Users/o/Projects/home"));
  assert.throws(() => outsideFolder("/", minute), /inside the projects folder/);
  assert.throws(() => outsideFolder("Projects", minute), /absolute path/);
  assert.throws(() => outsideFolder("/Users/o/Projects", "2026"));
});

test("a scratch repository has one commit and one linked worktree, and an existing folder is refused", () => inScratch((folder) => {
  const made = scratchRepo(join(folder, "ca-trial-4-outside-202611031345"));
  assert.deepEqual(linkedWorktrees(made.repo), [made.worktree]);
  assert.equal(git(made.repo, ["rev-list", "--count", "HEAD"]), "1");
  assert.equal(git(made.worktree, ["branch", "--show-current"]), "trial4-side");
  assert.throws(() => scratchRepo(made.repo), /already exists/);
}));

test("the re-add folder sits inside the projects folder and is a git repository with one commit and no worktree", () => inScratch((folder) => {
  const minute = stamp(new Date("2026-11-03T16:40:00Z"));
  assert.equal(readdFolder("/Users/o/CrazyAgent", minute), "/Users/o/CrazyAgent/ca-trial-4-readd-202611031640");
  assert(inside(readdFolder("/Users/o/CrazyAgent", minute), "/Users/o/CrazyAgent"));
  assert.throws(() => readdFolder("CrazyAgent", minute), /absolute path/);
  assert.throws(() => readdFolder("/Users/o/CrazyAgent", "2026"));
  const repo = seedRepo(readdFolder(folder, minute), "# readd\n");
  assert.equal(repo, join(folder, "ca-trial-4-readd-202611031640"));
  assert.equal(git(repo, ["rev-list", "--count", "HEAD"]), "1");
  assert.equal(git(repo, ["branch", "--show-current"]), "main");
  assert.equal(git(repo, ["status", "--porcelain"]), "");
  assert.deepEqual(linkedWorktrees(repo), []);
  assert.throws(() => seedRepo(repo, "# again\n"), /already exists/);
}));

test("git's paths of a change set match the app's, with a rename as two paths, and a stale base is told apart", () => inScratch((folder) => {
  const { repo, head, fork } = agentRepo(folder);
  const truth = setTruth(repo, { base: fork, head }, "trial3-land");
  assert.deepEqual(truth.paths, ["edit.md", "moved.md", "new.md", "old.md"]);
  assert.equal(truth.ancestor, true);
  assert.equal(truth.mergeBase, fork);
  const app = [
    { path: "edit.md", status: "modified" },
    { path: "moved.md", old_path: "old.md", status: "renamed" },
    { path: "new.md", status: "added" },
  ];
  assert.deepEqual(appPaths(app), truth.paths);
  assert.deepEqual(pathGap(appPaths(app), truth.paths), { extra: [], missing: [] });
  assert.deepEqual(pathGap(["edit.md", "keep.md"], truth.paths), { extra: ["keep.md"], missing: ["moved.md", "new.md", "old.md"] });
  const unrelated = git(repo, ["commit-tree", "-m", "unrelated", git(repo, ["hash-object", "-t", "tree", "/dev/null"])]);
  assert.equal(setTruth(repo, { base: unrelated, head }, "trial3-land").ancestor, false);
  assert.throws(() => setTruth(repo, { base: "main", head }, "trial3-land"), /base is a commit/);
}));

test("an outside commit lands on the branch through a temporary worktree and leaves a set's paths alone", () => inScratch((folder) => {
  const { repo, head, fork } = agentRepo(folder);
  const scratch = join(folder, "scratch");
  mkdirSync(scratch);
  const before = setTruth(repo, { base: fork, head }, "trial3-land");
  const commit = commitOutside(repo, "trial3-land", "ca-trial-4-outside-edit.md", "edited outside the app\n", scratch);
  assert.equal(git(repo, ["rev-parse", "trial3-land"]), commit);
  assert.equal(git(repo, ["show", `${commit}:ca-trial-4-outside-edit.md`]), "edited outside the app");
  assert.deepEqual(linkedWorktrees(repo), [], "the temporary worktree is removed");
  assert.equal(git(repo, ["branch", "--show-current"]), "main", "the main checkout stays on main");
  const after = setTruth(repo, { base: before.mergeBase, head }, "trial3-land");
  assert.equal(after.mergeBase, before.mergeBase);
  assert.deepEqual(after.paths, before.paths);
  assert(!after.paths.includes("ca-trial-4-outside-edit.md"));
  assert.throws(() => commitOutside(repo, "main", "x.md", "x\n", scratch), /checked out in a worktree/);
  assert.throws(() => commitOutside(repo, "trial3-land", "../x.md", "x\n", scratch), /one plain name/);
  assert(!existsSync(join(repo, "x.md")));
}));

test("the projects checks N-D6, N-D13 and N-D15 have the trial 4 shape and run after a real step", async () => {
  const { TRIAL3_STEPS } = await import("./trial3-steps.mjs");
  const slugs = new Set([...TRIAL3_STEPS.map((step) => step.slug), "end"]);
  const found = [];
  for (const file of ["n-d6-add-folder-in-place.mjs", "n-d13-changes-own-files.mjs", "n-d15-readd-project.mjs"]) {
    const { checks } = await import(`./trial4/${file}`);
    assert(Array.isArray(checks) && checks.length === 1, `${file} exports one check`);
    const [check] = checks;
    assert.equal(file.split("-").slice(0, 2).join("-"), check.id.toLowerCase(), `${file} is named after its check id`);
    assert(check.op.length > 0 && check.op.every((op) => /^OP-[0-9]+$/.test(op)), `${check.id} names its OP rows`);
    assert(check.title.length > 0 && check.why.startsWith("owner: "), `${check.id} has a title and an owner why`);
    assert(slugs.has(check.after), `${check.id} runs after ${check.after}, a trial 3 step or end`);
    assert.equal(typeof check.run, "function");
    found.push(check.id);
  }
  assert.deepEqual(found, ["N-D6", "N-D13", "N-D15"]);
});

test("N-D13 measures change sets from the land branch the runner names in TRIAL_LAND_ON", () => {
  const module = new URL("./trial4/n-d13-changes-own-files.mjs", import.meta.url).href;
  const read = `const { LAND } = await import(${JSON.stringify(module)}); process.stdout.write(String(LAND));`;
  const child = spawnSync(process.execPath, ["--input-type=module", "-e", read], { env: { ...process.env, TRIAL_LAND_ON: "trial4-land" }, encoding: "utf8", timeout: 30000 });
  assert.equal(child.status, 0, child.stderr);
  assert.equal(child.stdout, "trial4-land");
});

test("N-D6 says it is not run without the folder chooser helper, before it makes any folder", async () => {
  const { checks: [check] } = await import("./trial4/n-d6-add-folder-in-place.mjs");
  await assert.rejects(check.run({}), { message: /^not run: needs kit\.chooseFolder\(path\)/ });
});

test("N-D15 seeds its own project and never reads or replaces the trial project", () => {
  const source = readFileSync(new URL("./trial4/n-d15-readd-project.mjs", import.meta.url), "utf8");
  assert.doesNotMatch(source, /TRIAL_PROJECT|kit\.project\(|kit\.tab\(|run\.project/);
  assert.match(source, /seedRepo\(readdFolder\(home, stamp\(new Date\(\)\)\)/);
});

test("a could not start message is found in any of its spellings", () => {
  assert.equal(couldNotStart("Setup\nThe setup could not start.\nRetry"), "The setup could not start.");
  assert.equal(couldNotStart("The master couldn't start: no repository"), "The master couldn't start: no repository");
  assert.equal(couldNotStart("It couldn’t start"), "It couldn’t start");
  assert.equal(couldNotStart("Build the code graph\nDone\nMap the project\nWorking"), null);
});

test("the Move anyway checks N-D35 and N-D36 have the trial 4 shape and say not run before they make a folder", async () => {
  for (const [file, id] of [["n-d35-add-folder-move-anyway.mjs", "N-D35"], ["n-d36-add-folder-anyway-use-here.mjs", "N-D36"]]) {
    const { checks } = await import(`./trial4/${file}`);
    assert(Array.isArray(checks) && checks.length === 1, `${file} exports one check`);
    const [check] = checks;
    assert.equal(check.id, id);
    assert.deepEqual(check.op, ["OP-19"]);
    assert.equal(check.after, "end");
    assert(check.why.startsWith("owner: "), `${id} has an owner why`);
    await assert.rejects(check.run({}), { message: /^not run: needs kit\.chooseFolder\(path\)/ }, id);
  }
  const { scratchBase } = await import("./trial4/n-d35-add-folder-move-anyway.mjs");
  assert.throws(() => scratchBase("/Users/owner/CrazyAgent"), /^Error: not run: needs TRIAL_ADD_SCRATCH/);
});
