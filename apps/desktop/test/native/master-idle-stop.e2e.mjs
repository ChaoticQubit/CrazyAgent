import assert from "node:assert/strict";
import { invoke, newProject, openMasterSettings, prepareWindow } from "./master-settings-window.mjs";
import { pick, reopen, run, until } from "./trial-kit.mjs";

const UI_MS = 30000;
const OPTIONS = [["off", "Off", "one_hour"], ["fifteen_minutes", "15 min", "one_hour"], ["one_hour", "1 h", "off"]];

/** The selector of the setting's select for the kit's project. Output: the selector. */
const field = () => `[id="tg-master-idle-${run.project}"]`;

/** Waits until the engine holds a choice. Input: the choice. Output: none. */
async function kept(choice) {
  await until(async () => (await invoke("desktop_team_status", { id: run.project })).master_idle_stop === choice, UI_MS, `the engine never kept ${choice}`);
}

/** Reads the choice the drawer shows. Output: its value and label, or null. */
function drawerChoice() {
  return browser.execute((selector) => {
    const select = document.querySelector(selector);
    return select ? { value: select.value, label: select.selectedOptions[0]?.textContent ?? null } : null;
  }, field());
}

describe("Stop the master when idle after, in the real window", () => {
  before(async () => {
    await prepareWindow();
    await newProject("idle-stop-shop");
  });

  for (const [choice, label, first] of OPTIONS) {
    it(`${label}: the owner's pick reaches the engine and shows again after leaving the tab`, async () => {
      await openMasterSettings(field());
      await pick(field(), first);
      await kept(first);
      await pick(field(), choice);
      await kept(choice);
      await reopen("conversation");
      await openMasterSettings(field());
      assert.deepEqual(await drawerChoice(), { value: choice, label });
    });
  }
});
