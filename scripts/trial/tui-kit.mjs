import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { mask } from "./run.mjs";

const TMUX_MS = 10000;
const POLL_MS = 250;
const COLS = 160;
const ROWS = 48;
const WAIT_MS = 30000;
const FRAME = /^[a-z0-9-]{1,96}$/;
const KEY = /^[A-Za-z0-9\]-]{1,16}$/;
const SHIFT_ENTER = "\u001b[13;2u";

export const PLACES = ["project-master", "project-team"];

/** The tmux send-keys arguments for one key as the TUI keymap names it: "Enter", "Down",
 * "Esc", a letter, "Ctrl-K", "Shift-Tab", "Shift-Left" or "Shift-Enter", which goes out as
 * its CSI u sequence because a plain terminal sends Shift+Enter as Enter. Input: the name.
 * Output: the arguments after the target. */
export function keyArgs(name) {
  assert(typeof name === "string" && KEY.test(name), `a key name looks like Enter, l or Ctrl-K, got ${JSON.stringify(name)}`);
  if (name === "Shift-Enter") return ["-l", SHIFT_ENTER];
  if (name === "Shift-Tab") return ["BTab"];
  if (name === "Esc") return ["Escape"];
  if (name.length === 1) return ["-l", name];
  const ctrl = /^Ctrl-([A-Za-z\]])$/.exec(name);
  if (ctrl) return [`C-${ctrl[1].toLowerCase()}`];
  const shift = /^Shift-(Left|Right|Up|Down)$/.exec(name);
  if (shift) return [`S-${shift[1]}`];
  return [name];
}

/** Starts a program, the crazyagent TUI in a trial, in a tmux server of its own (tmux -L with a
 * fresh socket name and its own config), so no command can reach the owner's tmux or herdr
 * panes. Input: { argv with an absolute program, env for the program, evidence folder, the
 * project name go() opens, cols, rows, waitMs }. Output: the TUI kit { socket, type, key,
 * pane, waitFor, until, capture, go, stop }; frames are saved masked under tui4/, apart from
 * the runner's TUI recorder, which alone writes frames/. */
