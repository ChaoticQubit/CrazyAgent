import assert from "node:assert/strict";
import { click, shown, type } from "./settings-steps.mjs";

/** The mark every scratch account and pool the trial 4 account checks make starts with. */
export const SCRATCH = "ca-trial-4";
const NAME_MAX = 60;
const ACCOUNT_PAGE = 200;
const ACCOUNT_PAGES_MAX = 100;
const READY = new Set(["signed_in", "key_saved"]);
/** The search field of Settings, Accounts. */
export const SEARCH = "main input[type=\"search\"][placeholder=\"Search accounts\"]";

/** Names one scratch account or pool. Inputs: a short kind ("rename", "pool") and the time in
 * ms. Output: "ca-trial-4 <kind> <time in base 36>", at most 60 characters. */
export function scratchName(kind, now) {
  assert(/^[a-z0-9-]{1,20}$/u.test(kind), "a scratch kind is 1 to 20 lowercase letters, digits or dashes");
  assert(Number.isSafeInteger(now) && now >= 0, "the time is a whole number of ms");
  const name = `${SCRATCH} ${kind} ${now.toString(36)}`;
  assert(name.length <= NAME_MAX);
  return name;
}

/** The names that appear more than once, compared without case or spaces at the ends.
 * Input: the names in screen order. Output: each repeated name once, as first shown. */
export function repeated(names) {
  assert(Array.isArray(names));
  const seen = new Map();
  for (const name of names) {
    const key = String(name).trim().toLowerCase();
    seen.set(key, { name: seen.get(key)?.name ?? String(name).trim(), count: (seen.get(key)?.count ?? 0) + 1 });
  }
  return [...seen.values()].filter((entry) => entry.count > 1).map((entry) => entry.name);
}

/** Judges Settings, Accounts, Add account (N-D18; OP-45, OP-58): every provider tile once,
 * one OpenAI and one Anthropic tile with no separate API tile beside them, and every way to
 * add an account once inside each. Input: { tiles, methods }, the tile names and each opened
 * tile's method titles. Output: the detail; throws on a repeat. */
export function providerVerdict({ tiles, methods }) {
  assert(Array.isArray(tiles) && methods && typeof methods === "object");
  const twice = repeated(tiles);
  if (twice.length) throw new Error(`OP-45: Add account lists ${twice.join(" and ")} more than once`);
  for (const company of ["OpenAI", "Anthropic"]) {
    if (!tiles.includes(company)) throw new Error(`Add account shows no ${company} tile`);
    const beside = tiles.find((tile) => tile !== company && tile.toLowerCase().startsWith(company.toLowerCase()) && /\bapi\b/iu.test(tile));
    if (beside) throw new Error(`OP-58: Add account lists ${beside} beside ${company}`);
  }
  for (const [tile, titles] of Object.entries(methods)) {
    const again = repeated(titles);
    if (again.length) throw new Error(`OP-45: ${tile} lists ${again.join(" and ")} more than once`);
  }
  const ways = Object.entries(methods).map(([tile, titles]) => `${tile} (${titles.length} ${titles.length === 1 ? "way" : "ways"})`);
  return `${tiles.length} providers, each once: ${ways.join(", ")}; tiles ${tiles.join(", ")}`;
}

/** Judges Home's Needs you rows for one signed-out account (N-D19; OP-46): exactly one row
 * names it. Input: { name, rows }, the account name and every row's text. Output: the detail. */
export function signInRowsVerdict({ name, rows }) {
  assert(typeof name === "string" && name.length > 0 && Array.isArray(rows));
  const naming = rows.filter((row) => row.includes(name));
  if (naming.length === 0) throw new Error(`Home shows no sign-in row for ${name}`);
  if (naming.length > 1) throw new Error(`OP-46: Home shows ${naming.length} rows for ${name}`);
  return `one Needs you row for ${name}: "${naming[0]}"`;
}

/** Judges Home's Get set up Add an account step (N-D20; OP-47) against the accounts read:
 * done exactly when one is ready for agents (signed in or key saved), and the panel hidden
 * only when that step is done. Input: { panel, done, body, ready, total }. Output: the detail. */
export function accountStepVerdict({ panel, done, body, ready, total }) {
  assert(typeof panel === "boolean" && Number.isInteger(ready) && Number.isInteger(total) && ready >= 0 && ready <= total);
  const counted = `${ready} of ${total} accounts ready for agents`;
  if (!panel) {
    if (ready === 0) throw new Error(`OP-47: Get set up is hidden as done with ${counted}`);
    return `Get set up is hidden, every step done, with ${counted}`;
  }
  assert(typeof done === "boolean");
  if (done && ready === 0) throw new Error(`OP-47: Add an account reads Done with ${counted}`);
  if (!done && ready > 0) throw new Error(`Add an account reads not done with ${counted}`);
  return `Add an account reads ${done ? "Done" : "not done"} with ${counted}: "${body}"`;
}

