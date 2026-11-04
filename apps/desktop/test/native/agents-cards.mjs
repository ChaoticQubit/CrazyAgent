import assert from "node:assert/strict";
import { click } from "./settings-steps.mjs";

/** The longest an agents check waits for one screen change, in ms. */
export const UI_MS = 30000;

/** The engine statuses of an agent that still runs. */
export const RUNNING = ["starting", "idle", "working"];

const STATUS_WORDS = { starting: "Starting", idle: "Idle", working: "Working", stopped: "Stopped", failed: "Failed" };
const NO_TASK = "No task yet.";
const ROLE_NAME = /^[^()@]+ \(@[^()\s]+\)$/;
const HARNESS_NAME = /^(claude code|claude|codex|gemini|opencode|cursor)\b/i;
const NOT_SHOWN = /(\d+) agents? not shown\./;
const EDGE_PX = 1;
const COPY_CHARS = 3;
const CONNECTING = "Connecting to the terminal";
const WHEEL_TICKS = 3;
const BLOCK_ROWS = 3;

/** The status word a node's card names, as the app picks it: the engine's status label,
 * else the fixed word for the status. Input: a graph node. Output: the word, empty when the
 * node has neither. */
export function statusWord(node) {
  return node.status_label?.trim() || STATUS_WORDS[node.status] || "";
}

/** The card a graph node is drawn as: the card whose button label starts with the node's
 * label and a comma or period. Inputs: the cards readCards read and the node. Output: the
 * card, or undefined when none is drawn. */
export function cardOf(cards, node) {
  return cards.find((card) => card.label.startsWith(`${node.label},`) || card.label.startsWith(`${node.label}.`));
}

/** Whether a card shows a current task: its first task line, trimmed, is neither empty nor
 * the no-task line. Input: a card. Output: a boolean. */
export function showsTask(card) {
  const first = card.task[0]?.trim() ?? "";
  return first !== "" && first !== NO_TASK;
}

/** The pairs of cards whose boxes share more than a pixel each way; cards that only touch
 * do not count. Input: the cards. Output: the pairs of titles, in page order. */
export function overlapping(cards) {
  const pairs = [];
  for (let i = 0; i < cards.length; i += 1) {
    for (let j = i + 1; j < cards.length; j += 1) {
      const [a, b] = [cards[i], cards[j]];
      const wide = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
      const tall = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
      if (wide > EDGE_PX && tall > EDGE_PX) pairs.push([a.title, b.title]);
    }
  }
  assert(pairs.length <= (cards.length * (cards.length - 1)) / 2);
  return pairs;
}

/** The text boxes that leave their card by more than a pixel on any side; empty boxes are
 * skipped. Input: the cards. Output: { card, text } for each, in page order. */
export function spilled(cards) {
  const out = [];
  for (const card of cards) {
    for (const clip of card.clips) {
      if (clip.w === 0 && clip.h === 0) continue;
      const left = clip.x < card.x - EDGE_PX || clip.y < card.y - EDGE_PX;
      const right = clip.x + clip.w > card.x + card.w + EDGE_PX || clip.y + clip.h > card.y + card.h + EDGE_PX;
      if (left || right) out.push({ card: card.title, text: clip.text });
    }
  }
  return out;
}

/** Whether an agent's name is a persona with its @role name, such as "Ada (@lead-a)", and
 * not a harness name with a number, such as "Claude Code 1". Input: the label. Output: a
 * boolean. */
export function roleName(label) {
  const name = label.trim();
  return ROLE_NAME.test(name) && !HARNESS_NAME.test(name);
}

/** How many agents the graph line says the drawing leaves out. Input: the graph line's
 * text. Output: the count, 0 when the line names none. */
export function notShown(summary) {
  const found = NOT_SHOWN.exec(summary);
  return found ? Number(found[1]) : 0;
}

/** How far the screen's rows moved between two reads: the first block of BLOCK_ROWS non-blank
 * rows that each read holds exactly once and that changed place, compared without trailing
 * spaces. Inputs: the row texts before and after, top first. Output: the rows it moved down,
 * negative when up, 0 when no such block moved. */
export function redrawShift(before, after) {
  assert(Array.isArray(before) && Array.isArray(after), "a redraw compares two lists of row texts");
  const [was, now] = [before, after].map((rows) => rows.map((row) => row.replace(/\s+$/, "")));
  const places = (rows, block) => {
    const found = [];
    for (let at = 0; at + BLOCK_ROWS <= rows.length; at += 1) if (rows.slice(at, at + BLOCK_ROWS).join("\n") === block) found.push(at);
    return found;
  };
  for (let at = 0; at + BLOCK_ROWS <= was.length; at += 1) {
    const rows = was.slice(at, at + BLOCK_ROWS);
    if (rows.some((row) => row.trim() === "")) continue;
    const block = rows.join("\n");
    if (places(was, block).length !== 1) continue;
    const there = places(now, block);
    if (there.length === 1 && there[0] !== at) return there[0] - at;
  }
  return 0;
}

