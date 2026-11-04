import { click, texts } from "../settings-steps.mjs";
import { poolFitVerdict } from "../trial3-pool-fit.mjs";
import { UI_MS, lit } from "../trial-kit.mjs";

const CARD = "[role=\"radiogroup\"][aria-label=\"Pool\"] label.pj-radio-card.selected";
const MASTER = "#ps-master-harness";
const HELP = "section[aria-labelledby=\"ps-master\"] .pj-help";
const WARNINGS = "section[aria-labelledby=\"ps-master\"] .pj-note.warn";

/** The XPath of one section button of Project Settings. Input: its label. Output: the selector. */
function section(label) {
  return `//nav[@aria-label="Project settings"]//button[normalize-space(.)=${lit(label)}]`;
}

export const checks = [{
  id: "N-D28",
  op: ["OP-41"],
  title: "Settings, Team reads a pool of signed-in Claude accounts as fitting Claude Code",
  why: "owner: a pool of signed-in Claude accounts read as \"no account Claude Code can use\" (LB-12)",
  after: "add-project",
  run: async (kit) => {
    await kit.tab("settings");
    await click(section("Pool"));
    await kit.until(async () => (await texts(`${CARD} .pj-small`)).length > 0, UI_MS, "the project's pool card never showed its readiness");
    const pool = { name: (await texts(`${CARD} strong`))[0], summary: (await texts(`${CARD} .pj-secondary`))[0], readiness: (await texts(`${CARD} .pj-small`))[0] };
    await click(section("Team"));
    await kit.until(() => browser.execute((selector) => document.querySelector(selector)?.querySelector("option[value=\"claude-code\"]") != null, MASTER), UI_MS, "the Team editor never read the engine's harness catalog");
    await kit.pick(MASTER, "claude-code");
    await kit.until(async () => (await texts(HELP))[0]?.startsWith("Uses an account from ") ?? false, UI_MS, "the Team editor never read the project's pool");
    const master = await browser.execute((selector) => document.querySelector(selector).selectedOptions[0].textContent, MASTER);
    const warnings = await texts(WARNINGS);
    const help = (await texts(HELP))[0];
    await kit.inView(MASTER);
    await kit.screen();
    return `${poolFitVerdict({ pool, master, warnings, help })}; the Team draft is left unsaved`;
  },
}];
