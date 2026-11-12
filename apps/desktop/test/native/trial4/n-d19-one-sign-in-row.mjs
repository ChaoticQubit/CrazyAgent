import { click, shown } from "../settings-steps.mjs";
import { closeDialog, dropScratch, scratchAccount, signInRowsVerdict, statusOf, tidy } from "../trial4-accounts.mjs";

/** Reads every Needs you row on Home, opening Show all first when the strip hides some.
 * Output: each row's text. */
async function needRows() {
  if (await shown("section.needs button[aria-expanded=\"false\"]")) await click("section.needs button[aria-expanded=\"false\"]");
  return browser.execute(() => [...document.querySelectorAll("section.needs .need")].filter((node) => node.getClientRects().length)
    .map((node) => node.innerText.replace(/\s+/gu, " ").trim()));
}

export const checks = [{
  id: "N-D19",
  op: ["OP-46"],
  title: "Home shows one sign-in row for a signed-out account",
  why: "owner: Home shows the same \"ChaoticQubit isn't signed in\" banner twice (leader record)",
  after: "end",
  only: "fixture",
  run: async (kit) => {
    const account = await scratchAccount("home");
    return tidy(async () => {
      await kit.until(async () => (await statusOf(account.id)) === "not_signed_in", kit.UI_MS, `${account.name} never read Not signed in`);
      await kit.go("#/settings/accounts");
      await kit.go("#/home");
      let rows = [];
      await kit.until(async () => (rows = await needRows()).some((row) => row.includes(account.name)), kit.UI_MS, `Home never showed a sign-in row for ${account.name}`);
      await kit.inView("section.needs");
      await kit.screen();
      return signInRowsVerdict({ name: account.name, rows: await needRows() });
    }, async () => {
      await closeDialog(kit);
      await dropScratch([account.id]);
    });
  },
}];
