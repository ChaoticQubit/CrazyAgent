import { click, menu, texts, type } from "../settings-steps.mjs";
import { SEARCH, closeDialog, confirmVerdict, dropScratch, findAccount, listTotal, rowNames, scratchAccount, scratchName, tidy } from "../trial4-accounts.mjs";

const DRAWER = "dialog.drawer[open]";
const CONFIRM = "dialog.dialog[open]";

export const checks = [{
  id: "N-D2",
  op: ["OP-44"],
  title: "Rename and delete an account that is not signed in",
  why: "owner: \"I cannot even delete those, I cannot cancel those processes, nothing like that\"",
  after: "end",
  only: "fixture",
  run: async (kit) => {
    const account = await scratchAccount("rename");
    return tidy(async () => {
      const before = listTotal(await findAccount(kit, account.name));
      await menu(`More for ${account.name}`, "Details and rename");
      const renamed = scratchName("renamed", Date.now());
      await type("#drawer-account-name", renamed);
      await click(`//dialog[@open]//button[@type="submit"][normalize-space(.)="Rename"]`);
      await kit.until(async () => (await texts(`${DRAWER} h2`))[0] === renamed, kit.UI_MS, `OP-44: the account drawer never read the new name ${renamed}`);
      await closeDialog(kit);
      await type(SEARCH, renamed);
      await kit.until(async () => (await rowNames()).includes(renamed), kit.UI_MS, `OP-44: Settings, Accounts never listed the new name ${renamed}`);
      await kit.screen();
      await menu(`More for ${renamed}`, "Delete");
      await kit.until(async () => (await texts(`${CONFIRM} h2`)).length > 0, kit.UI_MS, `Delete on ${renamed} never asked to confirm`);
      const confirm = confirmVerdict({ title: (await texts(`${CONFIRM} h2`))[0], body: (await texts(`${CONFIRM} .dialog-body`))[0] ?? "", wants: [renamed, "removed from CrazyAgent"] });
      await click(`//dialog[@open]//button[normalize-space(.)="Delete account"]`);
      await kit.until(async () => !(await rowNames()).includes(renamed), kit.UI_MS, `OP-44: ${renamed} is still listed after Delete account`);
      const after = listTotal((await texts("main .set-bar .count"))[0]);
      await kit.screen();
      if (after !== before - 1) throw new Error(`OP-44: the account count went from ${before} to ${after} after Delete account`);
      return `renamed ${account.name} to ${renamed} and the row showed it; deleted after ${confirm}; the count went from ${before} to ${after}`;
    }, async () => {
      await closeDialog(kit);
      await dropScratch([account.id]);
    });
  },
}];
