import { click, pick } from "../settings-steps.mjs";
import { closeDialog, ownTexts, providerVerdict } from "../trial4-accounts.mjs";

const TILES = "dialog.dialog[open] .tiles button.tile .title";
const METHODS = "dialog.dialog[open] .methods button.method .title";

/** Opens one provider tile of Add account, reads its ways to add an account, takes the proof
 * screenshot and goes back to the tiles. Inputs: the kit and the tile name. Output: the titles. */
async function ways(kit, company) {
  await pick("tile", company);
  await kit.until(async () => (await ownTexts(METHODS)).length > 0, kit.UI_MS, `${company} never showed its ways to add an account`);
  const titles = await ownTexts(METHODS);
  await kit.screen();
  await click("dialog.dialog[open] button[aria-label=\"Back\"]");
  await kit.until(async () => (await ownTexts(TILES)).length > 0, kit.UI_MS, `Back from ${company} never showed the provider tiles again`);
  return titles;
}

export const checks = [{
  id: "N-D18",
  op: ["OP-45", "OP-58"],
  title: "Add account lists each provider once",
  why: "owner: \"Why do we have so much redundancy in the accounts creation part?\"",
  after: "end",
  run: async (kit) => {
    await kit.go("#/settings/accounts");
    await click("button=Add account");
    await kit.until(async () => (await ownTexts(TILES)).length > 0, kit.UI_MS, "Add account never showed its provider tiles");
    try {
      const tiles = await ownTexts(TILES);
      await kit.screen();
      const methods = {};
      for (const company of ["OpenAI", "Anthropic"].filter((name) => tiles.includes(name))) methods[company] = await ways(kit, company);
      return providerVerdict({ tiles, methods });
    } finally {
      await closeDialog(kit);
    }
  },
}];
