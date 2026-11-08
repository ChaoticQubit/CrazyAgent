import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { join } from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);
const OBSERVATION_BYTES_MAX = 4096;

/** Quotes text as an XPath string literal. Input: text without a double quote. Output: the literal. */
function literal(text) {
  assert(!text.includes('"'), "XPath text has no double quote");
  return `"${text}"`;
}

/** Turns a step selector into a WebDriver selector. Input: "button=<exact text>" or
 * "text=<part>" for a button or link by its visible text, "label=<part>" for a label, or a CSS
 * or XPath selector as given. Output: the CSS or XPath selector. */
function locator(selector) {
  if (selector.startsWith("button=")) return `//*[self::button or self::a][normalize-space(.)=${literal(selector.slice(7))}]`;
  if (selector.startsWith("text=")) return `//*[self::button or self::a][contains(normalize-space(.), ${literal(selector.slice(5))})]`;
  if (selector.startsWith("label=")) return `//label[contains(normalize-space(.), ${literal(selector.slice(6))})]`;
  return selector;
}

/** Finishes every finite CSS animation, such as a dialog rising from opacity 0, so the window
 * shows what the owner sees once it settles: WebKit leaves them on their first frame in a
 * window that is not in front. Infinite animations (spin, shimmer) keep running. */
function settle() {
  return browser.execute(() => document.getAnimations()
    .filter((animation) => animation.effect?.getComputedTiming().endTime !== Infinity)
    .forEach((animation) => animation.finish()));
}

/** Finds the first displayed element a step selector names, once animations have settled.
 * Input: the selector. Output: the WebDriver element, or null when none is displayed. */
async function find(selector) {
  await settle();
  for (const element of await $$(locator(selector))) {
    if (await element.isDisplayed()) return element;
  }
  return null;
}

/** Whether the owner could act on an element: displayed, enabled, not aria-disabled, and the
 * element under its centre (document.elementFromPoint) is the element or inside it, so a
 * dialog, backdrop or overlay above it makes it unreachable. Input: the element. */
async function reachable(element) {
  return await element.isClickable() && await element.getAttribute("aria-disabled") !== "true";
}

/** Whether a step selector names a displayed element. Input: the selector. Output: a boolean. */
export async function shown(selector) {
  return Boolean(await find(selector));
}

/** Reads the visible text of every element a CSS selector matches. Input: the selector.
 * Output: their texts in page order. */
export function texts(selector) {
  return browser.execute((selector) => [...document.querySelectorAll(selector)]
    .filter((node) => node.getClientRects().length).map((node) => node.textContent), selector);
}

/** Reads the visible text of every element a CSS selector matches. Input: the selector.
 * Output: their text joined by newlines, empty when none is visible. */
export async function text(selector) {
  return (await texts(selector)).join("\n");
}

/** Whether exactly one visible element matches a CSS selector and its text is exactly the
 * expected text. Inputs: the selector and the text. Output: a boolean. */
export async function says(selector, expected) {
  const found = await texts(selector);
  return found.length === 1 && found[0] === expected;
}

/** Reads each visible toast's own message, without its icon or Open button. Output: the
 * messages in page order. */
export function toasts() {
  return browser.execute(() => [...document.querySelectorAll(".toast")].filter((node) => node.getClientRects().length)
    .map((node) => [...node.childNodes].filter((child) => child.nodeType === Node.TEXT_NODE).map((child) => child.textContent).join("")));
}

/** Waits for a condition and fails with what the window shows when it never holds.
 * Inputs: the condition, the time limit in ms and the failure label. Output: none. */
export async function until(condition, timeout, label) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 250 });
  } catch {
    const screen = await text("main, dialog[open], .toast");
    throw new Error(`${label}; on screen: ${JSON.stringify(screen.slice(0, 2000))}; at ${await browser.execute(() => location.hash)}`);
  }
}

/** Clicks a control with a WebDriver element click once the owner could reach it.
 * Inputs: the step selector and the time limit in ms. */
export async function click(selector, timeout = 15000) {
  let target = null;
  await until(async () => {
    target = await find(selector);
    return Boolean(target) && await reachable(target);
  }, timeout, `${selector} was never shown, enabled and on top`);
  await target.click();
}

/** Opens the fold with the given label, such as the Agents tab's "Agents and tasks", once it
 * shows, and leaves an open one as it is. Input: the fold's label. */
export async function unfold(label) {
  const fold = `details[aria-label="${label}"]`;
  await until(async () => await browser.execute((selector) => document.querySelector(selector) !== null, fold), 15000, `no "${label}" fold`);
  if (!(await browser.execute((selector) => document.querySelector(selector).open, fold))) await click(`${fold} > summary`);
  await until(async () => await browser.execute((selector) => document.querySelector(selector)?.open === true, fold), 10000, `the "${label}" fold did not open`);
}

/** Types into a field with WebDriver setValue once the owner could reach it, and waits for
 * the field to hold exactly that value. Inputs: the step selector and the value. */
