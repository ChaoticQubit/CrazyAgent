import assert from "node:assert/strict";
import { invoke, newProject, openMasterSettings, prepareWindow } from "./master-settings-window.mjs";
import { pick, reopen, run, until } from "./trial-kit.mjs";

const UI_MS = 30000;
const OPTIONS = [["off", null, "Off", "600"], ["120", 120, "2 min", "600"], ["600", 600, "10 min", "off"], ["3600", 3600, "1 h", "600"]];

/** The selector of the setting's select for the kit's project. Output: the selector. */
const field = () => `[id="tg-done-quiet-${run.project}"]`;

/** Waits until the engine holds a choice. Input: the seconds, null for Off. Output: none. */
async function kept(secs) {
  await until(async () => (await invoke("desktop_team_status", { id: run.project })).done_quiet_secs === secs, UI_MS, `the engine never kept ${secs}`);
}

/** Reads the choice the drawer shows. Output: its value and label, or null. */
function drawerChoice() {
  return browser.execute((selector) => {
    const select = document.querySelector(selector);
    return select ? { value: select.value, label: select.selectedOptions[0]?.textContent ?? null } : null;
  }, field());
}

describe("End a turn after done, in the real window", () => {
  before(async () => {
    await prepareWindow();
    await newProject("done-quiet-shop");
  });

  for (const [value, secs, label, first] of OPTIONS) {
    it(`${label}: the owner's pick reaches the engine and shows again after leaving the tab`, async () => {
      await openMasterSettings(field());
      await pick(field(), first);
      await kept(first === "off" ? null : Number(first));
      await pick(field(), value);
      await kept(secs);
      await reopen("conversation");
      await openMasterSettings(field());
      assert.deepEqual(await drawerChoice(), { value, label });
    });
  }
});
