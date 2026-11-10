import { shown } from "../settings-steps.mjs";
import { KEYS, REPLY_MARKS, REPLY_MS, TERMINAL, UI_MS, masterRows, masterState, startIfPaused, terminalLive } from "./n-d8-master-reply.mjs";

/** The two lines the owner types into the master terminal, short enough to stay on one row. */
export const LINES = ["Trial 4 check N-D10, first line.", "Second line: reply with the single word ok."];

/** Reads whether two typed lines reached the harness as one message that it answered: the
 * second line sits under the last first line with no reply between them, and a reply follows.
 * A reply between the lines means Enter sent the first line alone, which no later screen can
 * undo. Inputs: the terminal rows, the two lines and the harness id. Output: { done, ok, seen }. */
export function oneMessage(rows, [first, second], harness) {
  const mark = REPLY_MARKS[harness];
  if (mark === undefined) throw new Error(`no reply mark is known for the harness ${harness}`);
  const replied = (row) => row.trimStart().startsWith(mark);
  const top = rows.findLastIndex((row) => row.includes(first));
  if (top < 0) return { done: false, ok: false, seen: "the first line is not on the terminal" };
  const under = rows.findIndex((row, at) => at > top && row.includes(second));
  if (under < 0) {
    const early = rows.slice(top + 1).some(replied);
    return early
      ? { done: true, ok: false, seen: "the harness answered the first line alone: Shift+Enter sent it" }
      : { done: false, ok: false, seen: "the second line is not under the first" };
  }
  if (rows.slice(top + 1, under).some(replied)) return { done: true, ok: false, seen: "a reply sits between the two lines: they arrived as two messages" };
  if (!rows.slice(under + 1).some(replied)) return { done: false, ok: false, seen: "both lines are one message; no reply under it yet" };
  return { done: true, ok: true, seen: "both lines arrived as one message and the harness answered it" };
}

export const checks = [{
  id: "N-D10", op: ["OP-12"], title: "The master terminal is the harness's own screen; Shift+Enter adds a line and Enter sends",
  why: "owner: \"The master conversation is a chat form, not a terminal. Input is broken, and Enter and Shift-Enter do not behave as in a terminal\"",
  after: "hidden-folders",
  run: async (kit) => {
    await kit.tab("conversation");
    const started = await startIfPaused(kit, kit.project());
    await terminalLive(kit, REPLY_MS);
    const { master } = masterState((await kit.graph()).nodes);
    if (!master) throw new Error("the team graph has no master");
    if (!["claude-code", "claude"].includes(master.harness)) throw new Error(`the master runs ${master.harness}; this check needs the Claude Code master the trial chooses`);
    if (await shown(".pj-convo")) throw new Error("the master area shows the chat console, not the harness's terminal");
    await $(TERMINAL).click();
    await browser.keys(LINES[0]);
    await browser.keys([KEYS.shift, KEYS.enter]);
    await browser.keys(LINES[1]);
    await kit.until(async () => (await masterRows()).some((row) => row.includes(LINES[1])), UI_MS, "the second typed line never showed on the terminal");
    await browser.keys(KEYS.enter);
    let verdict = null;
    await kit.waitFor(async () => {
      verdict = oneMessage(await masterRows(), LINES, master.harness);
      return { done: verdict.done, seen: verdict.seen };
    }, REPLY_MS, "the two typed lines got no answer as one message");
    await kit.screen();
    if (!verdict.ok) throw new Error(verdict.seen);
    return `${started ? "Start all, then " : ""}typed two lines into the ${master.harness} terminal with Shift+Enter between them and Enter after: ${verdict.seen}`;
  },
}];
