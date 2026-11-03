import assert from "node:assert/strict";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { SECRET_SOURCES, mask } from "./trial3-mask.mjs";
import { teamWait } from "./trial3-team.mjs";
import { click, shot, shown } from "./settings-steps.mjs";

export const MODE = process.env.TRIAL3_MODE;
export const EVIDENCE = process.env.TRIAL_EVIDENCE;
export const PROJECT = process.env.TRIAL_PROJECT;
export const NAME = PROJECT ? basename(PROJECT) : "";
export const TEAM_MS = Number(process.env.TRIAL_TEAM_MS ?? 7200000);
export const UI_MS = 30000;
assert(Number.isFinite(TEAM_MS) && TEAM_MS > 0, "the team deadline is positive");

/** The state the steps of one trial run share: desktop step count, the project id, the leads,
 * the owner's Master input, the failed step slugs, the team deadline and the step being run. */
export const run = { desktop: 0, project: null, leads: [], input: null, failed: [], teamBy: null, current: null };

/** Quotes text as an XPath string literal. Input: text without a double quote. Output: it quoted. */
export function lit(value) {
  assert(!value.includes('"'), "XPath text has no double quote");
  return `"${value}"`;
}

/** The XPath of one option button of a segmented control. Inputs: the control's label and
 * the option's text. Output: the selector. */
export function segment(group, option) {
  return `//*[@role="radiogroup"][@aria-label=${lit(group)}]//button[normalize-space(.)=${lit(option)}]`;
}

/** Appends one masked desktop line to steps.jsonl, numbered after every line already there.
 * The runner (scripts/trial/run.mjs) writes its own lines only before it starts this
 * WebdriverIO run and after the run's process group is stopped, so the two writers never
 * append at once and every n is unique. Input: the line without its n. */
export function record(line) {
  const steps = join(EVIDENCE, "steps.jsonl");
  const before = existsSync(steps) ? readFileSync(steps, "utf8").split("\n").filter(Boolean).length : 0;
  appendFileSync(steps, `${JSON.stringify({ n: before + 1, surface: "desktop", ...line, detail: mask(line.detail) })}\n`, { mode: 0o600 });
}

/** Masks every secret shape in the window's text and form values and hides terminal
 * canvases, whose text cannot be masked, so the next screenshot holds no secret. */
function maskWindow() {
  return browser.execute((sources) => {
    const patterns = sources.map((source) => new RegExp(source, "g"));
    const clean = (value) => patterns.reduce((out, pattern) => out.replace(pattern, "[masked]"), value);
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const next = clean(node.nodeValue);
      if (next !== node.nodeValue) node.nodeValue = next;
    }
    for (const field of document.querySelectorAll("input, textarea")) {
      const next = clean(field.value);
      if (next !== field.value) field.value = next;
    }
    for (const canvas of document.querySelectorAll(".tg-terminal-screen canvas")) canvas.style.visibility = "hidden";
  }, SECRET_SOURCES);
}

/** Shows the terminal canvases maskWindow hid once the screenshot is taken. */
function unhideTerminals() {
  return browser.execute(() => {
    for (const canvas of document.querySelectorAll(".tg-terminal-screen canvas")) canvas.style.visibility = "";
  });
}

/** Saves one masked screenshot. Input: the file stem. Output: its path in the run folder. */
async function maskedShot(name) {
  await maskWindow();
  await shot(name);
  await unhideTerminals();
  return `screenshots/${name}.png`;
}

/** Runs one owner step: the action, then a masked screenshot NN-<slug>.png on success or
 * failure, then its steps.jsonl line listing every screenshot the step took. Inputs: the
 * slug, the title, why the trial does it and the action, which returns what it saw. Throws
 * when the action or the screenshot failed. */
export async function step(slug, title, why, action) {
  run.desktop += 1;
  const name = `${String(run.desktop).padStart(2, "0")}-${slug}`;
  assert.match(name, /^[a-z0-9-]{1,40}$/);
  const at = new Date().toISOString();
  run.current = { name, shots: [] };
  let ok = true;
  let detail;
  try {
    detail = String(await action());
  } catch (failure) {
    ok = false;
    detail = String(failure?.message ?? failure);
  }
  const evidence = run.current.shots;
  run.current = null;
  try {
    evidence.push(await maskedShot(name));
  } catch (failure) {
    ok = false;
    detail = `${detail}; no screenshot: ${failure?.message ?? failure}`;
  }
  record({ title, why, at, ok, detail, evidence });
  if (!ok) {
    run.failed.push(slug);
    throw new Error(`${title}: ${mask(detail)}`);
  }
}

/** Takes a masked screenshot at the moment that proves a check, named after the running step
 * (NN-<slug>-<k>.png) and listed in that step's evidence. Output: the window text as screen()
 * reads it; throws outside a step. */
export async function proof() {
  assert(run.current, "proof runs inside a step");
  const k = run.current.shots.length + 1;
  assert(k <= 9, "a step takes at most 9 proof screenshots");
  run.current.shots.push(await maskedShot(`${run.current.name.slice(0, 38)}-${k}`));
  return screen();
}

/** The text the window shows as the owner reads it: the innerText of the page, an open
 * dialog and toasts, so labels stay apart and the mask cannot fuse them into one token.
 * Output: the text. */
