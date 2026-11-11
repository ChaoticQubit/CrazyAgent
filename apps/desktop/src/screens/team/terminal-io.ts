import type { IFunctionIdentifier, IParser } from "@xterm/xterm";
import type { TerminalMessage } from "../../engine-client";

export const INPUT_BYTES_MAX = 4096;
export const SIZE_MIN = 2;
export const SIZE_MAX = 1000;
export const QUEUED_CHUNKS_MAX = 256;

/** Where the owner types to a master or an agent: straight into its terminal, only in the
 * message box under it, or in both. */
export type InputChoice = "terminal" | "box" | "both";
export const INPUT_DEFAULT: InputChoice = "terminal";
export const INPUT_CHOICES: { value: InputChoice; label: string }[] = [
  { value: "terminal", label: "Terminal only" },
  { value: "box", label: "Message box only" },
  { value: "both", label: "Both" },
];

/** Reads the stored Terminal input choice, which every terminal follows. Input: the stored
 * text, if any. Output: the choice, or `INPUT_DEFAULT` for anything else. */
export function inputChoice(stored: string | null): InputChoice {
  const known = INPUT_CHOICES.find((choice) => choice.value === stored);
  return known ? known.value : INPUT_DEFAULT;
}

/** What one input choice turns on. Input: the choice. Output: whether keys reach the
 * terminal and whether the message box shows. */
export function inputMode(choice: InputChoice): { keys: boolean; box: boolean } {
  return { keys: choice !== "box", box: choice !== "terminal" };
}

const encoder = new TextEncoder();

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Encodes what the owner typed, as xterm's data event gives it, into the UTF-8 bytes the
 * agent's terminal reads; control keys and escape sequences pass through unchanged.
 * Input: the typed text. Output: the bytes. */
export function encodeTyped(data: string): Uint8Array {
  return encoder.encode(data);
}

/** The newline key both Claude Code and Codex read as a new line in the message being typed,
 * in every terminal: Ctrl+J. */
export const NEW_LINE = "\n";

/** What a key press means before xterm sees it. Shift+Enter is a new line in the agent's
 * message rather than a second Enter, so its keydown types `NEW_LINE` and every event of it
 * is kept from xterm; any other key goes to xterm unchanged. Input: the key event's type,
 * key and modifiers. Output: the text to type and false to keep it from xterm, nothing to
 * type and false, or nothing to type and true. */
export function keyOverride(event: { type: string; key: string; shiftKey: boolean; altKey: boolean; ctrlKey: boolean; metaKey: boolean }): { typed: string | null; xterm: boolean } {
  const shiftEnter = event.key === "Enter" && event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey;
  if (!shiftEnter) return { typed: null, xterm: true };
  return { typed: event.type === "keydown" ? NEW_LINE : null, xterm: false };
}

/** Whether a key copies the terminal's selection instead of reaching the agent: Cmd+C on a
 * Mac, Ctrl+C or Ctrl+Shift+C elsewhere, and only while text is selected, so Ctrl+C with
 * nothing selected still interrupts the agent and Ctrl+C on a Mac always does. Every event of
 * the key matches, so none of them reaches xterm. Inputs: the key event, whether text is
 * selected and whether this is a Mac. Output: true when the key copies. */
