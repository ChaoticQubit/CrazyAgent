import { basename } from "node:path";
import { click, shown, text, texts, toasts, type } from "../settings-steps.mjs";
import { couldNotStart } from "../trial4-projects.mjs";

const UI_MS = 30000;
const FOUND_MS = 60000;
const SETUP_MS = Number(process.env.TRIAL_SETUP_MS ?? 1800000);
const POOL = process.env.TRIAL_POOL ?? "";
const STEPS = "ol[aria-label=\"Setup steps\"] > li";
const CONFIRM = "//button[normalize-space(.)=\"Confirm and finish setup\"]";
const REMOVE_SECTION = "//div[h2[normalize-space(.)=\"Remove project\"]]//button[normalize-space(.)=\"Remove project\"]";

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

/** N-D15 (B-23, OP-55): the trial project, removed from the app and added again from the same
 * folder, sets up with both steps done and never says "could not start". Runs last: it
 * removes the trial project. */
export const checks = [{
  id: "N-D15",
  op: ["OP-55"],
  title: "Remove the trial project, then add the same folder again",
  why: "owner: no quote on disk; re-adding a project whose id was used before failed with \"could not start\" and logged nothing",
  after: "end",
  run: async (kit) => {
    const folder = basename(process.env.TRIAL_PROJECT);
    const old = kit.project();
    const sightings = [];

    await kit.tab("settings");
    await click("//nav[@aria-label=\"Project settings\"]//button[normalize-space(.)=\"Remove project\"]");
    await click(REMOVE_SECTION);
    await kit.until(async () => (await texts("dialog[open] h2")).some((title) => /^Remove .+ from CrazyAgent\?$/.test(title)), UI_MS, "the Remove project question did not open");
    await click("//dialog[@open]//button[normalize-space(.)=\"Remove project\"]");
    await kit.until(async () => (await kit.hash()) === "#/home", FOUND_MS, "removing the project did not go back Home");
    const row = `//li[contains(concat(" ", normalize-space(@class), " "), " in-row ")][.//strong[normalize-space(.)="${folder}"]]`;
    await kit.until(() => shown(row), FOUND_MS, `Home never listed ${folder} after it was removed`);
    const action = (await $(`${row}//button`).getText()).trim();
    await look(sightings);
    await kit.screen();
    if (action !== "Set up this project") throw new Error(`after Remove, Home offers ${action} for ${folder}: the app did not forget the project`);

    await click(`${row}//button`);
    await kit.until(async () => (await texts("dialog[open] h2")).includes(`Set up ${folder}`), UI_MS, "the Set up dialog did not open");
    if (POOL) await click(`//dialog[@open]//label[.//*[normalize-space(.)="${POOL}"]]`);
    await click("//dialog[@open]//button[normalize-space(.)=\"Start setup\"]", 60000);
    let route = "";
    await kit.until(async () => {
      await look(sightings);
      return /^#\/projects\/[^/]+\/setup$/.test(route = await kit.hash());
    }, FOUND_MS * 2, `setup of ${folder} did not start`);
    const id = decodeURIComponent(route.split("/")[2]);

    let failed = null;
    const words = await kit.waitFor(async () => {
      await look(sightings);
      const now = await steps();
      failed = now.find((step) => step.word === "Failed") ?? null;
      const done = failed !== null || now.length === 2 && now.every((step) => step.word === "Done") && await shown(CONFIRM) && await $(CONFIRM).isEnabled();
      return { done, seen: now.map((step) => `${step.title}: ${step.word}`).join(", ") || "no setup steps shown" };
    }, SETUP_MS, `setup of ${folder} did not finish both steps`);
    await kit.screen();
    if (failed) throw new Error(`setup of ${folder} after it was removed: ${failed.title} failed (${words})`);
    if (await shown("#ob-name") && (await $("#ob-name").getValue()).trim() === "") await type("#ob-name", folder);
    await click(CONFIRM);
    await kit.until(async () => {
      await look(sightings);
      return /^#\/projects\/[^/]+\/overview$/.test(await kit.hash());
    }, UI_MS * 2, `the set-up project ${folder} did not open`);
    await look(sightings);
    await kit.screen();
    if (kit.run) kit.run.project = id;
    if (sightings.length > 0) throw new Error(`the window said "could not start": ${sightings.join(" | ")}`);
    return `removed project ${old}; Home offered Set up this project for ${folder}; setup of project ${id}${id === old ? " (the same id)" : ""} finished with ${words}; no "could not start" shown`;
  },
}];
