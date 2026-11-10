import { click, menu, selectPool, shown, texts, type } from "../settings-steps.mjs";
import { closeDialog, confirmVerdict, dropScratch, findAccount, noLoop, rowNames, scratchAccount, scratchName, tidy, windowText } from "../trial4-accounts.mjs";

const PICKER = "//div[@role=\"radiogroup\"][@aria-label=\"Pool\"]";
const NO_POOL = "No pool";
const CONFIRM = "dialog.dialog[open]";

/** The pool names Settings, Pools lists now. Output: the names. */
function poolNames() {
  return texts(".pool-item-label");
}

/** Opens the trial project's Settings, Pool section and waits for its pool cards. Input: the kit. */
async function poolSection(kit) {
  await kit.tab("settings");
  await click(`//nav[@aria-label="Project settings"]//button[normalize-space(.)="Pool"]`);
  await kit.until(() => shown(`${PICKER}//label`), kit.UI_MS, "the project's Pool section never showed its pool cards");
}

/** The pool card the project's Pool section has selected. Output: its name, "No pool" for none. */
async function selectedPool() {
  return (await texts("[role=\"radiogroup\"][aria-label=\"Pool\"] label.pj-radio-card.selected strong"))[0] ?? null;
}

/** Gives the trial project one pool, or none, as the owner does in its Settings, Pool section,
 * and waits for the line that says so. Inputs: the kit and the card name. Output: the window text. */
async function assignPool(kit, name) {
  await poolSection(kit);
  if ((await selectedPool()) === name) return windowText();
  await click(`${PICKER}//label[.//strong[normalize-space(.)=${kit.lit(name)}]]`);
  await click("//button[normalize-space(.)=\"Save pool\"]");
  const line = name === NO_POOL ? "This project has no pool now." : `${name} is assigned.`;
  await kit.until(async () => (await windowText()).includes(line), kit.UI_MS, `OP-56: the project never said "${line}"`);
  return windowText();
}

/** Makes a scratch pool holding one account through Settings, Pools, New pool. Inputs: the
 * kit, the pool name and the account name. */
async function newPool(kit, name, member) {
  await kit.go("#/settings/pools");
  await click("button=New pool");
  await type("#pool-name", name);
  await type("main input[type=\"search\"][placeholder=\"Search accounts\"]", member);
  await click(`//label[contains(concat(" ", normalize-space(@class), " "), " pick ")][.//span[normalize-space(.)=${kit.lit(member)}]]`);
  await click("button=Save pool");
  await kit.until(async () => (await poolNames()).includes(name), kit.UI_MS, `Settings, Pools never listed the new pool ${name}`);
}

/** Reads the open confirm and judges that it names what changes. Inputs: what it must name.
 * Output: the verdict line. */
async function confirmNames(wants) {
  return confirmVerdict({ title: (await texts(`${CONFIRM} h2`))[0] ?? "", body: (await texts(`${CONFIRM} .dialog-body`))[0] ?? "", wants });
}

export const checks = [{
  id: "N-D32",
  op: ["OP-56"],
  title: "Unassign a pool, delete it, then delete its account",
  why: "owner: \"I cannot unassign a pool from a project ... it's a dead loop\"",
  after: "end",
  run: async (kit) => {
    const account = await scratchAccount("pool");
    const pool = scratchName("pool", Date.now());
    let original = null;
    return tidy(async () => {
      await poolSection(kit);
      original = await selectedPool();
      if (!original) throw new Error("the project's Pool section shows no selected card");
      await newPool(kit, pool, account.name);
      await assignPool(kit, pool);
      noLoop(await assignPool(kit, NO_POOL));
      await kit.screen();
      await kit.go("#/settings/pools");
      await selectPool(pool);
      await click(`button[aria-label=${JSON.stringify(`Delete pool ${pool}`)}]`);
      await kit.until(() => shown(CONFIRM), kit.UI_MS, `Delete pool on ${pool} never asked to confirm`);
      const poolConfirm = await confirmNames([pool, "Only the pool goes away"]);
      await click("//dialog[@open]//button[normalize-space(.)=\"Delete pool\"]");
      await kit.until(async () => !(await poolNames()).includes(pool), kit.UI_MS, `OP-56: ${pool} is still listed after Delete pool`);
      noLoop(await windowText());
      await kit.screen();
      await findAccount(kit, account.name);
      await menu(`More for ${account.name}`, "Delete");
      await kit.until(() => shown(CONFIRM), kit.UI_MS, `Delete on ${account.name} never asked to confirm`);
      const accountConfirm = await confirmNames([account.name, "removed from CrazyAgent"]);
      await click("//dialog[@open]//button[normalize-space(.)=\"Delete account\"]");
      await kit.until(async () => !(await rowNames()).includes(account.name), kit.UI_MS, `OP-56: ${account.name} is still listed after Delete account`);
      noLoop(await windowText());
      await kit.screen();
      return `the project took ${pool}, then No pool; deleted the pool after ${poolConfirm}; deleted ${account.name} after ${accountConfirm}`;
    }, async () => {
      await closeDialog(kit);
      if (original && original !== pool) await assignPool(kit, original);
      await dropScratch([account.id]);
    });
  },
}];
