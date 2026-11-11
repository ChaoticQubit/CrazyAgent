/** The most bytes one command may hold, as the engine counts them: UTF-8. */
export const COMMAND_BYTES_MAX = 512;

/**
 * How long a run waits for its new terminal to attach, in milliseconds: under the engine's
 * five second wait for the acknowledgement, so a command is not typed after the owner was told
 * it was not.
 */
export const READY_WAIT_MS = 4000;

const BLANK = /^[\t-\r \u{85}\u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}]*$/u;
const HIDDEN = /[\p{Cc}\u{ad}\u{34f}\u{61c}\u{115f}\u{1160}\u{180e}\u{200b}-\u{200f}\u{2028}-\u{202e}\u{2060}-\u{206f}\u{3164}\u{fe00}-\u{fe0f}\u{feff}\u{ffa0}\u{fff9}-\u{fffb}\u{e0000}-\u{e007f}\u{e0100}-\u{e01ef}]/u;
/** The characters a bare word may hold beside letters and digits; the shared rule table pins them. */
export const BARE = "-_./:=,@+%";

/** The characters a quoted word may not hold, since a shell still acts on them there. */
export const QUOTED_REFUSED = "$`\\!";

const WORD = /^[A-Za-z0-9 ]$/;

/** The engine's `run_command` event: the run to acknowledge, the command and whether to press Enter. */
export interface RunEvent {
  request: string;
  command: string;
  press_enter: boolean;
}

/** The part of a VS Code terminal a run uses. */
export interface RunTerminal {
  show(): void;
  sendText(text: string, addNewLine: boolean): void;
  dispose(): void;
}

/** A new terminal and whether it attached: true once typing reaches its shell, false when it stopped first. */
export interface Opened {
  terminal: RunTerminal;
  ready: Promise<boolean>;
}

/** What a run needs: how to open a new terminal, how to acknowledge, and the attach wait in milliseconds. */
export interface RunDeps {
  open: () => Opened;
  ack: (request: string) => Promise<unknown>;
  waitMs?: number;
}

/**
 * Whether a line is only plain words: bare words of letters, digits and the `BARE` characters,
 * separated by spaces, and closed single- or double-quoted words that hold no `QUOTED_REFUSED`
 * character. The shell expands nothing in such a line.
 * Input: the line, at most `COMMAND_BYTES_MAX` bytes. Output: the answer.
 */
function plain(command: string): boolean {
  let quote = "";
  for (const mark of command) {
    if (quote !== "") {
      if (mark === quote) quote = "";
      else if (QUOTED_REFUSED.includes(mark)) return false;
    } else if (mark === "'" || mark === '"') quote = mark;
    else if (!WORD.test(mark) && !BARE.includes(mark)) return false;
  }
  return quote === "";
}

/**
 * What to type for a command, or null for one the engine's single-command rule refuses,
 * whatever the engine sent: blank, over `COMMAND_BYTES_MAX` bytes, more than one line, a
 * control or invisible character, or anything but plain words.
 * Inputs: the command and whether to press Enter. Output: the text verbatim and whether a new
 * line follows it, or null.
 */
export function typed(command: unknown, pressEnter: boolean): { text: string; addNewLine: boolean } | null {
  if (typeof command !== "string" || BLANK.test(command)) return null;
  if (Buffer.byteLength(command, "utf8") > COMMAND_BYTES_MAX) return null;
  if (/[\n\r]/.test(command) || HIDDEN.test(command)) return null;
  if (!plain(command)) return null;
  return { text: command, addNewLine: pressEnter === true };
}

/**
 * Resolves with a promise's value, or undefined once the time is up. Inputs: the promise and
 * the time in milliseconds. Output: the value or undefined; the timer never outlives the call.
 */
async function within<T>(promise: Promise<T>, timeMs: number): Promise<T | undefined> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<undefined>((resolve) => {
    timer = setTimeout(() => resolve(undefined), timeMs);
  });
  try {
    return await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Runs one `run_command` event: opens a new terminal, waits until it attached, shows it, types
 * the command and only then acknowledges. A command the rule refuses opens nothing; a terminal
 * the engine refused is shown with its reason and gets no text; one that does not attach within
 * the wait is closed, so nothing is typed after the engine stopped waiting.
 * Inputs: the event and how to open and acknowledge. Output: whether the command was typed.
 */
export async function handle(event: RunEvent, deps: RunDeps): Promise<boolean> {
  const line = typed(event.command, event.press_enter);
  if (line === null) return false;
  const { terminal, ready } = deps.open();
  const attached = await within(ready, deps.waitMs ?? READY_WAIT_MS);
  if (attached === undefined) {
    terminal.dispose();
    return false;
  }
  terminal.show();
  if (attached !== true) return false;
  terminal.sendText(line.text, line.addNewLine);
  await deps.ack(event.request);
  return true;
}
