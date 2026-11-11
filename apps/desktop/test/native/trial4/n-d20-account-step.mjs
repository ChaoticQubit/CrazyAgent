import { shown } from "../settings-steps.mjs";
import { accountStepVerdict, accounts, readyCount } from "../trial4-accounts.mjs";

const SETTLE_MS = 10000;

/** Reads Home's Get set up Add an account step. Output: { done, body }, or null when the panel
 * has no such step. */
function accountItem() {
  return browser.execute(() => {
    const item = [...document.querySelectorAll(".setup-panel li")].find((node) => node.querySelector("strong")?.textContent.trim() === "Add an account");
    return item ? { done: Boolean(item.querySelector(".done")), body: item.querySelector("p")?.textContent.trim() ?? "" } : null;
  });
}

export const checks = [{
  id: "N-D20",
  op: ["OP-47"],
  title: "Get set up reads Add an account done only with an account ready for agents",
  why: "owner: \"Add an account: Done\" while no account is signed in (leader record)",
  after: "end",
  run: async (kit) => {
    const list = await accounts();
    const ready = readyCount(list);
    await kit.go("#/settings/accounts");
    await kit.go("#/home");
    await kit.until(() => shown("main .project-card"), kit.UI_MS, "Home never listed its projects");
    const panel = await kit.until(() => shown(".setup-panel"), SETTLE_MS, "Get set up stayed hidden").then(() => true, () => false);
    if (!panel) {
      await kit.screen();
      return accountStepVerdict({ panel, done: null, body: "", ready, total: list.length });
    }
    const item = await accountItem();
    if (!item) throw new Error("Get set up shows no Add an account step");
    await kit.inView(".setup-panel");
    await kit.screen();
    return accountStepVerdict({ panel, done: item.done, body: item.body, ready, total: list.length });
  },
}];
