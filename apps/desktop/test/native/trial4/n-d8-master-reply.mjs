import { setTimeout as pause } from "node:timers/promises";
import { click, shown } from "../settings-steps.mjs";
import { pasteTerminal } from "../trial-kit.mjs";

/** The first character of a reply line each live master harness draws on its terminal:
 * Claude Code's record dot and Codex's bullet. Keyed by catalog id and alias. */
export const REPLY_MARKS = { "claude-code": "\u23fa", claude: "\u23fa", codex: "\u2022" };

/** How long the owner waits for a master reply after a message (OP-1: 5 minutes). */
export const REPLY_MS = 300000;

/** How long one screen change may take before a check fails. */
export const UI_MS = 30000;

/** The WebDriver key codes for Shift and Enter. */
export const KEYS = { shift: "\uE008", enter: "\uE007" };

/** The live master terminal and its parts as the owner sees them. */
export const TERMINAL = ".pj-master-live .tg-terminal-screen .xterm";
export const BOX = ".pj-master-live textarea.tg-composer-input";
const SEND = "//div[contains(@class, \"pj-master-live\")]//form[contains(@class, \"tg-composer\")]//button[normalize-space(.)=\"Send\"]";
const START = "//div[contains(@class, \"pj-master-live\")]//button[normalize-space(.)=\"Start all\"]";
const RUNNING = ["starting", "idle", "working"];

/** How long the terminal gets between two reads after a paste or an Enter, in ms. */
export const READ_MS = 250;

/** How many reads a paste gets to draw and stop growing: 10 seconds at READ_MS. */
export const DRAW_READS_MAX = 40;

/** How many reads one Enter gets to take the paste off the terminal: 5 seconds at READ_MS. */
export const ENTER_READS_MAX = 20;

/** How many times Enter is pressed before a send fails: the first may only clear a notice. */
export const ENTER_PRESSES_MAX = 4;

/** Counts the reply lines a harness drew on the master terminal: rows that start with its
 * reply mark followed by text. Inputs: the terminal rows and the harness id. Output: the
 * count; throws for a harness with no known mark. */
export function replyLines(rows, harness) {
  const mark = REPLY_MARKS[harness];
  if (mark === undefined) throw new Error(`no reply mark is known for the harness ${harness}`);
  return rows.filter((row) => {
    const line = row.trimStart();
    return line.startsWith(mark) && line.slice(mark.length).trim() !== "";
  }).length;
}

/** Reads the master and its running agents from team graph nodes. A master with no agent row
 * or a status outside starting, idle and working has ended, as trial 3 reads it. Input: the
 * nodes. Output: { master, ended, working }. */
export function masterState(nodes) {
  const master = nodes.find((node) => node.kind === "master");
  const ended = !master || master.status == null || !RUNNING.includes(master.status);
  const working = nodes.filter((node) => node.kind !== "master" && RUNNING.includes(node.status)).length;
  return { master, ended, working };
}

/** Reads the message count from the Sessions bar's live session line. Input: the line.
 * Output: the count, 0 for a live session with no messages, null for no live session. */
export function messageCount(line) {
  if (/no messages yet/.test(line)) return 0;
  const found = /(\d+) messages?$/.exec(line.trim());
  return found ? Number(found[1]) : null;
}

/** Reads the rows of the live master terminal as text. Output: one string per row. */
export function masterRows() {
  return browser.execute(() => [...document.querySelectorAll(".pj-master-live .tg-terminal-screen .xterm-rows > div")]
    .map((row) => row.textContent.replace(/\u00a0/g, " ")));
}

/** Reads the Sessions bar's live session line. Output: its text, or "" when it is not shown. */
export async function liveLine() {
  return browser.execute(() => document.querySelector("[data-testid=master-live-session]")?.textContent ?? "");
}

/** Reads whether a project's team is stopped (Stop all) from the app's desktop_team_status
 * answer. Inputs: the answer and the project id. Output: the paused flag; throws when the read
 * failed, is another project's or has no flag, so a check never decides without the read. */