/** Judges Settings, Pack for Graphify (N-D4; OP-49): one Graphify tool row and one Graphify
 * pack entry, named apart; the tool reads Found or Installed, and the entry, then installed,
 * has its switch. Input: rows { section, title, words, on } (on is the switch, or null).
 * Output: { tool, entry }; throws "not run: ..." when the command is not on this Mac. */
export function graphifyVerdict(rows) {
  assert(Array.isArray(rows));
  const graphify = rows.filter((row) => /graphify/iu.test(row.title));
  const tools = graphify.filter((row) => row.section === "Tools");
  const entries = graphify.filter((row) => row.section !== "Tools");
  if (tools.length !== 1) throw new Error(`OP-49: Graphify shows ${tools.length} times among the tools`);
  if (entries.length !== 1) throw new Error(`OP-49: Graphify shows ${entries.length} times in the pack`);
  const [tool] = tools;
  const [entry] = entries;
  if (tool.title === entry.title) throw new Error(`OP-49: the Graphify tool and pack entry share the name ${tool.title}`);
  if (!tool.words.some((word) => word === "Found" || word === "Installed")) {
    throw new Error(`not run: the Graphify command is not installed on this Mac (${tool.words.join(", ")})`);
  }
  if (entry.on === null) throw new Error(`OP-49: the Graphify pack entry reads ${entry.words.join(", ")} with no switch while the Graphify command reads ${tool.words.join(", ")}`);
  return { tool, entry };
}

/** Judges the app as the Dock opens it (N-D5; OP-43): Home lists projects and no line says
 * the engine is missing. Input: { text, projects }, the window text and the project rows.
 * Output: the detail. */
export function engineVerdict({ text, projects }) {
  assert(typeof text === "string" && Number.isInteger(projects));
  const missing = ["engine is not running", "cannot connect to engine", "can't connect to the engine", "couldn't connect to the engine"]
    .find((words) => text.toLowerCase().includes(words));
  if (missing) throw new Error(`OP-43: the app says "${missing}"`);
  if (projects === 0) throw new Error("Home lists no project");
  return `Home lists ${projects} projects with no engine error`;
}

/** Judges one delete or unassign confirm: its title and body name each thing that changes.
 * Input: { title, body, wants }. Output: the title and body as one line. */
export function confirmVerdict({ title, body, wants }) {
  assert(typeof title === "string" && typeof body === "string" && Array.isArray(wants) && wants.length > 0);
  for (const want of wants) {
    if (!`${title}\n${body}`.includes(want)) throw new Error(`the confirm "${title}" does not name ${want}`);
  }
  return `"${title}": ${body}`;
}

/** Fails when the window loops back to the old refusal (N-D32; OP-56). Input: the window
 * text. Output: the same text. */
export function noLoop(text) {
  assert(typeof text === "string");
  if (/assigned to a project/iu.test(text)) throw new Error("OP-56: the app still says \"assigned to a project\"");
  return text;
}

/** Calls one of the app's own commands from the window, as the app's screens do. Inputs: the
 * command and its arguments. Output: its value; throws with the app's failure. */
export async function command(name, args) {
  assert(/^desktop_[a-z_]+$/u.test(name), "only the app's own desktop_ commands");
  const answer = await browser.executeAsync((name, args, done) => {
    window.__TAURI_INTERNALS__.invoke(name, args).then((value) => done({ value }), (failure) => done({ failed: typeof failure === "string" ? failure : JSON.stringify(failure) }));
  }, name, args);
  if (answer.failed !== undefined) throw new Error(`${name} failed: ${answer.failed}`);
  return answer.value;
}

/** Every account the engine lists, page by page as the Accounts page reads them. Output: the
 * accounts. */
export async function accounts() {
  const all = [];
  let after = null;
  for (let page = 0; page < ACCOUNT_PAGES_MAX; page += 1) {
    const read = await command("desktop_list_accounts", { after, limit: ACCOUNT_PAGE });
    all.push(...read.accounts);
    if (!read.next) return all;
    after = read.next;
  }
  throw new Error("the engine lists more accounts than the trial reads");
}

/** Counts the accounts ready for agents, signed in or with a key saved, as Get set up does.
 * Input: the accounts. Output: the count. */