export function screen() {
  return browser.execute(() => [...document.querySelectorAll("main, dialog[open], .toast")].map((node) => node.innerText).join("\n"));
}

/** Waits for a condition and fails with what the window shows when it never holds.
 * Inputs: the condition, the time limit in ms and the failure label. */
export async function until(condition, timeout, label) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 250 });
  } catch {
    throw new Error(`${label}; on screen: ${JSON.stringify((await screen()).slice(0, 2000))}; at ${await hash()}`);
  }
}

/** Waits for a condition with a deadline and fails with the last thing it observed.
 * Inputs: the condition, which returns { done, seen }, the deadline in ms and the label. */
export async function waitFor(condition, timeout, label) {
  let seen = "nothing read yet";
  try {
    await browser.waitUntil(async () => {
      const result = await condition();
      seen = result.seen;
      return result.done;
    }, { timeout, interval: 1000 });
  } catch {
    throw new Error(`${label} within ${Math.round(timeout / 1000)} s; last seen: ${seen}`);
  }
  return seen;
}

/** The window's route. Output: location.hash. */
export function hash() {
  return browser.execute(() => location.hash);
}

/** Opens a route by its hash and waits until the window shows it. Input: the hash. */
export async function go(route) {
  await browser.execute((next) => { location.hash = next; }, route);
  await until(async () => (await hash()) === route, UI_MS, `${route} did not open`);
}

/** The trial project's id, read by the add-project step. Output: the id; throws when no
 * step found it. */
export function project() {
  assert(run.project, "needs the project id from the add-project step");
  return run.project;
}

/** The time the next team step may wait, from the one deadline the team steps share; throws
 * a not-run failure when the project or, live, the prompt failed. Output: the wait in ms. */
export function team() {
  const next = teamWait({ mode: MODE, failed: run.failed, teamBy: run.teamBy, now: Date.now(), teamMs: TEAM_MS, floorMs: Math.min(UI_MS, TEAM_MS) });
  run.teamBy = next.teamBy;
  return next.wait;
}

/** Opens one tab of the trial project. Input: the tab id. */
export function tab(id) {
  return go(`#/projects/${encodeURIComponent(project())}/${id}`);
}

/** Scrolls a control to the middle of the window, so the step's screenshot shows the setting
 * it read back. Input: the selector. */
export async function inView(selector) {
  await $(selector).scrollIntoView({ block: "center" });
}

/** Opens another tab and comes back, so the tab reads what the engine saved rather than
 * what the form still holds. Input: the tab id. */
export async function reopen(id) {
  await tab("overview");
  await tab(id);
}

/** Sets a native select as the owner's choice does, firing its change event. Inputs: the
 * selector and the option value. Output: whether the value changed. */
export async function pick(selector, value) {
  await until(() => shown(selector), UI_MS, `${selector} was never shown`);
  return browser.execute((selector, value) => {
    const field = document.querySelector(selector);
    if (![...field.options].some((option) => option.value === value)) throw new Error(`${selector} has no option ${value}`);
    if (field.value === value) return false;
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(field, value);
    field.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }, selector, value);
}

/** Puts text into a React text area as one paste, so a newline never presses Enter and
 * sends early. Inputs: the selector and the text. */
export async function paste(selector, value) {
  await browser.execute((selector, value) => {
    const box = [...document.querySelectorAll(selector)].find((node) => node.getClientRects().length);
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(box, value);
    box.dispatchEvent(new Event("input", { bubbles: true }));
  }, selector, value);
  await until(async () => (await $(selector).getValue()) === value, UI_MS, `${selector} did not keep the pasted text`);
}

/** Picks one option of a segmented control and checks it reads as chosen after the tab is
 * opened again. Inputs: the tab it is on, the control's label and the option. Output: what
 * was done. */
export async function choose(where, group, option) {
  await tab(where);
  await until(() => shown(segment(group, option)), UI_MS, `${group} was never shown`);
  const was = await $(segment(group, option)).getAttribute("aria-checked") === "true";
  if (!was) await click(segment(group, option));
  await until(async () => await $(segment(group, option)).getAttribute("aria-checked") === "true", UI_MS, `${group} did not take ${option}`);
  await reopen(where);
  await until(async () => await shown(segment(group, option)) && await $(segment(group, option)).getAttribute("aria-checked") === "true", UI_MS, `${group} did not read ${option} after reopening`);
  await inView(segment(group, option));
  return `${group} reads ${option} after reopening the tab${was ? " (it already was)" : ""}`;
}

/** Reads the project's team graph from the engine through the app's own command and the
 * cards the Agents tab draws. Output: the nodes and the card counts by kind. */
export async function graph() {
  const answer = await browser.executeAsync((id, done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_team_graph", { id }).then(done, (failure) => done({ failed: String(failure) }));
  }, project());
  const nodes = answer?.nodes ?? answer?.value?.nodes;
  assert(Array.isArray(nodes), `desktop_team_graph answered no nodes: ${JSON.stringify(answer).slice(0, 300)}`);
  const cards = await browser.execute(() => Object.fromEntries(["master", "lead", "sub", "teammate"]
    .map((kind) => [kind, [...document.querySelectorAll(`.tg-card.${kind}`)].filter((node) => node.getClientRects().length).length])));
  return { nodes, cards };
}
