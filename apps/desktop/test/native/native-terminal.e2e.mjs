import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { invoke, newProject, prepareWindow } from "./master-settings-window.mjs";
import { go, paste, pick, reopen, run, tab, until, waitFor } from "./trial-kit.mjs";
import { startIfPaused, teamNodes } from "./trial4/n-d8-master-reply.mjs";

const ROOT = process.env.MS_ROOT;
const SOCKET = join(ROOT, "engine", "terminal", "tmux.sock");
const SESSION = "crazyagent";
const UI_MS = 30000;
const MASTER_MS = 180000;
const END_MS = 20000;
const QUIET_MS = 4000;
const SHORT_TURN_MS = 6000;
const RUNNING = ["starting", "idle", "working"];
const READ = { kind: "read" };
const RETURN = { kind: "key", characters: "\r", code: 36 };
const RECT = { x: 40, y: 120, width: 640, height: 400 };
const THEME = {
  background: "#101010", foreground: "#e0e0e0", cursor: "#e0e0e0", cursorAccent: "#101010",
  selectionBackground: "#335588", black: "#000000", red: "#cc0000", green: "#00cc00",
  yellow: "#cccc00", blue: "#0000cc", magenta: "#cc00cc", cyan: "#00cccc", white: "#cccccc",
  brightBlack: "#555555", brightRed: "#ff5555", brightGreen: "#55ff55", brightYellow: "#ffff55",
  brightBlue: "#5555ff", brightMagenta: "#ff55ff", brightCyan: "#55ffff", brightWhite: "#ffffff",
};
const STYLE = { fontFamily: "Menlo", fontSize: 13, cursorBlink: false, theme: THEME };
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };
const opened = new Set();
let master = null;

/** Runs one tmux command on the test engine's own server, never another. Inputs: the
 * arguments after the socket. Output: the trimmed standard output; throws on a failure. */
