import { click, shown } from "../settings-steps.mjs";
import { closeDialog, command, dropScratch, scratchAccount, signInRowsVerdict, statusOf, tidy } from "../trial4-accounts.mjs";

const SIGN_INS = { filter: "pending", project_id: null, kind: "account_sign_in", fold: false, before: null, after: null, limit: 50 };
const PANEL = "div.notice-panel[role=\"dialog\"]";

/** Reads the engine's pending account sign-ins, unfolded so each account is its own item,
 * as a project's Needs you view reads them. Output: the items. */
async function signIns() {
  const page = await command("desktop_notifications", { query: SIGN_INS });
  return page.items;
}

/** Opens the bell's panel on its Pending filter when it is closed. */
async function openPending() {
  if (!(await shown(PANEL))) await click("button.notice-bell");
  await click(`${PANEL} .notice-filters label:has(input[value="pending"])`);
}

/** Reads every Account sign-in row the bell panel shows, where every account's sign-in should
 * fold into one row. Output: each row's text, empty when the panel shows none. */
function signInRows() {
  return browser.execute((panel) => [...document.querySelectorAll(`${panel} li.notice-row`)]
    .filter((node) => node.querySelector(".notice-kind")?.innerText.trim() === "Account sign-in")
    .map((node) => node.innerText.replace(/\s+/gu, " ").trim()), PANEL);
}

export const checks = [{
  id: "N-D19",
  op: ["OP-46"],
  title: "The bell holds one sign-in for a signed-out account",
  why: "owner: Home shows the same \"ChaoticQubit isn't signed in\" banner twice (leader record); Home's Needs you strip moved to the bell (DEC-136a)",
  after: "end",
  only: "fixture",
  run: async (kit) => {
    const account = await scratchAccount("home");
    return tidy(async () => {
      await kit.until(async () => (await statusOf(account.id)) === "not_signed_in", kit.UI_MS, `${account.name} never read Not signed in`);
      await kit.go("#/home");
      await kit.until(async () => (await signIns()).some((item) => item.target?.account === account.id), kit.UI_MS,
        `the engine never held a sign-in for ${account.name}`);
      await openPending();
      await kit.until(async () => (await signInRows()).length > 0, kit.UI_MS, "the bell never showed an Account sign-in row");
      await kit.screen();
      return signInRowsVerdict({ name: account.name, account: account.id, items: await signIns(), rows: await signInRows() });
    }, async () => {
      if (await shown(PANEL)) await browser.keys("Escape");
      await closeDialog(kit);
      await dropScratch([account.id]);
    });
  },
}];