export function pausedOf(answer, id) {
  if (answer?.failed !== undefined) throw new Error(`desktop_team_status failed: ${answer.failed}`);
  if (answer?.project_id !== id) throw new Error(`desktop_team_status answered for ${JSON.stringify(answer?.project_id)}, not ${id}`);
  if (typeof answer.paused !== "boolean") throw new Error(`desktop_team_status answered no paused flag for ${id}`);
  return answer.paused;
}

/** Reads through the app's own command whether a project's team is stopped (Stop all). Input:
 * the project id. Output: the paused flag; throws when the engine answered no team state. */
export async function teamPaused(id) {
  const answer = await browser.executeAsync((id, done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_team_status", { id }).then(done, (failure) => done({ failed: String(failure) }));
  }, id);
  return pausedOf(answer, id);
}

/** Presses Start all in the master area when the engine's team read says the team is stopped,
 * after the master area offers it; a page drawn before its own team read never decides. Inputs:
 * the kit and the id of the project whose master area is open. Output: whether it pressed it. */
export async function startIfPaused(kit, id) {
  if (!(await teamPaused(id))) return false;
  await kit.until(() => shown(START), UI_MS, "the team is stopped, but the master area never offered Start all");
  await click(START);
  return true;
}

/** Reads a project's team graph through the app's own command. Input: the project id.
 * Output: the nodes; throws when the engine answered none. */
export async function teamNodes(id) {
  const answer = await browser.executeAsync((id, done) => {
    window.__TAURI_INTERNALS__.invoke("desktop_team_graph", { id }).then(done, (failure) => done({ failed: String(failure) }));
  }, id);
  const nodes = answer?.nodes ?? answer?.value?.nodes;
  if (!Array.isArray(nodes)) throw new Error(`desktop_team_graph answered no nodes: ${JSON.stringify(answer).slice(0, 300)}`);
  return nodes;
}

/** Waits until the master area shows the harness's own terminal with text on it and no chat
 * console. Inputs: the kit and the time limit in ms. Output: the number of rows with text. */
export async function terminalLive(kit, timeout) {
  let filled = 0;
  await kit.waitFor(async () => {
    if (await shown(".pj-convo")) throw new Error("the master area shows the chat console, not the harness's terminal");
    filled = (await masterRows()).filter((row) => row.trim() !== "").length;
    return { done: filled > 0 && await shown(TERMINAL), seen: `${filled} terminal rows with text` };
  }, timeout, "the master terminal never showed the harness's screen");
  return filled;
}

/** Lists the rows a paste drew: those with a letter or digit that differ from the rows before
 * it. A rule or box edge is left out, as the next box edge can land on its row once the message
 * is sent. Inputs: the rows before and after the paste. Output: [{ at, row }]. */
export function drawnRows(before, after) {
  return after.flatMap((row, at) => (row !== before[at] && /[\p{L}\p{N}]/u.test(row) ? [{ at, row }] : []));
}

/** Says whether a drawn paste still sits on the terminal: any of its rows reads as it did.
 * Inputs: its rows and the terminal's rows now. Output: a boolean. */
export function pasteWaiting(drawn, rows) {
  return drawn.some(({ at, row }) => rows[at] === row);
}

/** Waits until a paste has drawn and stopped changing: the rows that differ from before it and
 * held still since the last read are the same on two reads in a row, so an animated row neither
 * holds this up nor counts. Inputs: the terminal and the rows before the paste. Output: the
 * drawn rows; throws when none held still, so no Enter is pressed on an unseen paste. */
async function steadyPaste(terminal, before) {
  let last = before;
  let held = [];
  for (let reads = 0; reads < DRAW_READS_MAX; reads += 1) {
    await terminal.pause();
    const rows = await terminal.rows();
    const kept = drawnRows(before, rows).filter(({ at, row }) => last[at] === row);
    if (kept.length > 0 && JSON.stringify(kept) === JSON.stringify(held)) return kept;
    held = kept;
    last = rows;
  }
  throw new Error("the paste never drew on the master terminal: Enter was not pressed");
}