/** Whether a wheel turn scrolled a terminal view. An agent that reads the mouse, such as a
 * fullscreen TUI, gets the wheel and redraws its own screen, so its rows must move the wheel's
 * way: down for a turn up. Otherwise the offset must change or Jump to latest appear; new rows
 * alone never count, since a live agent prints them on its own. Inputs: the views before and
 * after and whether the turn was up. Output: a boolean. */
export function moved(before, after, up) {
  assert.equal(typeof up, "boolean", "a wheel turn is up or down");
  if (before.mouse) {
    const shift = redrawShift(before.lines, after.lines);
    return up ? shift > 0 : shift < 0;
  }
  return before.top !== after.top || (!before.jump && after.jump);
}

/** Whether copied text came from a terminal's rows: it is not blank and, with whitespace
 * collapsed on both sides, part of them. Inputs: the clipboard text and the rows text.
 * Output: a boolean. */
export function fromRows(copied, rows) {
  const flat = (value) => value.replace(/\s+/g, " ").trim();
  return flat(copied) !== "" && flat(rows).includes(flat(copied));
}

/** Reads every card the Agents tab draws: its kind, button label, title, task lines, whether
 * it opens a terminal, its box and the boxes of its clipped text, in window pixels.
 * Output: the cards in page order. */
export function readCards() {
  return browser.execute(() => [...document.querySelectorAll(".tg-card")].filter((card) => card.getClientRects().length).map((card) => {
    const box = card.getBoundingClientRect();
    const clips = [...card.querySelectorAll(".tg-clip")].map((clip) => {
      const at = clip.getBoundingClientRect();
      return { text: clip.textContent, x: at.left, y: at.top, w: at.width, h: at.height };
    });
    return {
      kind: ["master", "lead", "sub", "teammate"].find((kind) => card.classList.contains(kind)) ?? "",
      label: card.querySelector(".tg-card-open")?.getAttribute("aria-label") ?? "",
      title: card.querySelector(".tg-card-title .tg-clip")?.textContent ?? "",
      task: [...card.querySelectorAll(".tg-card-task .tg-clip")].map((line) => line.textContent),
      opens: !card.classList.contains("static"),
      x: box.left, y: box.top, w: box.width, h: box.height, clips,
    };
  }));
}

/** Reads the graph line under the Team graph title. Output: its text, empty when the graph
 * is not shown. */
export function summary() {
  return browser.execute(() => document.querySelector(".tg-summary")?.textContent ?? "");
}

/** Reads one agent's terminal view as the owner sees it: the state line, the rows xterm
 * draws as text and one by one, the scroll offset, whether it is live, whether Jump to latest
 * shows and whether the agent reads the mouse. Input: the agent's label. Output: the view,
 * or null while none is shown. */
export function terminalView(label) {
  return browser.execute((label) => {
    const section = [...document.querySelectorAll("section.tg-terminal")]
      .find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    if (!section) return null;
    const jump = [...section.querySelectorAll("button")].some((button) => button.textContent.trim() === "Jump to latest");
    return {
      state: section.querySelector(".tg-terminal-state")?.innerText ?? "",
      rows: section.querySelector(".xterm-rows")?.innerText ?? "",
      lines: [...section.querySelectorAll(".xterm-rows > div")].map((row) => row.innerText),
      top: section.querySelector(".xterm-viewport")?.scrollTop ?? 0,
      mouse: Boolean(section.querySelector(".xterm.enable-mouse-events")),
      live: Boolean(section.querySelector(".tg-terminal-note")),
      jump,
    };
  }, label);
}

/** Opens an agent's terminal from its card and waits until it has connected and shows rows
 * or says why it has none. Inputs: the kit and the agent's label. Output: the view. */
export async function shownView(kit, label) {
  await openCard(label);
  let view = null;
  await kit.until(async () => {
    view = await terminalView(label);
    return Boolean(view && !view.state.includes(CONNECTING) && (view.rows.trim() || view.state.trim()));
  }, UI_MS, `${label}'s terminal did not open from its card`);
  return view;
}

/** Unfolds the Agents tab's Finished fold-out when it is folded, so its Wake buttons can be
 * pressed. Input: the kit. */
export async function openFinished(kit) {
  const folded = await browser.execute(() => document.querySelector('details[aria-label="Finished agents"]')?.open === false);
  if (folded) await click('//details[@aria-label="Finished agents"]/summary');
  await kit.until(() => browser.execute(() => document.querySelector('details[aria-label="Finished agents"]')?.open === true), UI_MS, "the Finished fold-out did not unfold");
}

/** Opens an agent's terminal as the owner does, by clicking its card. Input: the agent's
 * label, without a double quote. */
export async function openCard(label) {
  assert(!label.includes('"'), "an agent label has no double quote");
  await click(`//div[contains(concat(" ", normalize-space(@class), " "), " tg-card ")]/button[starts-with(@aria-label, "${label},")]`);
}