export async function startTui({ argv, env, evidence, project = "", cols = COLS, rows = ROWS, waitMs = WAIT_MS }) {
  assert(Array.isArray(argv) && argv.length > 0 && argv.every((part) => typeof part === "string"), "argv is a list of strings");
  assert(isAbsolute(argv[0]), `argv[0] must be an absolute path, got ${argv[0]}`);
  assert(isAbsolute(evidence), "the evidence folder is an absolute path");
  assert(Number.isInteger(waitMs) && waitMs > 0, "the wait limit is a positive number of ms");
  const socket = `ca-trial-tui-${process.pid}-${Date.now()}`;
  const frames = join(evidence, "tui4");
  const config = join(frames, "tmux.conf");
  mkdirSync(frames, { recursive: true, mode: 0o700 });
  writeFileSync(config, "set -g remain-on-exit on\nset -g history-limit 5000\nset -g status off\n", { mode: 0o600 });
  const tmuxEnv = { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: process.env.HOME ?? evidence, LANG: "en_US.UTF-8", TERM: "xterm-256color" };

  /** Runs one tmux command against the private server only. Input: tmux arguments. Output:
   * stdout; throws with tmux's stderr when it fails. */
  const tmux = (...args) => {
    const result = spawnSync("tmux", ["-L", socket, "-f", config, ...args], { encoding: "utf8", env: tmuxEnv, timeout: TMUX_MS, killSignal: "SIGKILL" });
    if (result.status !== 0) throw new Error(`tmux ${args[0]} failed: ${result.error?.message ?? result.stderr.trim()}`);
    return result.stdout;
  };
  const vars = Object.entries(env).flatMap(([name, value]) => ["-e", `${name}=${value}`]);
  tmux("new-session", "-d", "-s", "trial", "-x", String(cols), "-y", String(rows), ...vars, "--", ...argv);

  const pane = () => tmux("capture-pane", "-p", "-t", "trial");
  const type = async (value) => {
    assert(typeof value === "string" && value.length > 0, "type takes non-empty text");
    tmux("send-keys", "-t", "trial", "-l", "--", value);
  };
  const key = async (name) => { tmux("send-keys", "-t", "trial", ...keyArgs(name)); };
  const until = async (passes, timeout, label) => {
    let last = "";
    for (const deadline = Date.now() + timeout; Date.now() < deadline; await pause(POLL_MS)) {
      last = pane();
      if (passes(last)) return last;
    }
    throw new Error(`${label} within ${timeout / 1000} s; last pane:\n${mask(last.trimEnd().slice(-3000))}`);
  };
  const waitFor = (want, timeout = waitMs) => {
    assert((typeof want === "string" && want.length > 0) || want instanceof RegExp, "waitFor takes text or a RegExp");
    const passes = typeof want === "string" ? (text) => text.includes(want) : (text) => want.test(text);
    return until(passes, timeout, `the TUI never showed ${want}`);
  };
  const head = (text) => text.split("\n", 1)[0].trim().split(/\s{4,}/)[0];
  const crumb = (page) => (text) => head(text).endsWith(`/  ${project}  /  ${page}`);
  const atProject = (text) => head(text).endsWith(`/  ${project}`);
  const picked = (text = pane()) => {
    const line = text.split("\n").find((row) => /[▌|]/.test(row)) ?? "";
    const after = line.slice(line.search(/[▌|]/) + 1).replace(/^\s+/, "").trimEnd();
    return after === project || after.startsWith(`${project}  `);
  };
  const typing = (text = pane()) => /Enter\s+Send/.test(text.trimEnd().split("\n").at(-1));
  const go = async (place) => {
    assert(PLACES.includes(place), `go takes one of ${PLACES.join(", ")}, got ${place}`);
    assert(project.length > 0, "go needs the project name the TUI lists");
    await waitFor(project);
    await type("/");
    await waitFor("Clear filter");
    await type(project);
    await key("Enter");
    await until((text) => !/Clear filter/.test(text), waitMs, `${project} filter never applied`);
    for (let hops = 0; !picked(); hops += 1) {
      assert(hops < 64, `${project} is not selectable in the filtered Home list; a sibling such as ${project}-readd or ${project}0 must not be opened instead`);
      await key("Down");
      await pause(POLL_MS);
    }
    await key("Enter");
    await until(atProject, waitMs, `${project} never opened from Home`);
    await key("]");
    await until((text) => crumb("Conversation")(text) || crumb("Master")(text), waitMs, `${project} / Conversation never opened`);
    if (place === "project-team") {
      if (crumb("Master")(pane())) {
        await key("Ctrl-]");
        await key("Esc");
        await until(crumb("Conversation"), waitMs, `${project} / Master never went back to Conversation`);
      }
      await key(/Ctrl-T\s+Team/.test(pane()) ? "Ctrl-T" : "t");
      return until(crumb("Team"), waitMs, `${project} / Team never opened`);
    }
    if (crumb("Master")(pane())) return pane();
    if (!typing()) await key("Enter");
    await until(typing, waitMs, `the ${project} conversation input never took the keys`);
    await type("/master");
    await key("Enter");
    return until(crumb("Master"), waitMs, `${project} / Master never opened from /master`);
  };
  return {
    socket,
    /** Types text as the owner types it, literally. Input: the text. */
    type,
    /** Presses one key by its keymap name, such as Enter, Shift-Enter, Down, l or Ctrl-T. */
    key,
    /** The pane's text as the owner sees it now. Output: the text, unmasked. */
    pane,
    /** Waits until the pane shows text or matches a RegExp. Inputs: what to wait for and the
     * time limit in ms. Output: the pane text; throws with the last pane, masked. */
    waitFor,
    /** Waits until the pane's text passes a test. Inputs: the test, the time limit in ms and
     * the failure label. Output: the text; throws with the last pane, masked. */
    until,
    /** Opens a place in the project. It finds the project from Home by filtering the list to
     * its name (filter mode captures letters, so no letter of the name can trigger a Home
     * shortcut such as n for New project), then steps Down to the card the selection bar marks
     * as exactly this project, so a substring sibling such as the N-D15 re-add or a trailing
     * digit is never opened by mistake. It opens that card to Overview (whose crumbs are
     * Home / project, with no tab word), then steps right to the Conversation tab. project-
     * master then runs /master for the live master terminal; project-team opens the Team
     * screen. Input: the place. Output: the pane text once the header ends with the project
     * and the place; throws with the last pane. */
    go,
    /** Saves the pane, masked, as tui4/<name>.txt in the run folder. Input: the file stem.
     * Output: { path relative to the run folder, the masked text }. */
    capture: (name) => {
      assert(FRAME.test(name), `the frame name is a file stem, got ${name}`);
      const masked = mask(pane());
      writeFileSync(join(frames, `${name}.txt`), masked, { mode: 0o600 });
      return { path: `tui4/${name}.txt`, text: masked };
    },
    /** Ends the private tmux server and the program in it. */
    stop: async () => {
      spawnSync("tmux", ["-L", socket, "kill-server"], { env: tmuxEnv, timeout: TMUX_MS, killSignal: "SIGKILL" });
    },
  };
}
