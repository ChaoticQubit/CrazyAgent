import { realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { click, shown, texts } from "../settings-steps.mjs";
import { SETTINGS } from "../trial3-steps.mjs";
import { appCall, appPaths, changeFiles, commitOutside, git, pathGap, setTruth, stamp } from "../trial4-projects.mjs";

/** The branch change sets land on: the trial's Land on setting, trial<n>-land from TRIAL_LAND_ON. */
export const LAND = SETTINGS.landOn;
const UI_MS = 30000;
const FOLLOW_MS = 90000;
const OPEN = ["pending", "conflict"];
const ROWS = "ul[aria-label=\"Change sets\"] > li";
const FILES = "ul[aria-label=\"Changed files\"] > li span.pj-mono";

/** The name a change set goes by in a finding. Input: the set. Output: its agent and id. */
function label(set) {
  return `${set.agent?.label ?? "agent"} (set ${set.id})`;
}

/** Compares one change set the app lists with git: the paths of every diff page against
 * git's paths from the set's base, the file count, the base being an ancestor of the head
 * and, for an open set, measured from the merge base of the land branch and its head
 * (DEC-107). Inputs: the repo and the set. Output: its files, its paths and the problems. */
async function compare(repo, set) {
  const files = await changeFiles(set.id);
  const listed = appPaths(files);
  const truth = setTruth(repo, set, LAND);
  const gap = pathGap(listed, truth.paths);
  const problems = [];
  if (gap.extra.length > 0) problems.push(`${label(set)} lists ${gap.extra.length} paths git does not change from its base: ${gap.extra.slice(0, 5).join(", ")}`);
  if (gap.missing.length > 0) problems.push(`${label(set)} leaves out ${gap.missing.length} changed paths: ${gap.missing.slice(0, 5).join(", ")}`);
  if (files.length !== set.files) problems.push(`${label(set)} says ${set.files} files but its diff lists ${files.length}`);
  if (!truth.ancestor) problems.push(`${label(set)} is measured from ${set.base.slice(0, 8)}, which is not an ancestor of its head`);
  if (OPEN.includes(set.state) && set.base !== truth.mergeBase) {
    problems.push(`${label(set)} is measured from ${set.base.slice(0, 8)}, not from the merge base ${truth.mergeBase.slice(0, 8)} of ${LAND} and its head`);
  }
  return { files, listed, problems };
}

/** Opens one change set in the Changes tab as the owner clicks its row, then waits until its
 * row shows its file count and the file list shows only its own files. Inputs: the kit, the
 * row index, the set and its files. Output: null, or what the tab showed instead. */
async function openRow(kit, index, set, files) {
  const row = `(//ul[@aria-label="Change sets"]/li)[${index + 1}]//button`;
  await click(row);
  return kit.waitFor(async () => {
    const lines = await texts(FILES);
    const rowText = ((await texts(ROWS))[index] ?? "").replace(/\s+/g, " ").trim();
    const current = await shown(`${row}[@aria-current="true"]`);
    const counted = new RegExp(`\\b${set.files} files?\\b`).test(rowText);
    const own = lines.length > 0 && lines.every((line) => files.some((file) => line.endsWith(file.path)));
    return { done: current && counted && (own || files.length === 0), seen: `row "${rowText}", ${lines.length} file lines: ${lines.slice(0, 3).join("; ")}` };
  }, UI_MS, `${label(set)} did not show its own ${set.files} files`).then(() => null, (failure) => failure.message);
}

/** N-D13 (B-19, OP-5, OP-54): each change set lists only the files its agents changed, and
 * after an owner commit on the land branch outside the app an open set shows only what still
 * changes. */
export const checks = [{
  id: "N-D13",
  op: ["OP-5", "OP-54"],
  title: "Change sets list only their own files and follow an outside commit",
  why: "owner: \"One agent has changed 418 files\"",
  after: "final-tree",
  run: async (kit) => {
    const repo = realpathSync(process.env.TRIAL_PROJECT);
    const id = kit.project();
    await kit.tab("changes");
    const sets = await appCall("desktop_changes", { projectId: id });
    if (!Array.isArray(sets) || sets.length === 0) throw new Error("Changes lists no change set to check");
    await kit.until(async () => (await texts(ROWS)).length === sets.length, UI_MS, `the Changes tab does not show the ${sets.length} change sets the app lists`);
    const problems = [];
    const before = new Map();
    for (const [index, set] of sets.entries()) {
      const found = await compare(repo, set);
      problems.push(...found.problems);
      before.set(set.id, found.listed);
      const shownProblem = await openRow(kit, index, set, found.files);
      if (shownProblem) problems.push(shownProblem);
    }
    await kit.screen();
    const sizes = sets.map((set) => `${label(set)} ${set.state} ${set.files} files`).join("; ");
    const open = sets.filter((set) => OPEN.includes(set.state));
    if (open.length === 0) {
      problems.push(`no open change set after the trial (${sizes}), so none can show what still changes after an outside commit`);
      throw new Error(problems.join("; "));
    }

    const file = `ca-trial-4-outside-${stamp(new Date())}.md`;
    const commit = commitOutside(repo, LAND, file, "An owner edit committed outside the app (trial 4 N-D13).\n", realpathSync(tmpdir()));
    const land = git(repo, ["rev-parse", LAND]);
    await kit.reopen("changes");
    const followed = await kit.waitFor(async () => {
      const now = await appCall("desktop_changes", { projectId: id });
      const notes = [];
      let done = true;
      for (const set of open) {
        const current = now.find((item) => item.id === set.id);
        if (!current || !OPEN.includes(current.state)) {
          done = false;
          notes.push(`${label(set)} is ${current ? current.state : "gone"}`);
          continue;
        }
        const found = await compare(repo, current);
        const gap = pathGap(found.listed, before.get(set.id));
        const outside = found.listed.includes(file);
        if (found.problems.length > 0 || outside || gap.extra.length > 0 || gap.missing.length > 0) done = false;
        notes.push(`${label(set)} ${found.listed.length} paths${outside ? `, lists ${file}` : ""}${found.problems.length > 0 ? `, ${found.problems.join(", ")}` : ""}`);
      }
      return { done, seen: notes.join("; ") };
    }, FOLLOW_MS, `after the outside commit ${commit.slice(0, 8)} on ${LAND} the open change sets did not show only what still changes`)
      .catch((failure) => {
        problems.push(failure.message);
        return "not followed";
      });
    const after = await appCall("desktop_changes", { projectId: id });
    const index = after.findIndex((set) => set.id === open[0].id);
    if (index < 0) problems.push(`after the outside commit ${label(open[0])} is no longer listed`);
    else {
      const shownProblem = await openRow(kit, index, after[index], await changeFiles(open[0].id));
      if (shownProblem) problems.push(`after the outside commit ${shownProblem}`);
      if ((await texts(FILES)).some((line) => line.endsWith(file))) problems.push(`after the outside commit ${label(open[0])} shows ${file}`);
    }
    await kit.screen();
    if (problems.length > 0) throw new Error(problems.join("; "));
    return `${sets.length} change sets match git from their base (${sizes}); owner commit ${commit.slice(0, 8)} adding ${file} on ${LAND} (now ${land.slice(0, 8)}): ${followed}`;
  },
}];