function tmux(...args) {
  assert(isAbsolute(SOCKET) && SOCKET.startsWith(ROOT) && !SOCKET.includes("/tmux-"));
  const result = spawnSync("tmux", ["-S", SOCKET, ...args], { env: { PATH: process.env.PATH }, encoding: "utf8" });
  assert.equal(result.status, 0, `tmux ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Starts the project's team and waits until the master runs in a terminal, then leaves the
 * Conversation tab so the page's own master view closes and the spec's views own the
 * terminal. Output: the master's terminal name. */
async function liveMaster() {
  await tab("conversation");
  await startIfPaused(kit, run.project);
  let node = null;
  await waitFor(async () => {
    node = (await teamNodes(run.project)).find((each) => each.kind === "master") ?? null;
    return { done: Boolean(node?.terminal) && RUNNING.includes(node.status), seen: JSON.stringify(node).slice(0, 300) };
  }, MASTER_MS, "the master never ran in a terminal");
  await tab("overview");
  await pause(1500);
  assert.ok(node.terminal);
  return node.terminal;
}

/** Opens one native view on the master through the app's own open command, with a channel
 * whose events the window keeps. Input: whether typed keys reach the terminal. Output: the
 * view id and the channel's callback id. */
async function open(keys) {
  const tag = await browser.execute(() => {
    const kept = (window.__w5 ??= {});
    const id = window.__TAURI_INTERNALS__.transformCallback((raw) => (kept[id] ??= []).push(raw));
    return id;
  });
  const view = await invoke("desktop_native_terminal_open", {
    name: master, pane: null, label: "Native terminal trial", rect: RECT, style: STYLE,
    covered: false, keys, project: run.project, events: `__CHANNEL__:${tag}`,
  });
  assert.ok(Number.isInteger(view) && view > 0, `the open answered a view id: ${view}`);
  opened.add(view);
  return { view, tag };
}

/** Reads the events a view's channel delivered, in the order they were sent. Input: the
 * channel's callback id. Output: the event bodies, an end marker as { end: true }. */
function events(tag) {
  return browser.execute((tag) => (window.__w5?.[tag] ?? []).slice().sort((a, b) => a.index - b.index)
    .map((raw) => ("end" in raw ? { end: true } : raw.message)), tag);
}

/** Takes one probe step on a view. Inputs: the view and the step. Output: what the view
 * shows after it: rows, direct, focused, active, key, marked and pasteboard. */
function probe(view, step) {
  return invoke("desktop_native_terminal_probe", { view, step });
}

/** Reads a view until its state passes a check. Inputs: the view, the check, the time limit
 * and the failure label. Output: the state that passed. */
async function seenWhen(view, check, timeout, label) {
  let seen = null;
  await waitFor(async () => {
    seen = await probe(view, READ);
    return { done: check(seen), seen: JSON.stringify({ ...seen, rows: seen.rows.filter(Boolean).slice(-6) }) };
  }, timeout, label);
  return seen;
}

/** Waits for a view's ready event and the master's stand-in ready line on its rows, and
 * checks the test window sits on a 1x display, never the owner's built-in Retina. Input:
 * the opened view. Output: the view's state then. */
async function ready({ view, tag }) {
  await until(async () => (await events(tag)).some((event) => event.type === "ready"), UI_MS, `view ${view} never said ready`);
  const seen = await seenWhen(view, (seen) => seen.rows.some((row) => /stand-in ready/.test(row)), UI_MS, `view ${view} never showed the stand-in`);
  assert.equal(seen.scale, 1, "the test window is on a 1x display");
  return seen;
}

/** Counts the stand-in turns a view's rows show. Input: the view's state. Output: the count. */
function turns(seen) {
  return seen.rows.filter((row) => /stand-in turn/.test(row)).length;
}

/** Closes a view the spec opened. Input: the view. Output: none. */
async function close(view) {
  opened.delete(view);
  await invoke("desktop_native_terminal_close", { view });
}

describe("Native terminal views in the real window", () => {
  before(async () => {
    await prepareWindow();
    assert.notEqual(await browser.execute(() => document.title), "test", "the app embeds the real page, not a placeholder");
    await newProject("native-terminal-shop");
    master = await liveMaster();
  });

  after(async () => {
    for (const view of [...opened]) await close(view).catch(() => {});
  });

  it("opens on the master and draws its screen through the direct path", async () => {
    const shown = await open(true);
    const seen = await ready(shown);
    assert.equal(seen.direct, true, "a plain master window is drawn by the direct path");
    await close(shown.view);
  });

  it("focus gives the view the keyboard and leaves the app and its window as they were (N2)", async () => {
    const shown = await open(true);
    await ready(shown);
    const before = await probe(shown.view, READ);
    assert.equal(before.focused, false, "a fresh view does not hold the keyboard before focus");
    console.log(`N2 before ${JSON.stringify({ active: before.active, key: before.key })}`);
    await invoke("desktop_native_terminal_focus", { view: shown.view });
    const after = await seenWhen(shown.view, (seen) => seen.focused, UI_MS, "the view never took the keyboard");
    assert.equal(after.active, before.active, "focus never brings the app forward");
    assert.equal(after.key, before.key, "focus never makes the window key");
    await close(shown.view);
  });

  it("an input method commit and Return with keys off never reach the terminal (#517)", async () => {
    const shown = await open(false);
    const base = turns(await ready(shown));
    await invoke("desktop_native_terminal_focus", { view: shown.view });
    await probe(shown.view, { kind: "compose", text: "lo" });
    await probe(shown.view, { kind: "commit", text: "long" });
    await probe(shown.view, RETURN);
    await pause(QUIET_MS);
    assert.equal(turns(await probe(shown.view, READ)), base, "nothing typed with keys off started a turn");
    await invoke("desktop_native_terminal_keys", { view: shown.view, on: true });
    await probe(shown.view, RETURN);
    await seenWhen(shown.view, (seen) => turns(seen) === base + 1, UI_MS, "Return with keys on never started a turn");
    await probe(shown.view, RETURN);
    await seenWhen(shown.view, (seen) => turns(seen) === base + 2, SHORT_TURN_MS,
      "the first turn was not the short one: text typed with keys off reached the terminal");
    await probe(shown.view, { kind: "compose", text: "lo" });
    await probe(shown.view, { kind: "commit", text: "long" });
    await probe(shown.view, RETURN);
    await seenWhen(shown.view, (seen) => turns(seen) === base + 3, UI_MS, "the commit and Return with keys on never started a turn");
    await probe(shown.view, RETURN);
    await pause(SHORT_TURN_MS);
    assert.equal(turns(await probe(shown.view, READ)), base + 3, "with keys on the commit reached the terminal: its long turn holds the next Return");
    await close(shown.view);
  });

  it("a window the direct path refuses reopens fed by the engine and still draws", async () => {
    const target = `${SESSION}:=${master}`;
    const extra = tmux("split-window", "-d", "-P", "-F", "#{pane_id}", "-t", target, "/bin/sleep 600");
    assert.match(extra, /^%\d+$/);
    try {
      const shown = await open(true);
      const seen = await seenWhen(shown.view, (seen) => !seen.direct && seen.rows.some((row) => /stand-in/.test(row)),
        UI_MS, "the refused view never reopened fed by the engine");
      assert.equal(seen.direct, false);
      await close(shown.view);
    } finally {
      tmux("kill-pane", "-t", extra);
    }
  });

  it("an ended terminal closes the direct view", async () => {
    const shown = await open(true);
    assert.equal((await ready(shown)).direct, true);
    tmux("kill-window", "-t", `${SESSION}:=${master}`);
    await until(async () => (await events(shown.tag)).some((event) => event.type === "ended"), END_MS, "the view never ended");
    const ended = (await events(shown.tag)).find((event) => event.type === "ended");
    assert.equal(ended.code, "terminal_ended");
    opened.delete(shown.view);
    await assert.rejects(probe(shown.view, READ), /native_terminal_view_unknown/);
  });
});
