import type { WebglAddon } from "@xterm/addon-webgl";
import type { IFunctionIdentifier, IParser, Terminal } from "@xterm/xterm";
import type { TerminalMessage } from "../../engine-client";

export const INPUT_BYTES_MAX = 4096;
export const SIZE_MIN = 2;
export const SIZE_MAX = 1000;
export const QUEUED_CHUNKS_MAX = 256;
const ESC = 0x1b;

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

type Job = () => Promise<boolean>;

/** How many sends every `jobQueue` of this page took, so a lane's waiting batch can tell
 * whether anything was queued after it, through whatever wrapper hands sends to the queue. */
let queuedSends = 0;

/** Runs a terminal's sends one at a time in the order queued, so a long paste never overtakes
 * later keys or a prompt. Input: a callback for the stop. Output: a function that queues one
 * send, which resolves false once nothing more can be delivered; it returns whether the send
 * was queued. After a stop, or past `QUEUED_CHUNKS_MAX` waiting sends, it refuses them and
 * reports the stop once. */
export function jobQueue(stopped: () => void): (job: Job) => boolean {
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
    queuedSends += 1;
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

/** One lane's waiting batch on a shared send queue. Bytes that arrive while earlier input is
 * still being sent join the one batch that waits, up to `INPUT_BYTES_MAX`, but only while no
 * `jobQueue` of the page took a send after that batch, so nothing queued after it, a prompt or
 * the other lane's input, is ever overtaken; it goes out as one send when the queue reaches it,
 * and nothing waits on a timer. Bytes over the limit go out in chunks; empty input sends
 * nothing. Inputs: the queue, a `jobQueue` or a wrapper that hands it the sends, and the send
 * call for one chunk. Output: a function that queues bytes and, when told, closes the batch
 * after them. */
function batchLane(run: (job: Job) => boolean, send: (chunk: number[]) => Promise<boolean>): (bytes: Uint8Array, last: boolean) => void {
  const chunks = keyQueue(run, send);
  let waiting: { bytes: number[]; after: number } | null = null;
  return (bytes, last) => {
    if (bytes.length === 0) return;
    if (bytes.length > INPUT_BYTES_MAX) {
      waiting = null;
      return chunks(bytes);
    }
    const joins = waiting !== null
      && waiting.after === queuedSends
      && waiting.bytes.length + bytes.length <= INPUT_BYTES_MAX;
    if (joins && waiting) {
      waiting.bytes.push(...bytes);
    } else {
      const batch = Array.from(bytes);
      const queued = run(() => {
        if (waiting?.bytes === batch) waiting = null;
        return send(batch);
      });
      waiting = queued ? { bytes: batch, after: queuedSends } : null;
    }
    if (last) waiting = null;
    assert(waiting === null || waiting.bytes.length <= INPUT_BYTES_MAX, "a waiting batch fits one send");
  };
}

/** Splits a terminal's input into a typed lane and a wheel lane on one send queue. Keys typed
 * or wheel reports that arrive while earlier input is still being sent wait as one batch per
 * lane, so fast typing or a fast scroll costs one send per round trip rather than one per key
 * or wheel event, and never fills the queue. A batch takes more input only while no send was
 * queued after it, so keys, scrolling and prompts keep their order. A lone Esc closes its batch
 * after it, so Esc and a quick key after it never reach the app as one Alt key. Inputs: the
 * queue and the send call for one chunk, told whether the wheel made it. Output: the two
 * lanes. */
export function inputLanes(run: (job: Job) => boolean, send: (chunk: number[], wheel: boolean) => Promise<boolean>): { typed: (bytes: Uint8Array) => void; wheel: (bytes: Uint8Array) => void } {
  const keys = batchLane(run, (chunk) => send(chunk, false));
  const scrolls = batchLane(run, (chunk) => send(chunk, true));
  return {
    typed: (bytes) => keys(bytes, bytes.length === 1 && bytes[0] === ESC),
    wheel: (bytes) => scrolls(bytes, false),
  };
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

/** A view's drawing on the GPU, released at most once. */
export type GpuDrawing = { release(): void };

/** Moves an opened view's drawing onto the GPU with xterm's WebGL addon, so a busy pane stops
 * rebuilding DOM rows on every frame. A load that fails, as on a machine without WebGL2, keeps
 * the DOM renderer. Releasing disposes the addon, which hands drawing back to DOM rows, then
 * loses its context and empties its canvas, so the GPU memory goes at once; disposing first
 * means a release never reads as a context loss. A lost context releases it and tells `lost`.
 * Inputs: the opened terminal, the addon class and what hears a loss. Output: the drawing, or
 * null when the GPU refused it. */
export function drawOnGpu(terminal: Pick<Terminal, "element" | "loadAddon">, Webgl: new () => WebglAddon, lost: () => void): GpuDrawing | null {
  const element = terminal.element;
  assert(element !== undefined, "the view is open before its GPU renderer loads");
  const before = new Set(element.querySelectorAll("canvas"));
  const addon = new Webgl();
  try {
    terminal.loadAddon(addon);
  } catch {
    addon.dispose();
    return null;
  }
  const canvases = [...element.querySelectorAll("canvas")].filter((canvas) => !before.has(canvas));
  const contexts = canvases.flatMap((canvas) => canvas.getContext("webgl2") ?? []);
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    addon.dispose();
    for (const context of contexts) if (!context.isContextLost()) context.getExtension("WEBGL_lose_context")?.loseContext();
    for (const canvas of canvases) {
      canvas.width = 0;
      canvas.height = 0;
    }
  };
  addon.onContextLoss(() => {
    release();
    lost();
  });
  return { release };
}

/** The lines a terminal shows now, read from its buffer, so they read the same whichever
 * renderer draws them. Input: the terminal. Output: one line per row of the screen, with
 * trailing blanks cut. */
export function screenLines(terminal: Pick<Terminal, "buffer" | "rows">): string[] {
  const buffer = terminal.buffer.active;
  const lines: string[] = [];
  for (let row = 0; row < terminal.rows; row += 1) lines.push(buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? "");
  return lines;
}

/** Lets a reader on the page, such as an end-to-end check, read a view's screen from its
 * element: `readScreen()` on the element returns `screenLines` of its terminal, read only when
 * asked, so nothing runs per write. Inputs: the view's element and its terminal. Output: none. */
export function readableScreen(host: HTMLElement, terminal: Pick<Terminal, "buffer" | "rows">): void {
  Object.defineProperty(host, "readScreen", { configurable: true, value: () => screenLines(terminal) });
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

/** The text a drafted message types into an agent's terminal: as a clipboard paste, with each
 * run of line breaks made one space while the app has not turned on bracketed paste, as a bare
 * line break there presses Enter and would send the draft unasked. Inputs: the draft and
 * whether the app reads pastes bracketed. Output: the text to paste. */
export function seedText(text: string, bracketed: boolean): string {
  const kept = pasteText(text);
  if (bracketed) return kept;
  const flat = kept.replace(/[\r\n]+/g, " ");
  assert(!/[\r\n]/.test(flat), "an unbracketed seed keeps no line break");
  return flat;
}

/** The key that makes a drag select text while the app in a terminal reads the mouse: xterm
 * then sends a plain drag to the app, and only a drag with this key held selects. Input:
 * whether this is a Mac. Output: Option on a Mac, Shift elsewhere. */
export function dragKey(mac: boolean): "Option" | "Shift" {
  return mac ? "Option" : "Shift";
}

/** What a live terminal says while the app in it reads the mouse, as an agent in its
 * Fullscreen display does, so the owner can still select and copy. Input: whether this is a
 * Mac. Output: the sentence. */
export function selectNote(mac: boolean): string {
  return `Hold ${dragKey(mac)} and drag to select text; the app in this terminal reads the mouse.`;
}

/** What one terminal channel message means for the view. */
export type TerminalEvent =
  | { kind: "output"; bytes: Uint8Array }
  | { kind: "attached"; cols: number; rows: number }
  | { kind: "ended"; copy: string };

const END_COPY: Record<string, string> = {
  terminal_agent_unknown: "This agent's terminal isn't running. It may have finished or not started yet.",
  terminal_teammates_server_ended: "The terminal server this lead's teammates ran on ended, so its team panes are closed.",
  terminal_ended: "This run finished. Its last screen stays above.",
  terminal_unavailable: "Terminals need tmux, and it isn't ready on this Mac. Run setup again from Settings.",
  terminal_viewers_full: "Terminal limit reached. Close a terminal or a terminal window, then open this one again.",
  terminal_agent_mismatch: "The engine opened a different agent's terminal, so the view closed.",
  terminal_connection_lost: "Lost the connection to this terminal. Open it again to reconnect.",
  terminal_connection_closed: "The engine closed this terminal view. Open it again to reconnect.",
  terminal_view_replaced: "This terminal opened in a newer view, so this one closed. Open it again to bring it back here.",
  terminal_outcome_unknown: "The engine didn't answer in time while opening this terminal.",
  terminal_attach_timeout: "The engine took too long to open this terminal.",
  terminal_not_answering: "The terminal server didn't answer in time.",
  terminal_attach_failed: "The engine couldn't open this terminal.",
  exited: "The agent's app exited.",
};

/** Plain words for why a terminal view ended. Input: the engine's plain code. Output: one
 * sentence; unknown codes get a generic one. */
export function endCopy(code: string): string {
  return END_COPY[code] ?? "This terminal closed.";
}

/** The most times in a row a view that follows its agent opens the terminal again. */
export const FOLLOW_TRIES = 30;

/** How long a view stays open before its end counts as a new run to follow afresh. */
export const FOLLOW_STEADY_MS = 5000;

/** Counts one more try for a following view whose terminal ended or would not open; only a
 * view that stayed open `FOLLOW_STEADY_MS` starts the count afresh, and an end that reads
 * earlier than the open, as a clock stepped back would give, is not steady. Inputs: the tries
 * so far, when the view that just ended opened (null when it never opened) and now, in ms.
 * Output: the next try's count, or null once `FOLLOW_TRIES` are spent and it says why. */
export function followTry(tries: number, opened: number | null, now: number): number | null {
  assert(tries >= 0 && tries <= FOLLOW_TRIES, "follow tries out of bounds");
  const counted = opened !== null && now - opened >= FOLLOW_STEADY_MS ? 0 : tries;
  return counted < FOLLOW_TRIES ? counted + 1 : null;
}

/** The line a view shows while it opens its terminal. Inputs: why the last try ended, or
 * null on the first try, and the count of the try under way. Output: the line. */
export function connectingLine(reason: string | null, tries: number): string {
  assert(tries >= 0 && tries <= FOLLOW_TRIES, "follow tries out of bounds");
  assert(reason === null || reason.length > 0, "a reason is plain words");
  if (reason === null || tries === 0) return "Connecting to the terminal...";
  return `Connecting to the terminal again, try ${tries} of ${FOLLOW_TRIES}. Last try: ${reason}`;
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
