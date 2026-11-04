import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative } from "node:path";

const GIT_MS = 30000;
const IDENTITY = ["-c", "user.name=CrazyAgent Trial 4", "-c", "user.email=trial4@example.invalid", "-c", "commit.gpgsign=false"];

/** Runs git in a folder as an owner's terminal does, with a fixed identity and no signing
 * prompt. Inputs: the folder and the arguments. Output: git's output, trimmed; throws on a
 * non-zero exit. */
export function git(folder, args) {
  return execFileSync("git", ["-C", folder, ...IDENTITY, ...args], { encoding: "utf8", timeout: GIT_MS, env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } }).trim();
}

/** Whether one commit is an ancestor of another. Inputs: the repo and the two commits.
 * Output: a boolean; throws when git cannot tell. */
export function isAncestor(repo, older, newer) {
  const result = spawnSync("git", ["-C", repo, "merge-base", "--is-ancestor", older, newer], { encoding: "utf8", timeout: GIT_MS });
  assert([0, 1].includes(result.status), `git merge-base --is-ancestor ${older} ${newer}: ${result.stderr}`);
  return result.status === 0;
}

/** Whether a path is a folder or inside it. Inputs: the path and the folder. Output: a boolean. */
export function inside(path, folder) {
  const rest = relative(folder, path);
  return rest === "" || (!rest.startsWith("..") && !isAbsolute(rest));
}

/** A minute stamp that names one trial's scratch folders. Input: the time. Output: YYYYMMDDHHmm. */
export function stamp(now) {
  return now.toISOString().replace(/\D/g, "").slice(0, 12);
}

/** The folder N-D6 adds: next to the projects folder, never inside it. Inputs: the projects
 * folder and the minute stamp. Output: the folder path. */
export function outsideFolder(home, minute) {
  assert.match(minute, /^[0-9]{12}$/);
  assert(isAbsolute(home), `the projects folder is an absolute path: ${home}`);
  const folder = join(dirname(home), `ca-trial-4-outside-${minute}`);
  assert(!inside(folder, home), `${folder} is inside the projects folder ${home}`);
  return folder;
}

/** The folder N-D15 seeds, removes and adds again: its own project in the projects folder, so
 * the trial project stays as it is. Inputs: the projects folder and the minute stamp. Output:
 * the folder path. */
export function readdFolder(home, minute) {
  assert.match(minute, /^[0-9]{12}$/);
  assert(isAbsolute(home), `the projects folder is an absolute path: ${home}`);
  const folder = join(home, `ca-trial-4-readd-${minute}`);
  assert(inside(folder, home) && folder !== home, `${folder} is not inside the projects folder ${home}`);
  return folder;
}

/** Makes a git repository with one commit on main. Inputs: the new folder, which must not
 * exist, and the README text. Output: the folder's real path. */
export function seedRepo(folder, readme) {
  assert(!existsSync(folder), `${folder} already exists`);
  mkdirSync(folder);
  git(folder, ["init", "-q", "-b", "main"]);
  writeFileSync(join(folder, "README.md"), readme);
  git(folder, ["add", "README.md"]);
  git(folder, ["commit", "-q", "-m", "Seed the trial 4 scratch folder"]);
  return realpathSync(folder);
}

/** Makes a scratch git repository with one commit and one linked worktree on its own branch,
 * the shape whose move LB-8 lost. Input: the new folder, which must not exist. Output: the
 * real paths of the repository and of its linked worktree. */
export function scratchRepo(folder) {
  const worktree = `${folder}-wt`;
  assert(!existsSync(folder) && !existsSync(worktree), `${folder} or its worktree already exists`);
  seedRepo(folder, "# Trial 4 outside folder\n\nA scratch repository outside the projects folder.\n");
  git(folder, ["worktree", "add", "-q", "-b", "trial4-side", worktree]);
  assert.equal(linkedWorktrees(folder).length, 1, "the scratch repository has one linked worktree");
  return { repo: realpathSync(folder), worktree: realpathSync(worktree) };
}

/** The linked worktrees of a repository, without its main checkout. Input: the repository.
 * Output: their real paths, or the listed path of one that is gone. */
export function linkedWorktrees(repo) {
  const paths = git(repo, ["worktree", "list", "--porcelain"]).split("\n").filter((line) => line.startsWith("worktree ")).map((line) => line.slice(9));
  return paths.slice(1).map((path) => existsSync(path) ? realpathSync(path) : path);
}