export function readyCount(list) {
  return list.filter((account) => READY.has(account.status)).length;
}

/** Adds a scratch Claude subscription account that is not signed in, through the app's own
 * Add account command, without starting a sign-in. Input: the kind for its name. Output: the
 * account as the engine returned it. */
export async function scratchAccount(kind) {
  const name = scratchName(kind, Date.now());
  const account = await command("desktop_add_account", { draft: { provider: "claude", method: "subscription", name }, key: null });
  assert.equal(account.name, name, "the engine kept the scratch name");
  assert.notEqual(account.status, "signed_in", "a new scratch subscription is not signed in");
  return account;
}

/** Removes scratch accounts the checks left behind, by id, and only ones that carry the trial 4
 * mark. Input: the ids. Output: the names removed. */
export async function dropScratch(ids) {
  const left = (await accounts()).filter((account) => ids.includes(account.id));
  for (const account of left) assert(account.name.startsWith(`${SCRATCH} `), "only scratch accounts are removed");
  for (const account of left) await command("desktop_remove_account", { id: account.id });
  return left.map((account) => account.name);
}

/** Runs one check's work, then its cleanup of scratch data, whichever way the work ends.
 * Inputs: the work and the cleanup, both async. Output: the work's detail; throws the work's
 * failure, with a cleanup failure added to it, or the cleanup failure after a pass. */
export async function tidy(work, cleanup) {
  let failure = null;
  let detail;
  try {
    detail = await work();
  } catch (error) {
    failure = error;
  }
  try {
    await cleanup();
  } catch (error) {
    if (!failure) throw new Error(`the check passed but its cleanup failed: ${error?.message ?? error}`);
    failure = new Error(`${failure?.message ?? failure}; cleanup also failed: ${error?.message ?? error}`);
  }
  if (failure) throw failure;
  return detail;
}

/** Reads the total from the Accounts toolbar count, filtered ("1 of 1,204 accounts") or not
 * ("3 accounts"). Input: the count text. Output: the total. */
export function listTotal(text) {
  const found = /^(?:[\d,]+ of )?([\d,]+) accounts?$/u.exec(String(text).trim());
  if (!found) throw new Error(`the Accounts count reads ${JSON.stringify(text)}, not a number of accounts`);
  const total = Number(found[1].replaceAll(",", ""));
  assert(Number.isSafeInteger(total));
  return total;
}

/** Reads each visible element's own text, leaving out child elements such as a Recommended
 * badge. Input: the CSS selector. Output: the texts in page order, trimmed. */
export function ownTexts(selector) {
  return browser.execute((selector) => [...document.querySelectorAll(selector)].filter((node) => node.getClientRects().length)
    .map((node) => [...node.childNodes].filter((child) => child.nodeType === Node.TEXT_NODE).map((child) => child.textContent).join("").trim()), selector);
}

/** The account names the Accounts list draws now. Output: the names in row order. */
export function rowNames() {
  return browser.execute(() => [...document.querySelectorAll("button.account-row .cell .ellipsis")]
    .filter((node) => node.getClientRects().length).map((node) => node.textContent));
}

/** Opens Settings, Accounts searched down to one account, so its row and menu are drawn.
 * Inputs: the kit and the name. Output: the toolbar count text. */
export async function findAccount(kit, name) {
  if ((await kit.hash()) !== "#/settings/accounts") await kit.go("#/settings/accounts");
  await type(SEARCH, name);
  await kit.until(async () => (await rowNames()).includes(name), kit.UI_MS, `Settings, Accounts never listed ${name}`);
  return (await ownTexts("main .set-bar .count"))[0];
}

/** Closes the open dialog or drawer with its Close button and waits until none is open.
 * Input: the kit. Output: none. */
export async function closeDialog(kit) {
  if (!(await shown("dialog[open]"))) return;
  await click("dialog[open] button[aria-label=\"Close\"]");
  await kit.until(async () => !(await shown("dialog[open]")), kit.UI_MS, "the dialog did not close");
}

/** The text the window shows, as the kit's screen() reads it but without a screenshot, for
 * waits. Output: the text of the page, an open dialog and toasts. */
export function windowText() {
  return browser.execute(() => [...document.querySelectorAll("main, dialog[open], .toast")].map((node) => node.innerText).join("\n"));
}

/** One account's status as the engine lists it now. Input: the id. Output: the status, or
 * null when the account is gone. */
export async function statusOf(id) {
  return (await accounts()).find((account) => account.id === id)?.status ?? null;
}
