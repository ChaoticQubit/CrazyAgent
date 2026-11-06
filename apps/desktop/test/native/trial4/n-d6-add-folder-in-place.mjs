import { existsSync } from "node:fs";
import { basename, join } from "node:path";
import { click, shown, text, texts } from "../settings-steps.mjs";
import { appCall, linkedWorktrees, outsideFolder, scratchRepo, stamp } from "../trial4-projects.mjs";

const UI_MS = 30000;
const START_MS = 120000;
const POOL = process.env.TRIAL_POOL ?? "";
const WAYS = "[role=\"radiogroup\"][aria-label=\"How to add it\"] button";
const SETUP_ROUTE = /^#\/projects\/([^/]+)\/setup$/;

/** The XPath of one way to add a folder. Input: the way's label. Output: the selector. */
function way(label) {
  return `//*[@role="radiogroup"][@aria-label="How to add it"]//button[normalize-space(.)="${label}"]`;
}

/** N-D6 (B-06, OP-19): an existing repo outside the projects folder, with one linked worktree,
 * is set up where it is; Add folder still offers Move for it, which asks first (DEC-115). */
export const checks = [{
  id: "N-D6",
  op: ["OP-19"],
  title: "Add a repo outside the projects folder where it is",
  why: "owner: \"why did we create another folder man\"",
  after: "end",
  run: async (kit) => {
    if (typeof kit.chooseFolder !== "function") {
      throw new Error("not run: needs kit.chooseFolder(path) to answer the macOS folder chooser, which WebDriver cannot drive");
    }
    const home = (await appCall("desktop_projects_home", {}))?.path;
    if (!home) throw new Error("the app has no projects folder to add next to");
    const made = scratchRepo(outsideFolder(home, stamp(new Date())));
    const name = basename(made.repo);

    await kit.go("#/projects/add/folder");
    await kit.until(() => shown("button=Choose folder"), UI_MS, "the Add folder page offered no Choose folder");
    await kit.chooseFolder(made.repo, "project");
    await click("button=Choose folder");
    await kit.until(async () => (await text("main")).includes("stays where it is."), UI_MS, `the Add folder page never took ${made.repo}`);
    await kit.until(() => shown(way("Copy")), UI_MS, "the ways to add the folder were never shown");
    await click(way("Copy"));
    await kit.until(async () => (await text("main")).includes("This folder has 1 linked git worktree."), UI_MS,
      "Copy never said the folder has 1 linked git worktree, so the page did not read it");
    const ways = (await texts(WAYS)).map((label) => label.trim());
    await kit.screen();
    await click(way("Use where it is"));
    await kit.until(async () => (await text("main")).includes("stays where it is."), UI_MS, "Use where it is did not come back");

    await click("button=Set up here");
    await kit.until(async () => (await texts("dialog[open] h2")).includes(`Set up ${name}`), UI_MS, "the Set up dialog did not open");
    if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)="${POOL}"]]`);
    await click("//dialog[@open]//button[normalize-space(.)=\"Start setup\"]", 60000);
    let route = "";
    await kit.until(async () => SETUP_ROUTE.test(route = await kit.hash()), START_MS, `setup did not open on ${name}`);
    const id = decodeURIComponent(route.match(SETUP_ROUTE)[1]);
    await kit.until(() => shown("ol[aria-label=\"Setup steps\"]"), UI_MS, "the setup page showed no setup steps");
    await kit.until(() => existsSync(join(made.repo, ".crazyagent")), START_MS, `setup made no .crazyagent folder in ${made.repo}`);
    await kit.screen();

    const kept = existsSync(made.repo) && existsSync(made.worktree) && linkedWorktrees(made.repo).join() === made.worktree;
    const copied = existsSync(join(home, name));
    const seen = `${name} (${made.repo}, linked worktree ${made.worktree}): setup page of project ${id} opened on it, .crazyagent made in place; ways offered: ${ways.join(", ")}`;
    if (!kept) throw new Error(`${seen}; but the folder or its linked worktree is no longer where it was`);
    if (copied) throw new Error(`${seen}; but ${join(home, name)} exists in the projects folder`);
    if (!ways.includes("Move")) throw new Error(`${seen}; but Move is not offered, so the owner cannot move it anyway`);
    return `${seen}; nothing in ${home}`;
  },
}];