export async function type(selector, value) {
  let field = null;
  await until(async () => {
    field = await find(selector);
    return Boolean(field) && await reachable(field);
  }, 15000, `${selector} was never shown, enabled and on top`);
  await field.setValue(value);
  await until(async () => await field.getValue() === value, 10000, `${selector} did not keep the typed value`);
}

/** Whether the checkbox inside a label is checked. Input: the "label=<part>" selector.
 * Output: a boolean. */
export async function checked(selector) {
  const label = await find(selector);
  assert(label, `${selector} is shown`);
  return (await label.$("input")).isSelected();
}

/** Waits until no dialog is open, so a page-level click cannot land under a dialog that was
 * meant to close. Input: what the page click is for. */
async function noDialog(purpose) {
  await until(async () => !(await shown("dialog[open]")), 15000, `a dialog stayed open over the page before ${purpose}`);
}

/** Saves a screenshot of the settled window into the run's private evidence folder.
 * Input: the file stem. */
export async function shot(name) {
  assert.match(name, /^[a-z0-9-]{1,40}$/);
  await settle();
  await browser.saveScreenshot(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, `${name}.png`));
}

/** Opens one Settings tab by its label, as the owner clicks it. Input: the tab label. */
export async function settingsTab(label) {
  await noDialog(`Settings, ${label}`);
  await click(`//*[@aria-label="Settings sections"]//*[self::a or self::button][normalize-space(.)=${literal(label)}]`);
}

/** Picks one Add account tile or sign-in method by its title, as the owner clicks it.
 * Inputs: "tile" or "method" and the exact title (a method's Recommended badge is ignored). */
export async function pick(kind, title) {
  assert(["tile", "method"].includes(kind), "an Add account tile or method");
  const has = (name) => `contains(concat(" ", normalize-space(@class), " "), " ${name} ")`;
  await click(`//dialog[@open]//button[${has(kind)}][.//*[${has("title")}][normalize-space(text()[1])=${literal(title)}]]`);
}

/** Opens a row's actions menu and picks one item. Inputs: the menu's accessible label
 * ("More for <name>") and the item text. */
export async function menu(label, item) {
  await noDialog(label);
  await click(`button[aria-label=${JSON.stringify(label)}]`);
  await click(`//*[@role="menuitem"][normalize-space(.)=${literal(item)}]`);
}

/** Goes through first run the way an owner does: saves the profile, continues past the
 * projects folder, skips tool setup (nothing is installed) and picks Add account, which
 * opens Settings, Accounts with the Add account dialog. Input: the owner's name. */
export async function finishWelcome(name) {
  await until(() => shown("#profile-name"), 60000, "first-run Welcome page did not appear");
  assert.deepEqual(await texts("h1"), ["Tell CrazyAgent about you"]);
  await type("#profile-name", name);
  await type("#profile-role", "Founder");
  await type("#profile-bio", "Checks Settings on the real desktop app.");
  await click("button=Save and continue");
  await until(() => says("h1", "Choose your projects folder"), 15000, "the projects folder step did not open");
  await click("button=Continue");
  await until(() => says("h1", "Set up tools"), 15000, "the tools step did not open");
  await until(async () => await shown("button=Skip for now") || await shown("button=Continue"), 30000, "the tools step offered no way on");
  await click(await shown("button=Skip for now") ? "button=Skip for now" : "button=Continue");
  await until(() => says("h1", "Add an account"), 15000, "the accounts step did not open");
  await click("button=Add account");
  await until(async () => /^#\/settings\/accounts/.test(await browser.execute(() => location.hash)) &&
    await says("dialog[open] h2", "Add an account"), 15000, "Add account did not open in Settings, Accounts");
}

/** Reads what the engine itself holds for model access through the desktop owner fixture,
 * independently of the window. Output: the engine's saved account registry, the settings
 * accounts, connections and pools records and whether every settings collection is writable. */
export async function observeModelAccess() {
  const result = await execute(process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE, ["observe-model-access"],
    { timeout: 10000, killSignal: "SIGKILL", maxBuffer: OBSERVATION_BYTES_MAX * 2 });
  assert.equal(result.stderr, "", "the engine observation ran without diagnostics");
  assert(Buffer.byteLength(result.stdout) <= OBSERVATION_BYTES_MAX, "the engine observation stays bounded");
  const { collections: pages, registry } = JSON.parse(result.stdout);
  assert.deepEqual(pages.map((page) => page.collection), ["accounts", "connections", "pools"]);
  assert(Array.isArray(registry), "the engine listed its saved accounts");
  const writable = pages.every((page) => page.writable);
  assert.equal(pages.some((page) => page.writable), writable, "every collection has the same authority");
  return {
    writable,
    registry,
    accounts: pages[0].items.map((item) => item.account_ref),
    connections: pages[1].items.map((item) => item.connection),
    pools: pages[2].items.map((item) => item.pool),
  };
}
