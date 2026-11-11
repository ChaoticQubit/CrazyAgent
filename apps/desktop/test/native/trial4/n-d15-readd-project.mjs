import { basename } from "node:path";
import { click, shown, text, texts, toasts, type } from "../settings-steps.mjs";
import { appCall, couldNotStart, readdFolder, seedRepo, stamp } from "../trial4-projects.mjs";

const UI_MS = 30000;
const FOUND_MS = 60000;
const SETUP_MS = Number(process.env.TRIAL_SETUP_MS ?? 1800000);
const POOL = process.env.TRIAL_POOL ?? "";
const STEPS = "ol[aria-label=\"Setup steps\"] > li";
const CONFIRM = "//button[normalize-space(.)=\"Confirm and finish setup\"]";
const REMOVE_SECTION = "//div[h2[normalize-space(.)=\"Remove project\"]]//button[normalize-space(.)=\"Remove project\"]";

/** The XPath of one folder's row in Home's projects folder list. Input: the folder name.
 * Output: the selector. */
function homeRow(name) {
  return `//li[contains(concat(" ", normalize-space(@class), " "), " in-row ")][.//strong[normalize-space(.)="${name}"]]`;
}

/** Reads what the window shows and remembers every "could not start" it says, so a message
 * that comes and goes between two looks is still caught. Input: the list of sightings.
 * Output: the window text. */
async function look(sightings) {
  const shownText = `${await text("main, dialog[open]")}\n${(await toasts()).join("\n")}`;
  const line = couldNotStart(shownText);
  if (line && !sightings.includes(line)) sightings.push(line);
  return shownText;
}

/** Reads each setup step's title and status word. Output: [{ title, word }] in page order. */
async function steps() {
  const rows = await texts(STEPS);
  return rows.map((row) => {
    const title = ["Build the code graph", "Map the project"].find((name) => row.startsWith(name)) ?? row.slice(0, 30);
    const word = ["Done", "Working", "Waiting", "Failed", "Off", "Not installed", "Skipped"].find((status) => row.trimEnd().endsWith(status)) ?? row.slice(-30);
    return { title, word };
  });
}

/** Sets up a folder Home lists, as the owner does: Set up this project, the pool, both setup
 * steps done, then Confirm and finish setup. Inputs: the kit, the folder name, the list of
 * "could not start" sightings and the setup's name in a finding. Output: { id, words };
 * throws when Home offers another action, a step fails or the project does not open. */
async function setUp(kit, name, sightings, phase) {
  const row = homeRow(name);
  await kit.until(async () => {
    await look(sightings);
    return shown(row);
  }, FOUND_MS, `${phase}: Home never listed ${name}`);
  const action = (await $(`${row}//button`).getText()).trim();
  await kit.screen();
  if (action !== "Set up this project") throw new Error(`${phase}: Home offers ${action} for ${name}, not Set up this project`);

  await click(`${row}//button`);
  await kit.until(async () => (await texts("dialog[open] h2")).includes(`Set up ${name}`), UI_MS, `${phase}: the Set up dialog did not open`);
  if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)="${POOL}"]]`);
  await click("//dialog[@open]//button[normalize-space(.)=\"Start setup\"]", 60000);
  let route = "";
  await kit.until(async () => {
    await look(sightings);
    return /^#\/projects\/[^/]+\/setup$/.test(route = await kit.hash());
  }, FOUND_MS * 2, `${phase}: setup of ${name} did not start`);
  const id = decodeURIComponent(route.split("/")[2]);

  let failed = null;
  const words = await kit.waitFor(async () => {
    await look(sightings);
    const now = await steps();
    failed = now.find((step) => step.word === "Failed") ?? null;
    const done = failed !== null || now.length === 2 && now.every((step) => step.word === "Done") && await shown(CONFIRM) && await $(CONFIRM).isEnabled();
    return { done, seen: now.map((step) => `${step.title}: ${step.word}`).join(", ") || "no setup steps shown" };
  }, SETUP_MS, `${phase}: setup of ${name} did not finish both steps`);
  await kit.screen();
  if (failed) throw new Error(`${phase}: setup of ${name}: ${failed.title} failed (${words})`);
  if (await shown("#ob-name") && (await $("#ob-name").getValue()).trim() === "") await type("#ob-name", name);
  await click(CONFIRM);
  await kit.until(async () => {
    await look(sightings);
    return /^#\/projects\/[^/]+\/overview$/.test(await kit.hash());
  }, UI_MS * 2, `${phase}: the set-up project ${name} did not open`);
  await look(sightings);
  await kit.screen();
  return { id, words };
}

/** N-D15 (B-23, OP-55): a project removed from the app and added again from the same folder
 * sets up with both steps done and never says "could not start". It seeds its own project,
 * ca-trial-4-readd-<minute>, in the projects folder, so the trial project is untouched and
 * the check runs in any order. */
export const checks = [{
  id: "N-D15",
  op: ["OP-55"],
  title: "Remove a project, then add the same folder again",
  why: "owner: no quote on disk; re-adding a project whose id was used before failed with \"could not start\" and logged nothing",
  after: "end",
  run: async (kit) => {
    const home = (await appCall("desktop_projects_home", {}))?.path;
    if (!home) throw new Error("the app has no projects folder to seed the re-add project in");
    const repo = seedRepo(readdFolder(home, stamp(new Date())), "# Trial 4 re-add\n\nA scratch project trial 4 N-D15 sets up, removes and adds again.\n");
    const name = basename(repo);
    const first = [];
    const again = [];

    await kit.go("#/home");
    const before = await setUp(kit, name, first, "first setup");

    await kit.go(`#/projects/${encodeURIComponent(before.id)}/settings`);
    await click("//nav[@aria-label=\"Project settings\"]//button[normalize-space(.)=\"Remove project\"]");
    await click(REMOVE_SECTION);
    await kit.until(async () => (await texts("dialog[open] h2")).some((title) => /^Remove .+ from CrazyAgent\?$/.test(title)), UI_MS, "the Remove project question did not open");
    await click("//dialog[@open]//button[normalize-space(.)=\"Remove project\"]");
    await kit.until(async () => (await kit.hash()) === "#/home", FOUND_MS, `removing ${name} did not go back Home`);

    const after = await setUp(kit, name, again, "after Remove");
    const problems = [];
    if (first.length > 0) problems.push(`the first setup of ${name} said "could not start": ${first.join(" | ")}`);
    if (again.length > 0) problems.push(`adding ${name} again said "could not start": ${again.join(" | ")}`);
    if (problems.length > 0) throw new Error(problems.join("; "));
    return `seeded ${repo}; set up as project ${before.id} (${before.words}); removed it; Home offered Set up this project again; setup of project ${after.id}${after.id === before.id ? " (the same id)" : ""} finished with ${after.words}; no "could not start" shown`;
  },
}];
