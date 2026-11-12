import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { SECRET_SOURCES, mask } from "./trial3-mask.mjs";
import { teamWait } from "./trial3-team.mjs";
import { click, shot, shown } from "./settings-steps.mjs";
import { PICKS_VARIABLE, RELAUNCHED, appBundle, attach, command, ended, listener } from "./trial-app.mjs";

export { PICKS_VARIABLE };

export const MODE = process.env.TRIAL3_MODE;
export const EVIDENCE = process.env.TRIAL_EVIDENCE;
export const PROJECT = process.env.TRIAL_PROJECT;
export const NAME = PROJECT ? basename(PROJECT) : "";
export const TEAM_MS = Number(process.env.TRIAL_TEAM_MS ?? 7200000);
export const UI_MS = 30000;
const OPEN_MS = 60000;
const PURPOSES = ["skill", "project", "projects_home", "memory", "plugin"];
assert(Number.isFinite(TEAM_MS) && TEAM_MS > 0, "the team deadline is positive");

/** The state the steps of one trial run share: desktop step count, the project id, the leads,
 * the owner's Terminal input, the failed step slugs, the team deadline and the step being run. */
export const run = { desktop: 0, project: null, leads: [], failed: [], teamBy: null, current: null };

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

/** Appends one masked line to steps.jsonl, a desktop line unless it names its surface,
 * numbered after every line already there.
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
 * slug, the title, why the trial does it, the action, which returns what it saw, and the
 * surface the step drives with how it saves its last proof (a TUI step saves a frame).
 * Throws when the action or the last proof failed. */
export async function step(slug, title, why, action, finish = { surface: "desktop", shoot: maskedShot }) {
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
    evidence.push(await finish.shoot(name));
  } catch (failure) {
    ok = false;
    detail = `${detail}; no screenshot: ${failure?.message ?? failure}`;
  }
  record({ surface: finish.surface, title, why, at, ok, detail, evidence });
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

/** The dialog each user menu route opens over the page instead of a Settings page (DEC-151). */
export const USER_DIALOGS = { "#/settings/profile": "Profile", "#/settings/updates": "Updates", "#/settings/github": "GitHub", "#/settings/home": "Projects folder" };

/** The Close button of an open user menu dialog. */
const USER_DIALOG_CLOSE = "dialog[open]:has(> .dialog-body > .user-dialog) .dialog-head button:last-child";

/** Opens a route by its hash and waits until the window shows it: the page, or for Profile,
 * Updates, GitHub and Projects folder their dialog over the page. An open user menu dialog is
 * closed first, as the owner closes it before going elsewhere. Input: the hash. */
