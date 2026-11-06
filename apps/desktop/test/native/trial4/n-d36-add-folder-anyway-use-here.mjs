import { existsSync, mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { click, shown, text, texts } from "../settings-steps.mjs";
import { appCall, stamp } from "../trial4-projects.mjs";
import { scratchBase } from "./n-d35-add-folder-move-anyway.mjs";

const UI_MS = 30000;
const START_MS = 120000;
const POOL = process.env.TRIAL_POOL ?? "";
const SETUP_ROUTE = /^#\/projects\/([^/]+)\/setup$/;
const CLOSE_APPS = "Close editors, terminals and apps that use this folder first. Files saved while it moves can be lost.";
const NO_GIT = "This folder has no git history. Agents' changes can't be shown or undone with git until you run git init here.";

/** The XPath of one way to add a folder. Input: the way's label. Output: the selector. */
function way(label) {
  return `//*[@role="radiogroup"][@aria-label="How to add it"]//button[normalize-space(.)="${label}"]`;
}

/** N-D36 (DEC-115 addendum): Move on a scratch folder with no git asks first; Use where it is,
 * the question's default, sets the folder up where it is and nothing lands in the projects folder. */
export const checks = [{
  id: "N-D36",
  op: ["OP-19"],
  title: "Use where it is in the Move anyway question sets the folder up in place",
  why: "owner: \"no ... keep it where it is and then add it to the projects\"",
  after: "end",
  run: async (kit) => {
    if (typeof kit.chooseFolder !== "function") {
      throw new Error("not run: needs kit.chooseFolder(path) to answer the macOS folder chooser, which WebDriver cannot drive");
    }
    const home = (await appCall("desktop_projects_home", {}))?.path;
    if (!home) throw new Error("the app has no projects folder to add next to");
    const folder = join(scratchBase(home), `ca-trial-4-anyway-here-${stamp(new Date())}`);
    if (existsSync(folder)) throw new Error(`${folder} already exists`);
    mkdirSync(folder);
    writeFileSync(join(folder, "README.md"), "# Trial 4 plain folder\n\nNo git history.\n");
    const real = realpathSync(folder);
    const name = basename(real);

    await kit.go("#/projects/add/folder");
    await kit.until(() => shown("button=Choose folder"), UI_MS, "the Add folder page offered no Choose folder");
    await kit.chooseFolder(real, "project");
    await click("button=Choose folder");
    await kit.until(async () => (await text("main")).includes("stays where it is."), UI_MS, `the Add folder page never took ${real}`);
    await kit.until(() => shown(way("Move")), UI_MS, "Move was never offered");
    await click(way("Move"));
    await click("button=Move into projects folder");
    await kit.until(async () => (await texts("dialog[open] h2")).includes(`Move ${name} anyway?`), UI_MS, "the Move anyway question did not open");
    const body = await text("dialog[open]");
    await kit.screen();
    if (!(body.indexOf(CLOSE_APPS) >= 0 && body.indexOf(NO_GIT) > body.indexOf(CLOSE_APPS))) {
      throw new Error(`the question did not show the data loss line, then the no git line: ${body}`);
    }

    await click("//dialog[@open]//button[normalize-space(.)=\"Use where it is\"]");
    await kit.until(async () => (await texts("dialog[open] h2")).includes(`Set up ${name}`), UI_MS, "Use where it is did not open the Set up dialog");
    if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)="${POOL}"]]`);
    await click("//dialog[@open]//button[normalize-space(.)=\"Start setup\"]", 60000);
    let route = "";
    await kit.until(async () => SETUP_ROUTE.test(route = await kit.hash()), START_MS, `setup did not open on ${name}`);
    const id = decodeURIComponent(route.match(SETUP_ROUTE)[1]);
    await kit.until(() => existsSync(join(real, ".crazyagent")), START_MS, `setup made no .crazyagent folder in ${real}`);
    await kit.screen();
    const seen = `${name} (${real}): Move asked first with the data loss line, then no git; Use where it is opened setup of project ${id} in place`;
    if (!existsSync(join(real, "README.md"))) throw new Error(`${seen}; but the folder is no longer where it was`);
    if (existsSync(join(home, name))) throw new Error(`${seen}; but ${join(home, name)} exists in the projects folder`);
    return `${seen}; nothing in ${home}`;
  },
}];
