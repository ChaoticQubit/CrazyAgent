import type { IFunctionIdentifier, IParser } from "@xterm/xterm";
import type { TerminalMessage } from "../../engine-client";

export const INPUT_BYTES_MAX = 4096;
export const SIZE_MIN = 2;
export const SIZE_MAX = 1000;
export const QUEUED_CHUNKS_MAX = 256;

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

/** Sends typed input in order, one chunk at a time, on a queue of its own. Inputs: the send
 * call, which resolves false once input can no longer be delivered, and a callback for that
 * stop. Output: a function that queues bytes; after a stop it drops input. */
export function inputQueue(send: (chunk: number[]) => Promise<boolean>, stopped: () => void): (bytes: Uint8Array) => void {
  return keyQueue(jobQueue(stopped), send);
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

/** Stops a view answering the terminal queries an agent's app writes (device attributes,
 * status and cursor reports, mode and setting requests). tmux owns the agent's terminal and
 * answers them already; a second answer from the view would be typed into the agent.
 * Input: the view's terminal. Output: none; the handlers end with the terminal. */
export function muteQueries(terminal: { parser: Pick<IParser, "registerCsiHandler" | "registerDcsHandler"> }): void {
  for (const id of QUERY_CSI) terminal.parser.registerCsiHandler(id, () => true);
  for (const id of QUERY_DCS) terminal.parser.registerDcsHandler(id, () => true);
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
  terminal_viewers_full: "Too many terminals are open. Close one and try again.",
  terminal_agent_mismatch: "The engine opened a different agent's terminal, so the view closed.",
  terminal_connection_lost: "Lost the connection to this terminal. Open it again to reconnect.",
  terminal_connection_closed: "The engine closed this terminal view. Open it again to reconnect.",
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
