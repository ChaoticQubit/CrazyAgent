import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { click } from "../settings-steps.mjs";
import { RUNNING, TURN_MS, UI_MS, cardOf, copyRow, fromRows, inTurn, moved, openCard, paired, readCards, rowTexts, selectRow, terminalNote, terminalView, turnUp, wheel } from "../agents-cards.mjs";

const STEP_PX = 120;
const PAIR_TRIES = 3;
const COPIED = "Copied the selection.";
const REFUSED = "The clipboard refused the copy.";
const PASTE_MS = 5000;

/** The XPath of a button inside one agent's terminal. Inputs: the agent's label and the
 * button's path under the terminal section. Output: the selector. */
function inTerminal(label, path) {
  assert(!label.includes('"'), "an agent label has no double quote");
  return `//section[@aria-label="${label} terminal"]${path}`;
}

/** Opens the first running sub-agent's terminal, else a lead's, that shows live rows.
 * Input: the kit. Output: the agent's label; throws naming each card tried. */
async function liveTerminal(kit) {
  const { nodes } = await kit.graph();
  const cards = await readCards();
  const tried = [];
  for (const kind of ["sub", "lead"]) {
    for (const node of nodes.filter((item) => item.kind === kind && RUNNING.includes(item.status) && cardOf(cards, item)?.opens)) {
      await openCard(node.label);
      try {
        await kit.until(async () => {
          const view = await terminalView(node.label);
          return Boolean(view?.live && view.rows.trim());
        }, UI_MS, `${node.label}'s terminal showed no live rows`);
        return node.label;
      } catch (failure) {
        tried.push(`${node.label}: ${(await terminalView(node.label))?.state.trim() || failure.message.split(";")[0]}`);
      }
    }
  }
  throw new Error(`no running sub-agent or lead terminal opened live: ${tried.join("; ") || "no running card opens a terminal"}`);
}

/** Turns the wheel over a terminal once, through the app's own wheel route. Inputs: the
 * agent's label and the signed step. Throws when the screen is not drawn. */
async function turn(label, step) {
  if (!await wheel(label, step)) throw new Error(`${label}'s terminal screen was not drawn for the wheel`);
}

/** One wheel turn up and back down over an agent that reads the mouse: the turn up must move
 * a block of rows down from the read just before it within TURN_MS, and the turn down must
 * put that block back on its row within TURN_MS. Inputs: the kit and the agent's label.
 * Output: { took, paired }, the milliseconds the turn up took and whether the turn down
 * reversed it; throws when the turn up moved nothing. */
async function fullscreenPair(kit, label) {
  const reads = [await terminalView(label)];
  const started = Date.now();
  await turn(label, -STEP_PX);
  let found = null;
  let took = null;
  await kit.until(async () => {
    const view = await terminalView(label);
    const at = Date.now();
    if (!view) return false;
    reads.push(view);
    found = inTurn(started, at) ? turnUp(reads) : null;
    if (found) took = at - started;
    return found !== null;
  }, TURN_MS, `${label}'s terminal did not scroll within ${TURN_MS} ms of turning the wheel up (the agent reads the mouse, so its own screen had to move)`);
  const down = Date.now();
  await turn(label, STEP_PX);
  try {
    await kit.until(async () => {
      const view = await terminalView(label);
      return Boolean(view && paired(found.anchor, found.up, view) && inTurn(down, Date.now()));
    }, TURN_MS, "unpaired");
    return { took, paired: true };
  } catch {
    return { took, paired: false };
  }
}

/** Turns the wheel up over a terminal and times how long until it scrolls, then turns it
 * back down and returns to the latest output. An agent that reads the mouse redraws its own
 * screen, which its own redraws can also move, so its scroll counts only when a turn down puts
 * the rows a turn up moved back on their row; output that lands while the agent takes the
 * wheel can break one pair, so up to PAIR_TRIES pairs are turned. Inputs: the kit and the
 * agent's label. Output: { took, mouse, tries }, the milliseconds the paired turn up took,
 * whether the agent read the mouse and how many pairs it took. */
async function scroll(kit, label) {
  const before = await terminalView(label);
  if (before.mouse) {
    for (let tries = 1; tries <= PAIR_TRIES; tries += 1) {
      const pair = await fullscreenPair(kit, label);
      if (pair.paired) return { took: pair.took, mouse: true, tries };
    }
    throw new Error(`${label}'s terminal moved its rows down after each of ${PAIR_TRIES} wheel turns up but never put them back on their row within ${TURN_MS} ms of the wheel turned down, so each move up was its own redraw`);
  }
  const started = Date.now();
  await turn(label, -STEP_PX);
  let took = null;
  await kit.until(async () => {
    const after = await terminalView(label);
    const at = Date.now();
    if (after && moved(before, after) && inTurn(started, at)) took = at - started;
    return took !== null;
  }, TURN_MS, `${label}'s terminal did not scroll within ${TURN_MS} ms of turning the wheel up`);
  await turn(label, STEP_PX);
  if ((await terminalView(label))?.jump) await click(inTerminal(label, '//button[normalize-space(.)="Jump to latest"]'));
  await kit.until(async () => !(await terminalView(label))?.jump, UI_MS, `${label}'s terminal did not return to the latest output`);
  return { took, mouse: false, tries: 1 };
}

/** Selects the newest row of a terminal, copies it from the right-click menu and reads the
 * system clipboard back. Inputs: the kit and the agent's label. Output: how many characters
 * the clipboard took. */
async function copy(kit, label) {
  const index = copyRow(await rowTexts(label));
  if (index < 0) throw new Error(`${label}'s terminal has no row with text to select`);
  const picked = await selectRow(label, index);
  if (!picked) throw new Error(`${label}'s terminal row ${index} was not drawn`);
  await click(inTerminal(label, '//div[@role="menu"][@aria-label="Terminal"]/button[normalize-space(.)="Copy"]'));
  let note = "";
  await kit.until(async () => {
    note = (await terminalNote(label)).trim();
    return note === COPIED || note === REFUSED;
  }, UI_MS, `${label}'s terminal did not say whether the copy worked`);
  if (note === REFUSED) throw new Error(`${label}'s terminal says: ${REFUSED}`);
  await kit.screen();
  const clip = execFileSync("/usr/bin/pbpaste", { encoding: "utf8", timeout: PASTE_MS });
  if (!fromRows(picked.row, clip) || !fromRows(clip, picked.rows.join(""))) {
    throw new Error(`the clipboard holds ${clip.length} characters, not the selected row of ${label}'s terminal`);
  }
  return clip.length;
}

export const checks = [{
  id: "N-D9",
  op: ["OP-15"],
  title: "A live agent terminal scrolls at once with the wheel and copies a selected row to the clipboard",
  why: "owner: \"I can't even copy from the Claude Goat terminal harness, the master harness\"; \"the scrolling of the sub agent terminal is also super slow\"",
  after: "question",
  run: async (kit) => {
    await kit.tab("agents");
    const label = await liveTerminal(kit);
    const { took, mouse, tries } = await scroll(kit, label);
    const chars = await copy(kit, label);
    return `${label}'s live terminal scrolled ${took} ms after the wheel turned${mouse ? ` (the agent reads the mouse and redrew its own screen; the turn down put its rows back on pair ${tries} of ${PAIR_TRIES})` : ""}; Copy put the selected row on the clipboard (${chars} characters)`;
  },
}];