/** The paths an app change set lists, with a rename as its old and new path, which is how
 * git lists it without rename detection. Input: the files of every diff page. Output: the
 * paths, sorted. */
export function appPaths(files) {
  return [...new Set(files.flatMap((file) => file.status === "renamed" && file.old_path ? [file.old_path, file.path] : [file.path]))].sort();
}

/** What git says a change set changes and where it is measured from: the paths from its base
 * to its head, whether the base is an ancestor of the head, and the merge base of the land
 * branch's tip and the head (DEC-107). Inputs: the repo, the set and the land branch.
 * Output: { paths, ancestor, mergeBase }. */
export function setTruth(repo, set, land) {
  assert.match(set.base, /^[0-9a-f]{40,64}$/, `the change set base is a commit: ${set.base}`);
  assert.match(set.head, /^[0-9a-f]{40,64}$/, `the change set head is a commit: ${set.head}`);
  const listed = git(repo, ["diff", "--no-renames", "--name-only", set.base, set.head]);
  return {
    paths: listed ? [...new Set(listed.split("\n"))].sort() : [],
    ancestor: isAncestor(repo, set.base, set.head),
    mergeBase: git(repo, ["merge-base", land, set.head]),
  };
}

/** The paths one list has that another lacks, both ways. Inputs: what the app lists and what
 * git lists. Output: { extra, missing }, each sorted. */
export function pathGap(listed, truth) {
  const has = new Set(truth);
  const shown = new Set(listed);
  return { extra: [...shown].filter((path) => !has.has(path)).sort(), missing: [...has].filter((path) => !shown.has(path)).sort() };
}

/** Commits one new file on a branch from a temporary linked worktree, as the owner does from
 * a terminal, then removes that worktree. Refuses a branch that is checked out anywhere, so
 * no checkout in use changes under the app. Inputs: the repo, the branch, the file name, its
 * text and the folder the temporary worktree goes in. Output: the new commit. */
export function commitOutside(repo, branch, name, text, scratch) {
  assert.match(name, /^[A-Za-z0-9._-]{1,80}$/, "the outside file is one plain name");
  const listing = git(repo, ["worktree", "list", "--porcelain"]).split("\n");
  assert(!listing.includes(`branch refs/heads/${branch}`), `${branch} is checked out in a worktree; an outside commit there would change a checkout in use`);
  const place = join(scratch, `ca-trial-4-land-${process.pid}-${Date.now()}`);
  git(repo, ["worktree", "add", "-q", place, branch]);
  try {
    writeFileSync(join(place, name), text);
    git(place, ["add", "--", name]);
    git(place, ["commit", "-q", "-m", `Owner edit outside the app: ${name}`]);
    return git(place, ["rev-parse", "HEAD"]);
  } finally {
    git(repo, ["worktree", "remove", place]);
  }
}

/** Calls one of the app's own commands from its window, as its pages do. Inputs: the command
 * and its arguments. Output: the answer; throws with the refusal when the command fails. */
export async function appCall(command, args) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }),
      (failure) => done({ failed: typeof failure === "string" ? failure : JSON.stringify(failure) }));
  }, command, args);
  if ("failed" in answer) throw new Error(`${command} failed: ${answer.failed}`);
  const value = answer.value;
  const wrapped = value !== null && typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 1 && "value" in value;
  return wrapped ? value.value : value;
}

/** Reads every diff page of one change set through the app. Input: the change set id.
 * Output: the files of all pages; throws past 100 pages. */
export async function changeFiles(id) {
  const files = [];
  for (let page = 0, pages = 0; page !== null; pages += 1) {
    assert(pages < 100, `change set ${id} has more than 100 diff pages`);
    const diff = await appCall("desktop_change_diff", { id, page });
    assert(Array.isArray(diff?.files), `desktop_change_diff answered no files: ${JSON.stringify(diff).slice(0, 300)}`);
    files.push(...diff.files);
    page = diff.next ?? null;
  }
  return files;
}

/** Finds a "could not start" message in what the window shows. Input: the text. Output: the
 * line that says it, or null. */
export function couldNotStart(text) {
  return text.split("\n").find((line) => /could\s*(?:not|n['’]t)\s+start/i.test(line)) ?? null;
}
