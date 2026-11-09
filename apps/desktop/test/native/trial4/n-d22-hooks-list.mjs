import { click, shown, texts } from "../settings-steps.mjs";
import { UI_MS, segment } from "../trial-kit.mjs";
import { hooksVerdict } from "../trial4-context.mjs";

const FAILED = "//section[@role=\"alert\"][.//p[normalize-space(.)=\"Couldn't load hooks.\"]]";
const RETRY = `${FAILED}//button[normalize-space(.)="Try again"]`;
const EMPTY = "//*[normalize-space(.)=\"No hooks yet\"]";
const PLUGIN = "main .set-row .set-label";

/** Whether the Hooks list shows hooks or says it has none. Output: a boolean. */
async function listed() {
  return await shown(EMPTY) || (await texts(PLUGIN)).length > 0;
}

export const checks = [{
  id: "N-D22",
  op: ["OP-13", "OP-23"],
  title: "The Hooks list shows the hooks of the owner's plugins",
  why: "owner: \"Where are my by default hooks for caveman, adhd\"; \"clicking on try again does nothing\"",
  after: "process-instructions",
  run: async (kit) => {
    await kit.go("#/settings/plugins");
    await click(segment("Plugins and skills", "Hooks"));
    await kit.until(async () => await shown(FAILED) || await listed(), UI_MS, "the Hooks list never loaded, failed or said it is empty");
    const failedFirst = await shown(FAILED);
    let failedAfterRetry = false;
    if (failedFirst) {
      await click(RETRY);
      try {
        await kit.until(async () => !(await shown(FAILED)) && await listed(), UI_MS, "Try again did not load the Hooks list");
      } catch {
        failedAfterRetry = true;
      }
    }
    await kit.screen();
    return hooksVerdict({ plugins: await texts(PLUGIN), failedFirst, failedAfterRetry });
  },
}];
