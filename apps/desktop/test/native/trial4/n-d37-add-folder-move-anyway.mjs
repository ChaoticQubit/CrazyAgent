import { existsSync, mkdirSync, renameSync } from "node:fs";
import { basename, isAbsolute, join } from "node:path";
import { click, shown, text, texts } from "../settings-steps.mjs";
import { appCall, git, inside, linkedWorktrees, scratchRepo, stamp } from "../trial4-projects.mjs";

const UI_MS = 30000;
const MOVE_MS = 120000;
const SCRATCH = process.env.TRIAL_ADD_SCRATCH ?? "";
const CLOSE_APPS = "Close editors, terminals and apps that use this folder first. Files saved while it moves can be lost.";
const WORKTREES = "Git worktrees linked to this folder stop working until you run git worktree repair in each.";

/** The XPath of one way to add a folder. Input: the way's label. Output: the selector. */
function way(label) {
  return `//*[@role="radiogroup"][@aria-label="How to add it"]//button[normalize-space(.)="${label}"]`;
}

/** The scratch folder a check makes its repository in: TRIAL_ADD_SCRATCH, an absolute folder
 * outside the projects folder, never the owner's own folders. Input: the projects folder.
 * Output: the folder path; throws "not run" before anything is made when it is missing. */
export function scratchBase(home) {
  if (!SCRATCH || !isAbsolute(SCRATCH)) {
    throw new Error("not run: needs TRIAL_ADD_SCRATCH, an absolute scratch folder outside the projects folder");
  }
  if (inside(SCRATCH, home) || inside(home, SCRATCH)) throw new Error(`not run: TRIAL_ADD_SCRATCH ${SCRATCH} overlaps the projects folder ${home}`);
  mkdirSync(SCRATCH, { recursive: true });
  return SCRATCH;
}

/** N-D37 (DEC-115 addendum): a scratch repo with a linked worktree is offered Move; the question
 * shows the data loss warning before Move anyway, and Move anyway moves it into the projects
 * folder, where git worktree repair mends the worktree. The repo is moved back out afterwards. */
export const checks = [{
  id: "N-D37",
  op: ["OP-19"],
  title: "Move anyway moves a repo the engine used to refuse, after clear warnings",
  why: "owner: \"ask the user ... do you still want to move it ... yes ... move it forcefully\"",
  after: "end",
  run: async (kit) => {
    if (typeof kit.chooseFolder !== "function") {
      throw new Error("not run: needs kit.chooseFolder(path) to answer the macOS folder chooser, which WebDriver cannot drive");
    }
    const home = (await appCall("desktop_projects_home", {}))?.path;
    if (!home) throw new Error("the app has no projects folder to move into");
    const made = scratchRepo(join(scratchBase(home), `ca-trial-4-anyway-move-${stamp(new Date())}`));
    const name = basename(made.repo);
    const landed = join(home, name);
    if (existsSync(landed)) throw new Error(`${landed} already exists in the projects folder`);

    await kit.go("#/projects/add/folder");
    await kit.until(() => shown("button=Choose folder"), UI_MS, "the Add folder page offered no Choose folder");
    await kit.chooseFolder(made.repo, "project");
    await click("button=Choose folder");
    await kit.until(async () => (await text("main")).includes("stays where it is."), UI_MS, `the Add folder page never took ${made.repo}`);
    await kit.until(() => shown(way("Move")), UI_MS, "Move was never offered for a repo with a linked worktree");
    await click(way("Move"));
    await click("button=Move into projects folder");
    await kit.until(async () => (await texts("dialog[open] h2")).includes(`Move ${name} anyway?`), UI_MS, "the Move anyway question did not open");
    const body = await text("dialog[open]");
    const focused = await browser.execute(() => document.activeElement?.textContent?.trim() ?? "");
    await kit.screen();
    const loss = body.indexOf(CLOSE_APPS);
    if (loss < 0) throw new Error(`the question never showed the data loss line before Move anyway: ${body}`);
    if (!(body.indexOf(WORKTREES) > loss)) throw new Error(`the worktree warning is missing or comes before the data loss line: ${body}`);
    if (!body.includes(made.worktree)) throw new Error(`the question did not list the linked worktree ${made.worktree}`);
    if (focused !== "Use where it is") throw new Error(`the question starts on ${JSON.stringify(focused)}, not Use where it is`);

    await click("//dialog[@open]//button[normalize-space(.)=\"Move anyway\"]");
    await kit.until(() => existsSync(join(landed, ".git")) && !existsSync(made.repo), MOVE_MS, `Move anyway did not move ${made.repo} into ${home}`);
    await kit.until(async () => (await kit.hash()) === "#/home", UI_MS, "the app did not go to Home after the move");
    await kit.screen();
    git(landed, ["worktree", "repair", made.worktree]);
    const head = git(made.worktree, ["rev-parse", "--abbrev-ref", "HEAD"]);
    renameSync(landed, made.repo);
    git(made.repo, ["worktree", "repair", made.worktree]);
    const back = linkedWorktrees(made.repo).join() === made.worktree;
    const seen = `${name}: question opened on Use where it is with the data loss line first and ${made.worktree} listed; Move anyway moved it to ${landed}; worktree repair gave branch ${head}`;
    if (head !== "trial4-side") throw new Error(`${seen}; but the repaired worktree is on ${head}`);
    if (!back) throw new Error(`${seen}; but moving it back out of the projects folder lost its worktree link`);
    return `${seen}; moved back to ${made.repo}`;
  },
}];