export function copyKey(event: { key: string; altKey: boolean; ctrlKey: boolean; metaKey: boolean }, selected: boolean, mac: boolean): boolean {
  if (!selected || event.altKey || event.key.toLowerCase() !== "c") return false;
  return mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

/** Where a menu opened at a point sits inside its panel: at the point when it fits, flipped
 * up or left of it near an edge, and never past the panel's top or left. Inputs: the point and
 * the menu's and panel's sizes, all in the panel's coordinates. Output: the menu's top left. */
export function menuSpot(at: { x: number; y: number }, menu: { width: number; height: number }, panel: { width: number; height: number }): { x: number; y: number } {
  const fit = (point: number, size: number, room: number) => Math.max(0, Math.min(point + size > room ? point - size : point, room - size));
  return { x: fit(at.x, menu.width, panel.width), y: fit(at.y, menu.height, panel.height) };
}

/** The text Copy output puts on the clipboard: every line of the terminal's buffer, without
 * trailing spaces or the blank rows under the last written line. Input: the buffer's lines.
 * Output: the text, lines joined by new lines. */
export function outputText(lines: string[]): string {
  const kept = lines.map((line) => line.trimEnd());
  while (kept.length > 0 && kept[kept.length - 1] === "") kept.pop();
  return kept.join("\n");
}

/** Encodes xterm's binary event, one byte per character code, such as some mouse reports.
 * Input: the binary string. Output: the bytes, each code kept to its low eight bits. */
export function encodeBinary(data: string): Uint8Array {
  const bytes = new Uint8Array(data.length);
  for (let index = 0; index < data.length; index += 1) bytes[index] = data.charCodeAt(index) & 0xff;
  return bytes;
}

/** Splits input bytes into chunks the bridge accepts, never cutting a UTF-8 character in two.
 * Inputs: the bytes and the chunk limit (at least 4). Output: the chunks in order, as plain
 * number arrays for the bridge; empty input gives no chunks. */
export function inputChunks(bytes: Uint8Array, max = INPUT_BYTES_MAX): number[][] {
  assert(Number.isInteger(max) && max >= 4, "chunk limit holds a whole UTF-8 character");
  const chunks: number[][] = [];
  let start = 0;
  while (start < bytes.length) {
    let end = Math.min(start + max, bytes.length);
    while (end < bytes.length && end > start + 1 && (bytes[end] & 0xc0) === 0x80) end -= 1;
    assert(end > start && end - start <= max, "a chunk holds 1 to max bytes");
    chunks.push(Array.from(bytes.subarray(start, end)));
    start = end;
  }
  return chunks;
}

/** Keeps a terminal size inside the engine's limits. Inputs: columns and rows. Output: whole
 * numbers from 2 to 1000, or null when either is not a finite number. */
export function terminalSize(cols: number, rows: number): { cols: number; rows: number } | null {
  if (!Number.isFinite(cols) || !Number.isFinite(rows)) return null;
  const clamp = (value: number) => Math.min(SIZE_MAX, Math.max(SIZE_MIN, Math.floor(value)));
  return { cols: clamp(cols), rows: clamp(rows) };
}

/** Runs a terminal's sends one at a time in the order queued, so a long paste never overtakes
 * later keys or a prompt. Input: a callback for the stop. Output: a function that queues one
 * send, which resolves false once nothing more can be delivered; it returns whether the send
 * was queued. After a stop, or past `QUEUED_CHUNKS_MAX` waiting sends, it refuses them and
 * reports the stop once. */
export function jobQueue(stopped: () => void): (job: () => Promise<boolean>) => boolean {
  let tail: Promise<void> = Promise.resolve();
  let waiting = 0;
  let stop = false;
  const halt = () => {
    if (stop) return;
    stop = true;
    stopped();
  };
  return (job) => {
    if (stop) return false;
    if (waiting >= QUEUED_CHUNKS_MAX) {
      halt();
      return false;
    }
    waiting += 1;
    tail = tail
      .then(async () => { if (!stop && !await job()) halt(); })
      .catch(halt)
      .finally(() => { waiting -= 1; });
    assert(waiting <= QUEUED_CHUNKS_MAX, "the send queue stays bounded");
    return true;
  };
}

/** Queues typed bytes on a terminal's send queue as chunks the bridge accepts. Inputs: the
 * queue and the send call for one chunk. Output: a function that queues bytes in order. */
export function keyQueue(run: (job: () => Promise<boolean>) => boolean, send: (chunk: number[]) => Promise<boolean>): (bytes: Uint8Array) => void {
  return (bytes) => {
    for (const chunk of inputChunks(bytes)) {
      if (!run(() => send(chunk))) return;
    }
  };
}

/** Splits a terminal's input into a typed lane and a wheel lane on one send queue. Wheel
 * reports that arrive while earlier input is still being sent wait as one batch that later
 * reports join, so a fast scroll costs a few sends rather than one per wheel event and never
 * fills the queue. Typed bytes close the batch, so keys and scrolling keep their order.
 * Inputs: the queue and the send call for one chunk, told whether the wheel made it.
 * Output: the two lanes. */
export function inputLanes(run: (job: () => Promise<boolean>) => boolean, send: (chunk: number[], wheel: boolean) => Promise<boolean>): { typed: (bytes: Uint8Array) => void; wheel: (bytes: Uint8Array) => void } {
  const keys = keyQueue(run, (chunk) => send(chunk, false));
  const scrolls = keyQueue(run, (chunk) => send(chunk, true));
  let batch: number[] | null = null;
  const typed = (bytes: Uint8Array) => {
    batch = null;
    keys(bytes);
  };
  const wheel = (bytes: Uint8Array) => {
    if (bytes.length > INPUT_BYTES_MAX) {
      batch = null;
      return scrolls(bytes);
    }
    if (batch && batch.length + bytes.length <= INPUT_BYTES_MAX) {
      batch.push(...bytes);
      return;
    }
    const waiting = Array.from(bytes);
    batch = run(() => {
      if (batch === waiting) batch = null;
      return send(waiting, true);
    }) ? waiting : null;
  };
  return { typed, wheel };
}

/** Sends typed input in order, one chunk at a time, on a queue of its own. Inputs: the send
 * call, which resolves false once input can no longer be delivered, and a callback for that
 * stop. Output: a function that queues bytes; after a stop it drops input. */
export function inputQueue(send: (chunk: number[]) => Promise<boolean>, stopped: () => void): (bytes: Uint8Array) => void {
  return keyQueue(jobQueue(stopped), send);
}

/** Calls `settle` once, on the first frame after which no queued output write is still being
 * parsed, so a new view keeps the pane's size until the attach's history and screen are both
 * drawn; a resize partway would push history rows off the view. Inputs: a frame scheduler that
 * returns its cancel, and the action. Output: a function to call as each write is queued; it
 * returns that write's done callback. */
export function settleAfterWrites(frame: (run: () => void) => () => void, settle: () => void): () => () => void {
  let pending = 0;
  let settled = false;
  let cancel = () => {};
  return () => {
    pending += 1;
    cancel();
    return () => {
      pending -= 1;
      assert(pending >= 0, "every write is done once");
      if (settled || pending > 0) return;
      cancel = frame(() => {
        if (settled || pending > 0) return;
        settled = true;
        settle();
      });
    };
  };
}

export const PROMPT_BYTES_MAX = 4096;

/** Checks a composer prompt before it is sent as the owner's explicit message. The engine
 * types it into the agent as one submission and tells a sub-agent's master; typed keys are
 * never sent this way. Input: the text. Output: the trimmed text, blank, or too long with
 * the plain reason. */
export function promptText(text: string): { kind: "ok"; text: string } | { kind: "blank" } | { kind: "too_long"; reason: string } {
  const trimmed = text.trim();
  if (!trimmed) return { kind: "blank" };
  const bytes = encoder.encode(trimmed).length;
  if (bytes > PROMPT_BYTES_MAX) return { kind: "too_long", reason: `Too long: ${bytes} of ${PROMPT_BYTES_MAX} bytes.` };
  return { kind: "ok", text: trimmed };
}

const QUERY_CSI: IFunctionIdentifier[] = [
  { final: "c" }, { prefix: ">", final: "c" }, { prefix: "=", final: "c" },
  { final: "n" }, { prefix: "?", final: "n" },
  { intermediates: "$", final: "p" }, { prefix: "?", intermediates: "$", final: "p" },
];
const QUERY_DCS: IFunctionIdentifier[] = [{ intermediates: "$", final: "q" }];
const QUERY_OSC = [4, 10, 11, 12];
const PASTE_CONTROLS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g;

/** Stops a view answering the terminal queries an agent's app writes (device attributes,
 * status and cursor reports, mode and setting requests, and color queries). tmux owns the
 * agent's terminal and answers them already; a second answer from the view would be typed
 * into the agent. A color sequence that only sets colors still applies. Input: the view's
 * terminal. Output: none; the handlers end with the terminal. */
export function muteQueries(terminal: { parser: Pick<IParser, "registerCsiHandler" | "registerDcsHandler" | "registerOscHandler"> }): void {
  for (const id of QUERY_CSI) terminal.parser.registerCsiHandler(id, () => true);
  for (const id of QUERY_DCS) terminal.parser.registerDcsHandler(id, () => true);
  for (const id of QUERY_OSC) terminal.parser.registerOscHandler(id, (data) => data.split(";").includes("?"));
}

/** The text a clipboard paste may type into an agent: every control character is dropped
 * except tab, new line and carriage return, so pasted text can never end the app's
 * bracketed paste or press Escape, Ctrl keys or other keys inside it. Input: the clipboard
 * text. Output: the text to paste. */
export function pasteText(text: string): string {
  const kept = text.replace(PASTE_CONTROLS, "");
  assert(!kept.includes("\x1b"), "pasted text keeps no Escape");
  return kept;
}

/** What one terminal channel message means for the view. */
export type TerminalEvent =
  | { kind: "output"; bytes: Uint8Array }
  | { kind: "attached"; cols: number; rows: number }
  | { kind: "ended"; copy: string };

const END_COPY: Record<string, string> = {
  terminal_agent_unknown: "This agent's terminal isn't running. It may have finished or not started yet.",
  terminal_ended: "This run finished. Its last screen stays above.",
  terminal_unavailable: "Terminals need tmux, and it isn't ready on this Mac. Run setup again from Settings.",
  terminal_viewers_full: "Terminal limit reached. Close a terminal or a terminal window, then open this one again.",
  terminal_agent_mismatch: "The engine opened a different agent's terminal, so the view closed.",
  terminal_connection_lost: "Lost the connection to this terminal. Open it again to reconnect.",
  terminal_connection_closed: "The engine closed this terminal view. Open it again to reconnect.",
  terminal_view_replaced: "This terminal opened in a newer view, so this one closed. Open it again to bring it back here.",
  exited: "The agent's app exited.",
};

/** Plain words for why a terminal view ended. Input: the engine's plain code. Output: one
 * sentence; unknown codes get a generic one. */
export function endCopy(code: string): string {
  return END_COPY[code] ?? "This terminal closed.";
}

/** Reads one message from an attached terminal channel. Input: the message, raw bytes or a
 * control frame. Output: output bytes, the attached size, or the end with plain words. */
export function terminalEvent(message: TerminalMessage): TerminalEvent {
  if (message instanceof ArrayBuffer) return { kind: "output", bytes: new Uint8Array(message) };
  switch (message.kind) {
    case "attached": return { kind: "attached", cols: message.cols, rows: message.rows };
    case "closed": return { kind: "ended", copy: endCopy(message.reason) };
    case "rejected": return { kind: "ended", copy: endCopy(message.code) };
  }
}
