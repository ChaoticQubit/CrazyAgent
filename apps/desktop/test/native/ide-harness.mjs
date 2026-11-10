import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS, STEP_MS, bridgeCall, start as launch, value } from "./e2e-harness.mjs";

export const GIT = "/usr/bin/git";
export const FOLDER = "Project";
export const OWNER_NAME = "IDE Owner";
export const OWNER_EMAIL = "owner@example.invalid";
export const SETUP_MS = 120000;
export const BUILD_MS = 15 * 60 * 1000;
export const WORKBENCH_MS = 120000;
export const SETTLE_MS = 1500;
export const KEYS = { F1: ["F1", 112], Enter: ["Enter", 13], Escape: ["Escape", 27], ArrowLeft: ["ArrowLeft", 37], ArrowRight: ["ArrowRight", 39] };
export const IDE_FAILED = /didn't match its published checksum|couldn't be downloaded|couldn't start|stopped while it started|unavailable on/;

/** Runs git with no system or user config, so only the repository's own config counts.
 * Inputs: the folder and git's arguments. Output: git's trimmed stdout. */
export function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Starts the engine on its root and the end-to-end bridge on its socket. Inputs: the
 * binaries, the engine root, the environment and the evidence folder. Output: both children
 * and the bridge's port and token. */
export async function stack(engineBinary, bridgeBinary, runtime, env, evidence) {
  const { child: engine } = await launch(engineBinary,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, evidence, "engine", /^ready$/m);
  const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  return { engine, bridgeProcess: started.child, bridge: { port: Number(started.match[1]), token: started.match[2] } };
}

/** Makes the project an owner on the real engine: the owner profile, the projects home,
 * then the folder onboarded with the pool until setup settles. Inputs: the bridge, the
 * projects home, the repository, the pool and the generation counter. Output: the project. */
export async function onboard(bridge, projects, repo, pool, next) {
  assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
  assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: OWNER_NAME }, generation: next() })).outcome, "applied");
  assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: next() })).outcome, "applied");
  const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool, generation: next() });
  assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
  for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
    const view = await value(bridge, "desktop_onboarding", { id: receipt.project_id });
    if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
    assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
  }
  return receipt.project_id;
}

/** The answers for the IDE tab's native-only commands while the frontend runs in a browser:
 * an open asks the real engine through the bridge's `e2e_ide_open` and keeps the server it
 * opened; placing, hiding, closing and focus have no native webview to move here.
 * Inputs: the bridge and the list the opened servers go in. Output: the local answers. */
export function ideAnswers(bridge, opened) {
  const none = async () => ({ value: null });
  return {
    desktop_ide_open: async ({ projectId }) => {
      const answer = await bridgeCall(bridge, "e2e_ide_open", { projectId });
      if (!("value" in answer)) return answer;
      opened.push(answer.value);
      return { value: null };
    },
    desktop_ide_place: none,
    desktop_ide_hide: none,
    desktop_ide_close: none,
    desktop_ide_focus: none,
  };
}

/** Builds the workbench address the IDE tab loads: the server's loopback origin with the
 * project's workspace file and the session token read from its 0600 file, as the desktop
 * reads it. Input: the opened server. Output: the URL text. */
export function workbenchUrl(server) {
  assert.equal(statSync(server.token_path).mode & 0o777, 0o600, "the token file is private to the owner");
  const token = readFileSync(server.token_path, "utf8").trim();
  assert.match(token, /^[0-9a-f]{64}$/, "the token file holds one 256-bit token");
  assert.ok(isAbsolute(server.workspace) && existsSync(server.workspace), "the engine wrote the project's workspace file");
  const address = new URL(server.url);
  assert.equal(address.hostname, "127.0.0.1", "the IDE is reached on loopback only");
  address.pathname = "/";
  address.search = new URLSearchParams({ workspace: server.workspace, tkn: token }).toString();
  return address.toString();
}

/** Real keyboard and mouse input for a DevTools page: keys as the OS sends them, typed text as
 * one key press per character the way a keyboard types it (text with no keys is dropped by a
 * terminal still holding an earlier key down, IDE-TERM-FOCUS-TYPING), clicks and hovers at a
 * point. Input: the page. Output: the helpers; text() refuses control characters. */
export function input({ send }) {
  const key = async (name, modifiers = 0) => {
    const [code, keyCode] = KEYS[name] ?? [`Key${name}`, name.charCodeAt(0)];
    const pressed = KEYS[name] ? name : name.toLowerCase();
    for (const type of ["rawKeyDown", "keyUp"]) {
      await send("Input.dispatchKeyEvent", { type, key: pressed, code, windowsVirtualKeyCode: keyCode, modifiers });
    }
  };
  const text = async (value) => {
    if (/[\x00-\x1f\x7f]/.test(value)) throw new Error("type control characters with key(), not text()");
    for (const ch of value) {
      await send("Input.dispatchKeyEvent", { type: "keyDown", key: ch, text: ch, unmodifiedText: ch });
      await send("Input.dispatchKeyEvent", { type: "keyUp", key: ch });
    }
  };
  const hover = ({ x, y }) => send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
  const click = async ({ x, y }) => {
    for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
      await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: type === "mouseMoved" ? 0 : 1 });
    }
  };
  return { key, text, hover, click };
}

/** The workbench as the owner works it: waiting for what it shows, the F1 palette and its
 * rows, quick input rows, and the Source Control view's commit box and change groups.
 * Inputs: the page, its input helpers and the owner helpers. Output: the helpers. */
