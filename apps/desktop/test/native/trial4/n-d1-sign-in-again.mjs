import assert from "node:assert/strict";
import { click, menu, shown } from "../settings-steps.mjs";
import { OWNER_MS } from "../trial4-checks.mjs";
import { SCRATCH, accounts, closeDialog, command, dropScratch, findAccount, scratchAccount, statusOf, tidy, windowText } from "../trial4-accounts.mjs";

const START = "//dialog[@open]//button[normalize-space(.)=\"Open Claude sign-in\" or normalize-space(.)=\"Try again\" or normalize-space(.)=\"Get a new code\"]";
const CANCEL = "//dialog[@open]//button[normalize-space(.)=\"Cancel sign-in\"]";
const CONFLICT = "native_profile_binding_conflict";
const CLEAR = "//button[normalize-space(.)=\"Clear stuck sign-in\"]";
const STUCK_LINE = "A sign-in didn't confirm how it ended. Changes to that account and new sign-ins wait until you clear it.";
const ASK_MS = 1000;

/** Opens Sign in again from the account's row and starts the Claude sign-in, then waits until
 * the drawer offers Cancel sign-in while the engine reads Signing in. Inputs: the kit and the account. */
async function start(kit, account) {
  await findAccount(kit, account.name);
  await menu(`More for ${account.name}`, "Sign in again");
  await click(START, kit.UI_MS);
  await kit.until(async () => await shown(CANCEL) && (await statusOf(account.id)) === "signing_in", kit.UI_MS,
    `OP-7: ${account.name} never offered Cancel sign-in while it waited`);
}

/** The next cleanup move for an account's stuck sign-in, from one read of the app's status and
 * one look at the window. Inputs: { stuck, id, banner }: the account the app reads stuck or
 * null, this account's id, and the banner's { up, busy }. Output: "done", "wait", "click" or "ask". */
export function clearStep({ stuck, id, banner }) {
  assert(typeof id === "string" && id.length > 0 && (stuck === null || typeof stuck === "string"));
  assert(typeof banner?.up === "boolean" && typeof banner.busy === "boolean");
  if (stuck !== id) return "done";
  if (banner.busy) return "wait";
  return banner.up ? "click" : "ask";
}

/** Looks once at the window for the stuck sign-in banner, so its line, Clear stuck sign-in and
 * Clearing... are read together and no swap between them can read as none. Output: { up, busy }. */
function bannerLook() {
  return browser.execute((line) => {
    const labels = [...document.querySelectorAll("button")].filter((node) => node.getClientRects().length)
      .map((node) => node.textContent.replace(/\s+/gu, " ").trim());
    const busy = labels.includes("Clearing...");
    return { up: busy || labels.includes("Clear stuck sign-in") || document.body.innerText.includes(line), busy };
  }, STUCK_LINE);
}

/** Clears the account's stuck sign-in before its scratch account is deleted: with the banner's
 * Clear stuck sign-in when the window shows it, as the owner is told to, else with the app's own
 * Clear command, since the kit's direct commands never raise the banner. Done once the app's
 * status, not the banner, reads no stuck sign-in for the account; another account's is left
 * alone. Inputs: the kit and the account. Output: true when it had one to clear. */
async function clearStuck(kit, account) {
  const stuck = async () => (await command("desktop_status", {})).stuck_sign_in;
  if ((await stuck()) !== account.id) return false;
  let asked = 0;
  await kit.until(async () => {
    const step = clearStep({ stuck: await stuck(), id: account.id, banner: await bannerLook() });
    if ((step === "click" || step === "ask") && Date.now() - asked >= ASK_MS) {
      asked = Date.now();
      await (step === "click" ? click(CLEAR, kit.UI_MS) : command("desktop_clear_native_auth", {}));
    }
    return step === "done";
  }, kit.UI_MS, `OP-7: the sign-in of ${account.name} still reads stuck after Clear stuck sign-in`);
  return true;
}

export const checks = [{
  id: "N-D1",
  op: ["OP-7"],
  title: "Sign in again after an interrupted sign-in",
  why: "owner: \"Sign-in didn't finish. Try again. (native_profile_binding_conflict) ... I can't even sign in.\"",
  after: "end",
  only: "asked",
  run: async (kit) => {
    assert(OWNER_MS > 0, "the plan leaves N-D1 out unless TRIAL_OWNER_MS is set");
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
      await clearStuck(kit, account);
      await dropScratch([account.id]);
      if ((await statusOf(account.id)) !== null) throw new Error(`OP-7: ${account.name} is still listed after its delete`);
    });
  },
}];
