import assert from "node:assert/strict";
import { invoke, newProject, openMasterSettings, prepareWindow } from "./master-settings-window.mjs";
import { click } from "./settings-steps.mjs";
import { reopen, run, until } from "./trial-kit.mjs";

const UI_MS = 30000;
const OPTIONS = [["Off", false], ["On", true]];

/** The selector of the setting's switch for the kit's project. Output: the selector. */
const field = () => `[id="tg-live-terminal-${run.project}"]`;

/** Reads what the drawer's switch shows. Output: "true", "false", or null with no switch. */
function checked() {
  return browser.execute((selector) => document.querySelector(selector)?.getAttribute("aria-checked") ?? null, field());
}

/** Waits until the engine holds the setting. Input: the value. Output: none. */
async function kept(on) {
  await until(async () => (await invoke("desktop_team_status", { id: run.project })).master_live_terminal === on, UI_MS, `the engine never kept ${on}`);
}

/** Sets the switch as the owner does, in the drawer that stays open when the area changes
 * under it. Input: the value. Output: none. */
async function set(on) {
  if ((await checked()) !== String(on)) await click(field());
}

describe("Live terminal, in the real window", () => {
  before(async () => {
    await prepareWindow();
    await newProject("live-terminal-shop");
  });

  for (const [name, on] of OPTIONS) {
    it(`${name}: the owner's choice reaches the engine and shows again after leaving the tab`, async () => {
      await openMasterSettings(field());
      await set(!on);
      await kept(!on);
      await set(on);
      await kept(on);
      await reopen("conversation");
      await openMasterSettings(field());
      assert.equal(await checked(), String(on));
    });
  }
});