export function workbench(page, keys, me) {
  const until = async (what, expression, ms = STEP_MS) => {
    const deadline = Date.now() + ms;
    for (;;) {
      const seen = await page.evaluate(expression);
      if (seen) return seen;
      if (Date.now() > deadline) {
        await me.capture(`timeout-${what.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
        assert.fail(`the owner never saw ${what}`);
      }
      await pause(POLL_MS);
    }
  };
  const rows = `(() => {
    const widget = document.querySelector(".quick-input-widget");
    if (!widget || !widget.getClientRects().length || widget.style.display === "none") return null;
    return [...widget.querySelectorAll(".quick-input-list .monaco-list-row")].map((row) => (row.getAttribute("aria-label") ?? row.innerText).replace(/\\s+/g, " ").trim());
  })()`;
  const settled = async () => {
    let last = null;
    for (const deadline = Date.now() + STEP_MS; Date.now() < deadline; await pause(SETTLE_MS / 3)) {
      const now = await page.evaluate(rows);
      if (now !== null && last !== null && JSON.stringify(now) === JSON.stringify(last)) return now;
      last = now;
    }
    return last ?? [];
  };
  const palette = async (query) => {
    await keys.key("F1");
    await until("the F1 command palette", rows);
    await keys.text(query);
    await pause(SETTLE_MS);
    return settled();
  };
  const rowAt = (starts, has = "") => page.evaluate(`(() => {
    const row = [...document.querySelectorAll(".quick-input-widget .quick-input-list .monaco-list-row")].find((item) => {
      const label = (item.getAttribute("aria-label") ?? item.innerText).replace(/\\s+/g, " ").trim();
      return label.startsWith(${JSON.stringify(starts)}) && label.includes(${JSON.stringify(has)});
    });
    if (!row) return null;
    const rect = row.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  })()`);
  const pick = async (starts, has = "") => {
    const box = await rowAt(starts, has);
    if (!box) {
      await me.capture(`no-row-${starts.replace(/[^a-z0-9]+/gi, "-").slice(0, 30)}`);
      assert.fail(`no "${starts}" row; the list shows: ${JSON.stringify(await page.evaluate(rows))}`);
    }
    await keys.click(box);
  };
  const run = async (command) => {
    await palette(command);
    await pick(command);
  };
  const sidebar = (name) => `(() => {
    const title = document.querySelector(".part.sidebar .composite.title");
    return Boolean(title && title.getClientRects().length && ${name}.test(title.textContent));
  })()`;
  const commitBox = `(() => {
    const rows = [...document.querySelectorAll(".scm-view .monaco-list-row")];
    const named = rows.some((row) => row.querySelector(".scm-provider"));
    let current = null;
    for (const row of rows) {
      const name = row.querySelector(".scm-provider .monaco-icon-name-container")?.textContent?.trim();
      if (name) current = name;
      const editor = row.querySelector(".scm-editor");
      if (editor && editor.getClientRects().length && (!named || current === ${JSON.stringify(FOLDER)})) {
        const rect = editor.getBoundingClientRect();
        const text = [...editor.querySelectorAll(".view-line")].map((line) => line.textContent.replace(/\\u00a0/g, " ")).join("\\n");
        return { x: rect.left + 24, y: rect.top + rect.height / 2, text };
      }
    }
    return null;
  })()`;
  const group = (label) => `(() => {
    const row = [...document.querySelectorAll(".scm-view .monaco-list-row")].find((item) => item.querySelector(".resource-group") && item.innerText.includes(${JSON.stringify(label)}));
    if (!row) return null;
    const rect = row.getBoundingClientRect();
    const action = row.querySelector('.action-label[aria-label="Accept"], .action-label[title="Accept"]');
    const at = action?.getBoundingClientRect();
    return { row: { x: rect.left + rect.width / 3, y: rect.top + rect.height / 2 }, action: at && at.width > 0 ? { x: at.left + at.width / 2, y: at.top + at.height / 2 } : null };
  })()`;
  const dialogButton = (label) => `(() => {
    const button = [...document.querySelectorAll(".monaco-dialog-box .monaco-button")].find((item) => item.getClientRects().length && item.textContent.trim() === ${JSON.stringify(label)});
    if (!button) return null;
    const rect = button.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  })()`;
  return { until, rows, palette, pick, run, sidebar, commitBox, group, dialogButton };
}

/** Types a command list into the focused terminal, sends its output to a file in the project
 * and reads the file once the list has finished. Inputs: the workbench and input helpers, the
 * owner helpers and the project folder. Output: the function taking the file and commands,
 * which answers the file's text. */
export function terminalRunner({ ide, keys, me, repo }) {
  return async (file, commands) => {
    await ide.until("the terminal has the keyboard", `document.activeElement?.classList.contains("xterm-helper-textarea") === true`);
    await keys.text(`cd ${repo} && { ${commands}; echo finished; } > ${file} 2>&1`);
    await keys.key("Enter");
    const path = join(repo, file);
    for (const deadline = Date.now() + STEP_MS; !existsSync(path) || !readFileSync(path, "utf8").includes("finished"); await pause(POLL_MS)) {
      if (Date.now() > deadline) {
        await me.capture(`no-git-answer-${file}`);
        assert.fail(`no git answer in ${file}`);
      }
    }
    return readFileSync(path, "utf8");
  };
}