/** The Finished list row of one agent: the row whose name, trimmed, is the agent's label,
 * so a longer name sharing its start never matches. Inputs: the rows finishedList read and
 * the label. Output: the row, or undefined when the list does not hold the agent. */
export function finishedRow(rows, label) {
  return rows.find((row) => row.name.trim() === label.trim());
}

/** The terminal row a copy check selects: the newest row with at least COPY_CHARS visible
 * characters, so the copied text is long enough to find in the rows. Input: the row texts,
 * top first. Output: its index, -1 when no row qualifies. */
export function copyRow(rows) {
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    if (rows[index].replace(/\s+/g, "").length >= COPY_CHARS) return index;
  }
  return -1;
}

/** Reads the Agents tab's Finished fold-out: its toggle text and each row's name, harness,
 * ended line, whether Wake is enabled and Wake's note. Output: { label, rows }, label empty
 * and no rows while the fold-out is not drawn. */
export function finishedList() {
  return browser.execute(() => {
    const box = document.querySelector('details[aria-label="Finished agents"]');
    if (!box) return { label: "", rows: [] };
    return {
      label: box.querySelector("summary strong")?.textContent ?? "",
      rows: [...box.querySelectorAll('ul[aria-label="Finished agents"] > li')].map((row) => {
        const wake = [...row.querySelectorAll("button")].find((button) => ["Wake", "Waking..."].includes(button.textContent.trim()));
        const kind = row.querySelector(".pj-kind");
        return {
          name: [...(kind?.childNodes ?? [])].filter((node) => node.nodeType === Node.TEXT_NODE).map((node) => node.textContent).join("").trim(),
          harness: kind?.querySelector(".pj-small")?.textContent ?? "",
          ended: row.querySelector(".pj-row .pj-muted")?.textContent ?? "",
          wakeable: Boolean(wake && !wake.disabled),
          note: wake?.title ?? "",
        };
      }),
    };
  });
}

/** Reads the text of each row an agent's terminal draws, top first. Input: the agent's
 * label. Output: the row texts, empty while no terminal is drawn. */
export function rowTexts(label) {
  return browser.execute((label) => {
    const section = [...document.querySelectorAll("section.tg-terminal")]
      .find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    return [...(section?.querySelectorAll(".xterm-rows > div") ?? [])].map((row) => row.innerText);
  }, label);
}

/** Selects one row of an agent's terminal as the owner does on a Mac: an Option triple-click
 * on it, which selects the line even while the agent reads the mouse, then opens the
 * terminal's right-click menu on that row. Inputs: the agent's label and the row's index.
 * Output: { row, rows }, the row's text and every row's text then, or null when the
 * terminal or the row is not drawn. */
export function selectRow(label, index) {
  return browser.execute((label, index) => {
    const section = [...document.querySelectorAll("section.tg-terminal")]
      .find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    const row = section?.querySelectorAll(".xterm-rows > div")[index];
    const screen = section?.querySelector(".xterm-screen");
    if (!row || !screen) return null;
    const box = row.getBoundingClientRect();
    const at = { clientX: box.left + 4, clientY: box.top + box.height / 2, bubbles: true, cancelable: true, view: window };
    screen.dispatchEvent(new MouseEvent("mousedown", { ...at, button: 0, buttons: 1, detail: 3, altKey: true }));
    document.dispatchEvent(new MouseEvent("mouseup", { ...at, button: 0, buttons: 0, detail: 3, altKey: true }));
    screen.dispatchEvent(new MouseEvent("contextmenu", { ...at, button: 2, buttons: 2 }));
    return { row: row.innerText, rows: [...section.querySelectorAll(".xterm-rows > div")].map((line) => line.innerText) };
  }, label, index);
}

/** Turns an agent's terminal with the mouse wheel as the owner does: WHEEL_TICKS wheel
 * events over its screen, up for a negative step and down for a positive one. Inputs: the
 * agent's label and the step in pixels. Output: whether the screen was drawn. */
export function wheel(label, step) {
  assert(Number.isInteger(step) && step !== 0, "a wheel step is a whole, nonzero pixel count");
  return browser.execute((label, step, ticks) => {
    const section = [...document.querySelectorAll("section.tg-terminal")]
      .find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    const screen = section?.querySelector(".xterm-screen");
    if (!screen) return false;
    const box = screen.getBoundingClientRect();
    for (let tick = 0; tick < ticks; tick += 1) {
      screen.dispatchEvent(new WheelEvent("wheel", { deltaY: step, deltaMode: 0, clientX: box.left + box.width / 2, clientY: box.top + box.height / 2, bubbles: true, cancelable: true, view: window }));
    }
    return true;
  }, label, step, WHEEL_TICKS);
}

/** Reads an agent's terminal status line, where a copy reports how it went. Input: the
 * agent's label. Output: the line's text, empty while the terminal is not live. */
export function terminalNote(label) {
  return browser.execute((label) => {
    const section = [...document.querySelectorAll("section.tg-terminal")]
      .find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    return section?.querySelector(".tg-terminal-note [role=status]")?.textContent ?? "";
  }, label);
}
