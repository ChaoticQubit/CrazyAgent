import { click, menu, shown } from "../settings-steps.mjs";
import { SCRATCH, accounts, closeDialog, dropScratch, findAccount, scratchAccount, statusOf, tidy, windowText } from "../trial4-accounts.mjs";

const OWNER_MS = Number(process.env.TRIAL_OWNER_MS ?? 600000);
const START = "//dialog[@open]//button[normalize-space(.)=\"Open Claude sign-in\" or normalize-space(.)=\"Try again\" or normalize-space(.)=\"Get a new code\"]";
const CANCEL = "//dialog[@open]//button[normalize-space(.)=\"Cancel sign-in\"]";
const CONFLICT = "native_profile_binding_conflict";

/** Opens Sign in again from the account's row and starts the Claude sign-in, then waits until
 * the drawer offers Cancel sign-in while the engine reads Signing in. Inputs: the kit and the account. */
async function start(kit, account) {
  await findAccount(kit, account.name);
  await menu(`More for ${account.name}`, "Sign in again");
  await click(START, kit.UI_MS);
  await kit.until(async () => await shown(CANCEL) && (await statusOf(account.id)) === "signing_in", kit.UI_MS,
    `OP-7: ${account.name} never offered Cancel sign-in while it waited`);
}

export const checks = [{
  id: "N-D1",
  op: ["OP-7"],
  title: "Sign in again after an interrupted sign-in",
  why: "owner: \"Sign-in didn't finish. Try again. (native_profile_binding_conflict) ... I can't even sign in.\"",
  after: "end",
  run: async (kit) => {
    if (!(OWNER_MS > 0)) throw new Error("not run: TRIAL_OWNER_MS is 0, so no one is here to finish a browser sign-in");
    const account = await scratchAccount("signin");
    return tidy(async () => {
      await start(kit, account);
      await kit.screen();
      await click(CANCEL);
      await kit.until(async () => (await statusOf(account.id)) !== "signing_in", kit.UI_MS, `OP-7: Cancel sign-in left ${account.name} in Signing in`);
      await closeDialog(kit);
      await start(kit, account);
      const seen = await kit.waitFor(async () => {
        const status = await statusOf(account.id);
        const failed = await shown("dialog[open] [role=\"alert\"]");
        return { done: status === "signed_in" || failed, seen: failed ? (await windowText()).slice(0, 400) : status };
      }, OWNER_MS, `not proven: no one finished the Claude sign-in for ${account.name} in the browser, or it hung (OP-7)`);
      const text = await kit.screen();
      if (text.includes(CONFLICT)) throw new Error(`OP-7: Sign in again failed with ${CONFLICT}`);
      if ((await statusOf(account.id)) !== "signed_in") throw new Error(`OP-7: Sign in again did not finish: ${seen}`);
      const listed = await accounts();
      const same = listed.filter((item) => item.name === account.name);
      if (same.length !== 1) throw new Error(`OP-7: ${same.length} accounts are named ${account.name} after Sign in again`);
      const stuck = listed.filter((item) => item.name.startsWith(`${SCRATCH} `) && item.status === "signing_in").map((item) => item.name);
      if (stuck.length) throw new Error(`OP-7: ${stuck.join(", ")} stayed in Signing in`);
      return `${account.name} offered Cancel sign-in while it waited, cancelled cleanly, and after Sign in again on the same row reads Signed in, one row, none left in Signing in`;
    }, async () => {
      await closeDialog(kit);
      await dropScratch([account.id]);
    });
  },
}];