export async function go(route) {
  await browser.execute((close) => { document.querySelector(close)?.click(); }, USER_DIALOG_CLOSE);
  await browser.execute((next) => { location.hash = next; }, route);
  const title = USER_DIALOGS[route];
  if (title === undefined) {
    await until(async () => (await hash()) === route, UI_MS, `${route} did not open`);
    return;
  }
  await until(() => browser.execute((wanted) => [...document.querySelectorAll("dialog[open] .dialog-head h2")].some((head) => head.textContent === wanted), title), UI_MS, `${route} did not open the ${title} dialog`);
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

/** Makes the app's next native folder chooser for a purpose answer a path, because WebDriver
 * cannot drive the macOS chooser and Tauri 2 locks the page's invoke against wrapping: writes
 * the path as a private one-shot answer file named after the purpose in the folder
 * PICKS_VARIABLE names, which only a native-webdriver build reads and removes when the page
 * asks for that chooser. The app's own code gets the path and the engine does the rest; the
 * chooser window itself is not exercised. Inputs: the absolute path and the purpose (project,
 * the default, skill, projects_home, memory or plugin). Throws when no picks folder is named or
 * an earlier answer of the purpose still waits. */
export async function chooseFolder(path, purpose = "project") {
  assert(typeof path === "string" && isAbsolute(path), `chooseFolder takes an absolute path, got ${path}`);
  assert(!/[\u0000-\u001f\u007f]/.test(path), "chooseFolder takes a path without control characters");
  assert(PURPOSES.includes(purpose), `the purpose is one of ${PURPOSES.join(", ")}, got ${purpose}`);
  const picks = process.env[PICKS_VARIABLE];
  assert(picks && isAbsolute(picks), `chooseFolder needs ${PICKS_VARIABLE}, the absolute folder the native-webdriver app takes its next pick from`);
  mkdirSync(picks, { recursive: true, mode: 0o700 });
  chmodSync(picks, 0o700);
  const answer = join(picks, purpose);
  assert(!existsSync(answer), `an earlier ${purpose} pick still waits in ${picks}: the app never asked for that chooser`);
  writeFileSync(answer, `${path}\n`, { mode: 0o600, flag: "wx" });
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

/** Quits the app and opens it again as the Dock does: open -a on its bundle, so it starts with
 * no CRAZYAGENT_DESKTOP_SOCKET and only the variables its WebDriver server and its trial folder
 * pick (PICKS_VARIABLE, when set) need, then
 * attaches a new WebDriver session. Live runs only: an app opened that way looks for this
 * machine's default engine and may install its login service. The relaunched app is recorded
 * in relaunched.json; the conf and the runner quit it. Output: what it did. */
export async function relaunch() {
  assert.equal(MODE, "live", "relaunch runs in a live run only: an app opened without CRAZYAGENT_DESKTOP_SOCKET looks for this machine's default engine and may install its login service");
  const port = Number(process.env.WD_PORT);
  const bundle = appBundle(process.env.TRIAL3_APP);
  const old = listener(port);
  assert(old, `no app serves WebDriver on port ${port}`);
  const binary = command(old);
  process.kill(old, "SIGTERM");
  assert(await ended(old, binary, UI_MS), `the app (pid ${old}) did not quit within ${UI_MS / 1000} s`);
  const others = spawnSync("/usr/bin/pgrep", ["-f", binary], { encoding: "utf8", timeout: 10000 }).stdout.trim();
  assert.equal(others, "", `another ${binary} runs (pid ${others}), so open -a would bring it forward instead of opening the app`);
  const picks = process.env[PICKS_VARIABLE] ? ["--env", `${PICKS_VARIABLE}=${process.env[PICKS_VARIABLE]}`] : [];
  const opened = spawnSync("/usr/bin/open", ["-a", bundle, "--env", `TAURI_WEBDRIVER_PORT=${port}`, "--env", "WDIO_EMBEDDED_SERVER=true", ...picks], { encoding: "utf8", timeout: 30000 });
  assert.equal(opened.status, 0, `open -a ${bundle} failed: ${opened.stderr}`);
  let pid = null;
  for (const deadline = Date.now() + OPEN_MS; Date.now() < deadline; await pause(250)) {
    pid = listener(port);
    if (pid !== null && pid !== old && await fetch(`http://127.0.0.1:${port}/status`).then((reply) => reply.ok, () => false)) break;
    pid = null;
  }
  assert(pid, `the app opened by open -a never served WebDriver on port ${port} within ${OPEN_MS / 1000} s`);
  writeFileSync(join(EVIDENCE, RELAUNCHED), `${JSON.stringify({ pid, binary: command(pid), bundle, at: new Date().toISOString() })}\n`, { mode: 0o600 });
  await browser.reloadSession();
  await attach(browser);
  return `quit pid ${old}, opened ${bundle} with open -a (no CRAZYAGENT_DESKTOP_SOCKET), WebDriver attached to pid ${pid}`;
}

const WEBDRIVER_KEYS = { "\uE000": "Release", "\uE003": "Backspace", "\uE004": "Tab", "\uE006": "Enter", "\uE007": "Enter", "\uE008": "Shift", "\uE009": "Control", "\uE00A": "Alt", "\uE00C": "Escape", "\uE03D": "Meta" };
const MODIFIERS = { Shift: "shiftKey", Control: "ctrlKey", Alt: "altKey", Meta: "metaKey" };
const TERMINAL_BYTES = { Backspace: "\u007f", Tab: "\t", Escape: "\u001b", Enter: "\r" };
const KEY_GAP_MS = 60;
const typingInstalled = new WeakSet();

/** Turns what browser.keys was given into what the kit sends to a terminal: runs of typed
 * text with Control+letter as its control byte, Enter as a carriage return of its own, so a
 * harness never reads it inside pasted-looking text, and Shift+Enter as a key press. WebDriver's modifier keys stay held until pressed
 * again or released by U+E000, within the one call, as browser.keys holds them. Input: a
 * string or a list of strings. Output: [{ text } or { key: "Enter", shiftKey: true }];
 * throws for a key or chord the terminal path does not send. */
export function terminalKeys(value) {
  const parts = Array.isArray(value) ? value : [value];
  assert(parts.every((part) => typeof part === "string"), "browser.keys takes strings");
  const held = { shiftKey: false, ctrlKey: false, altKey: false, metaKey: false };
  const out = [];
  const text = (data) => {
    const last = out.at(-1);
    if (last && "text" in last && !last.alone) last.text += data;
    else out.push({ text: data });
  };
  for (const char of parts.flatMap((part) => Array.from(part))) {
    const named = WEBDRIVER_KEYS[char];
    if (named === "Release") {
      for (const flag of Object.keys(held)) held[flag] = false;
      continue;
    }
    if (MODIFIERS[named]) {
      held[MODIFIERS[named]] = !held[MODIFIERS[named]];
      continue;
    }
    assert(!held.altKey && !held.metaKey, `the terminal path types no Alt or Meta chord, got ${JSON.stringify(char)}`);
    if (named === "Enter") {
      assert(!held.ctrlKey, "the terminal path sends no Enter with Control");
      out.push(held.shiftKey ? { key: "Enter", shiftKey: true } : { text: TERMINAL_BYTES.Enter, alone: true });
      continue;
    }
    if (named) {
      assert(!held.shiftKey && !held.ctrlKey, `the terminal path sends ${named} with no modifier`);
      text(TERMINAL_BYTES[named]);
      continue;
    }
    const code = char.codePointAt(0);
    assert(code < 0xe000 || code > 0xe05d, `the terminal path does not send the WebDriver key U+${code.toString(16).toUpperCase()}`);
    if (held.ctrlKey) {
      assert(/^[a-z]$/i.test(char) && !held.shiftKey, "the terminal path types Control only with a letter");
      text(String.fromCharCode(char.toLowerCase().charCodeAt(0) - 96));
      continue;
    }
    if (held.shiftKey) {
      assert(/^[a-z]$/i.test(char), "the terminal path types Shift only with a letter or Enter");
      text(char.toUpperCase());
      continue;
    }
    text(char);
  }
  return out;
}

/** Sends one terminal key event to the focused xterm in the page: text as one insertText
 * input event, which xterm reads once, and Shift+Enter as a keydown and keyup with shiftKey
 * set, which the app's key override (terminal-io.ts keyOverride) turns into a new line.
 * Runs in the page. Inputs: the event from terminalKeys and the page's global scope.
 * Output: false when the focus is not a terminal, so nothing was sent. */
export function sendTerminal(event, scope = globalThis) {
  const target = scope.document.activeElement;
  if (!target || !target.classList || !target.classList.contains("xterm-helper-textarea")) return false;
  if ("text" in event) {
    target.dispatchEvent(new scope.InputEvent("input", { data: event.text, inputType: "insertText", bubbles: true, cancelable: true }));
    return true;
  }
  for (const type of ["keydown", "keyup"]) {
    target.dispatchEvent(new scope.KeyboardEvent(type, { key: "Enter", code: "Enter", shiftKey: true, bubbles: true, cancelable: true }));
  }
  return true;
}

/** Sends one clipboard paste to the focused xterm in the page, as the owner's paste key does:
 * the app's own paste listener takes the text and brackets it, so a new line in it never
 * presses Enter. Runs in the page. Inputs: the text and the page's global scope. Output: false
 * when the focus is not a terminal, so nothing was sent. */
export function pasteTerminal(text, scope = globalThis) {
  const target = scope.document.activeElement;
  if (!target || !target.classList || !target.classList.contains("xterm-helper-textarea")) return false;
  const event = new scope.Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: { getData: () => text } });
  target.dispatchEvent(event);
  return true;
}

/** Routes browser.keys into a focused terminal through terminalKeys and sendTerminal, since
 * the WebDriver plugin's key actions type some characters twice and drop Shift from Enter
 * there (tauri-plugin-wdio-webdriver 1.5.0 executor.rs:1505, 1572-1592, 1310-1311 and 1477).
 * Keys sent anywhere else still go through WebDriver. Installs once per browser. Input: the
 * browser. */
export function terminalTyping(browser) {
  if (typingInstalled.has(browser)) return;
  typingInstalled.add(browser);
  browser.overwriteCommand("keys", async function keys(webdriverKeys, value) {
    const focused = await browser.execute(() => document.activeElement?.classList?.contains("xterm-helper-textarea") === true);
    if (!focused) return webdriverKeys(value);
    for (const [at, event] of terminalKeys(value).entries()) {
      if (at > 0) await pause(KEY_GAP_MS);
      assert(await browser.execute(sendTerminal, event), "the terminal lost the focus while the keys were sent");
    }
    return undefined;
  });
}
