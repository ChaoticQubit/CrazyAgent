import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { join } from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);
const OBSERVATION_BYTES_MAX = 4096;

/** Acts on the first visible element a selector names, the way an owner reaches it.
 * Inputs: "button=<exact text>", "text=<part of a button or link>", "label=<part of a label>",
 * or a CSS selector; the action ("ready", "click", "type" or "checked"); the typed value.
 * Output: whether the element is usable, was clicked or typed into, or is checked. */
export function control(selector, action = "ready", value = "") {
  return browser.execute((selector, action, value) => {
    const visible = (node) => Boolean(node && node.getClientRects().length);
    const among = (query, match) => [...document.querySelectorAll(query)].find((node) => visible(node) && match(node));
    const node = selector.startsWith("button=") ? among("button, a", (item) => item.textContent.trim() === selector.slice(7))
      : selector.startsWith("text=") ? among("button, a", (item) => item.textContent.includes(selector.slice(5)))
      : selector.startsWith("label=") ? among("label", (item) => item.textContent.includes(selector.slice(6)))
      : among(selector, () => true);
    const usable = Boolean(node && !node.disabled && node.getAttribute("aria-disabled") !== "true");
    if (action === "ready") return usable;
    if (action === "checked") {
      const box = node?.matches("input") ? node : node?.querySelector("input");
      return Boolean(box?.checked);
    }
    if (!usable) return false;
    if (action === "click") {
      node.click();
      return true;
    }
    if (action === "type") {
      const prototype = node instanceof HTMLInputElement ? HTMLInputElement.prototype
        : node instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : null;
      if (!prototype) return false;
      Object.getOwnPropertyDescriptor(prototype, "value").set.call(node, value);
      node.dispatchEvent(new Event("input", { bubbles: true }));
      return node.value === value;
    }
    return false;
  }, selector, action, value);
}

/** Reads the visible text of every element a CSS selector matches. Input: the selector.
 * Output: their text joined by newlines, empty when none is visible. */
export function text(selector) {
  return browser.execute((selector) => [...document.querySelectorAll(selector)]
    .filter((node) => node.getClientRects().length).map((node) => node.textContent).join("\n"), selector);
}

/** Waits for a condition and fails with what the window shows when it never holds.
 * Inputs: the condition, the time limit in ms and the failure label. Output: none. */
export async function until(condition, timeout, label) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 250 });
  } catch {
    const shown = await text("main, dialog[open], .toast");
    throw new Error(`${label}; on screen: ${JSON.stringify(shown.slice(0, 2000))}; at ${await browser.execute(() => location.hash)}`);
  }
}

/** Clicks a control once it is usable. Inputs: the selector and the time limit in ms. */
export async function click(selector, timeout = 15000) {
  await until(() => control(selector), timeout, `${selector} never became usable`);
  assert.equal(await control(selector, "click"), true, `${selector} click`);
}

/** Types into a field once it is usable. Inputs: the selector and the value. */
export async function type(selector, value) {
  await until(() => control(selector), 15000, `${selector} never became usable`);
  assert.equal(await control(selector, "type", value), true, `${selector} typing`);
}

/** Saves a screenshot of the settled window into the run's private evidence folder: finite
 * CSS animations such as a dialog's entry are finished first, since WebKit can leave them
 * on their first frame in a window that is not in front. Input: the file stem. */
export async function shot(name) {
  assert.match(name, /^[a-z0-9-]{1,40}$/);
  await browser.execute(() => document.getAnimations()
    .filter((animation) => animation.effect?.getComputedTiming().endTime !== Infinity)
    .forEach((animation) => animation.finish()));
  await browser.saveScreenshot(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, `${name}.png`));
}

/** Opens one Settings tab by its label, as the owner clicks it. Input: the tab label. */
export async function settingsTab(label) {
  await until(() => browser.execute((label) => [...document.querySelectorAll("[aria-label='Settings sections'] a, [aria-label='Settings sections'] button")]
    .some((node) => node.textContent.trim() === label && node.getClientRects().length), label), 15000, `Settings tab ${label} never showed`);
  await browser.execute((label) => [...document.querySelectorAll("[aria-label='Settings sections'] a, [aria-label='Settings sections'] button")]
    .find((node) => node.textContent.trim() === label).click(), label);
}

/** Picks one Add account tile or sign-in method by its title, as the owner clicks it.
 * Inputs: "tile" or "method" and the exact title (a method's Recommended badge is ignored). */
export async function pick(kind, title) {
  assert(["tile", "method"].includes(kind), "an Add account tile or method");
  const act = (kind, title, press) => {
    const node = [...document.querySelectorAll(`dialog[open] button.${kind}`)]
      .find((item) => item.getClientRects().length && item.querySelector(".title")?.firstChild?.textContent === title);
    if (node && press) node.click();
    return Boolean(node);
  };
  await until(() => browser.execute(act, kind, title, false), 15000, `${kind} ${title} never showed`);
  assert.equal(await browser.execute(act, kind, title, true), true, `${kind} ${title} click`);
}

/** Opens a row's actions menu and picks one item. Inputs: the menu's accessible label
 * ("More for <name>") and the item text. */
export async function menu(label, item) {
  await click(`button[aria-label='${label}']`);
  await until(() => browser.execute((item) => [...document.querySelectorAll("[role=menuitem]")]
    .some((node) => node.textContent.trim() === item && node.getClientRects().length), item), 15000, `menu item ${item} never showed`);
  await browser.execute((item) => [...document.querySelectorAll("[role=menuitem]")]
    .find((node) => node.textContent.trim() === item).click(), item);
}

/** Goes through first run the way an owner does: saves the profile, continues past the
 * projects folder, skips tool setup (nothing is installed) and picks Add account, which
 * opens Settings, Accounts with the Add account dialog. Input: the owner's name. */
export async function finishWelcome(name) {
  await until(() => control("#profile-name"), 60000, "first-run Welcome page did not appear");
  assert.match(await text("h1"), /Tell CrazyAgent about you/);
  await type("#profile-name", name);
  await type("#profile-role", "Founder");
  await type("#profile-bio", "Checks Settings on the real desktop app.");
  await click("button=Save and continue");
  await until(async () => (await text("h1")).includes("Choose your projects folder"), 15000, "the projects folder step did not open");
  await click("button=Continue");
  await until(async () => (await text("h1")).includes("Set up tools"), 15000, "the tools step did not open");
  await until(async () => await control("button=Skip for now") || await control("button=Continue"), 30000, "the tools step offered no way on");
  await click(await control("button=Skip for now") ? "button=Skip for now" : "button=Continue");
  await until(async () => (await text("h1")).includes("Add an account"), 15000, "the accounts step did not open");
  await click("button=Add account");
  await until(async () => /^#\/settings\/accounts/.test(await browser.execute(() => location.hash)) &&
    (await text("dialog[open] h2")).includes("Add an account"), 15000, "Add account did not open in Settings, Accounts");
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