/** Pastes a message and presses Enter until its turn has started: a long paste followed at once
 * by Enter can sit unsent, and a first Enter can only clear a notice. The turn has started when
 * the caller's read says so or, with no such read, once the drawn paste left the terminal.
 * Inputs: the terminal, { rows, paste, enter, pause }, and the read for a terminal that draws no
 * echo. Output: how many times Enter was pressed; throws when the paste stayed unsent. */
export async function pasteAndSend(terminal, started) {
  const before = await terminal.rows();
  await terminal.paste();
  const drawn = started ? [] : await steadyPaste(terminal, before);
  const begun = started ?? (async () => !pasteWaiting(drawn, await terminal.rows()));
  for (let presses = 1; presses <= ENTER_PRESSES_MAX; presses += 1) {
    await terminal.enter();
    for (let reads = 0; reads < ENTER_READS_MAX; reads += 1) {
      await terminal.pause();
      if (await begun()) return presses;
    }
  }
  throw new Error(`the paste stayed on the master terminal after ${ENTER_PRESSES_MAX} Enter presses: it was not sent`);
}

/** Sends one message to the live master as the owner does: through the message box when the
 * Terminal input setting shows it, else pasted into the terminal and sent with Enter, so a new
 * line in the text never presses Enter. Inputs: the kit, the text and, for a terminal that
 * draws no echo, a read of whether the turn started. Output: which input it used. */
export async function sendToMaster(kit, text, started) {
  if (await shown(BOX)) {
    await kit.paste(BOX, text);
    await kit.until(async () => {
      if ((await $(BOX).getValue()) !== text) await kit.paste(BOX, text);
      return $(SEND).isEnabled();
    }, UI_MS, "the master's Send button stayed off with the text in the box");
    await click(SEND);
    await kit.until(async () => (await $(BOX).getValue()) === "", UI_MS, "the message box kept the text: it was not sent");
    return "message box";
  }
  await $(TERMINAL).click();
  await pasteAndSend({
    rows: masterRows,
    paste: async () => {
      if (!(await browser.execute(pasteTerminal, text))) throw new Error("the master terminal lost the focus before the paste");
    },
    enter: () => browser.keys(KEYS.enter),
    pause: () => pause(READ_MS),
  }, started);
  return "terminal";
}

export const checks = [{
  id: "N-D8", op: ["OP-1"], title: "The master replies to the trial prompt and stays live while its agents start",
  why: "owner: \"The master conversation gives no reply. It shows starting up forever or The master stopped\"",
  after: "send-prompt",
  run: async (kit) => {
    await kit.tab("conversation");
    const begun = Date.now();
    let replies = 0;
    let alert = false;
    let harness = "unknown";
    await kit.waitFor(async () => {
      const state = masterState((await kit.graph()).nodes);
      harness = state.master?.harness ?? "unknown";
      replies = state.master ? replyLines(await masterRows(), harness) : 0;
      alert = await shown(".pj-master-live [role=alert]");
      return { done: replies > 0 || alert, seen: `${replies} reply lines from ${harness}; master ${state.master?.status ?? "missing"}${alert ? "; the master area shows an alert" : ""}` };
    }, REPLY_MS, "the master drew no reply on its terminal");
    await kit.screen();
    if (replies === 0) throw new Error(`the master area shows an alert and no reply from ${harness}`);
    const answered = Math.round((Date.now() - begun) / 1000);
    let last = null;
    await kit.waitFor(async () => {
      last = masterState((await kit.graph()).nodes);
      return { done: last.ended || last.working > 0, seen: `master ${last.master?.status ?? "missing"}, ${last.working} agents running` };
    }, kit.team(), "no agent of the master started");
    if (last.ended) throw new Error(`the master stopped (${last.master?.status ?? "no master node"}) before its agents ran`);
    await kit.screen();
    return `${harness} master drew ${replies} reply lines ${answered} s after the prompt step; it was ${last.master.status} while ${last.working} of its agents ran`;
  },
}];
