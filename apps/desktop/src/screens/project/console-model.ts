import type { GraphNode } from "../../engine-client";
import type { Route } from "../../ui";
import { sendsMessage, type ComposerKeyEvent } from "../../ui/keys";

export const PROMPT_ROWS_MAX = 8;
export const HISTORY_MAX = 100;
export const MENU_ITEMS_MAX = 8;
export const FILES_MAX = 200;
export const HANDLE_CHARS_MAX = 32;
export const MASTER_HANDLE = "master";

/** Where a navigating console command goes. */
export type ConsoleTarget = "agents" | "changes" | "usage" | "process";

/** What a console command does: open a page, clear the view, list the commands or stop the master's turn. */
export type ConsoleAction = { kind: "navigate"; target: ConsoleTarget } | { kind: "clear" } | { kind: "help" } | { kind: "stop" };

/** One `/` command: its name without the slash, one line of help and its action. */
export interface ConsoleCommand {
  name: string;
  help: string;
  action: ConsoleAction;
}

/** The console's commands, in menu order. Add a command here and the menu, /help and Enter know it. */
export const CONSOLE_COMMANDS: readonly ConsoleCommand[] = [
  { name: "help", help: "List these commands", action: { kind: "help" } },
  { name: "clear", help: "Clear this view; the conversation stays", action: { kind: "clear" } },
  { name: "agents", help: "Open the Agents tab", action: { kind: "navigate", target: "agents" } },
  { name: "changes", help: "Open the Changes tab", action: { kind: "navigate", target: "changes" } },
  { name: "usage", help: "Open Usage in Settings", action: { kind: "navigate", target: "usage" } },
  { name: "process", help: "Open the project's Process tab", action: { kind: "navigate", target: "process" } },
  { name: "stop", help: "Stop the master's turn", action: { kind: "stop" } },
];

/** One `@` completion: the handle it inserts without the `@`, the name it shows and whether it names an agent or a file. */
export interface Mention {
  handle: string;
  label: string;
  kind: "agent" | "file";
}

/** What a prompt key does, or null when the textarea keeps the key. */
export type ConsoleKey = "send" | "newline" | "clear" | "history-back" | "history-forward"
  | "menu-up" | "menu-down" | "menu-pick" | "menu-complete" | "menu-close" | null;

/** What Enter submitted: nothing, a message for the master, a known command or an unknown one. */
export type Submitted = { kind: "empty" } | { kind: "message"; text: string } | { kind: "command"; command: ConsoleCommand } | { kind: "unknown"; name: string };

const COMMAND_WORD = /^\/([A-Za-z][A-Za-z0-9-]*)$/;
const TAG = /(?<![^\s(])@[A-Za-z0-9._/-]+/g;

/** The commands the `/` menu offers for the prompt: names starting with what follows the slash,
 * or names containing it when none starts with it, at most MENU_ITEMS_MAX. Inputs: the prompt
 * text and the command list. Output: the commands, or none unless the prompt is a lone `/` word. */
export function filterCommands(text: string, commands: readonly ConsoleCommand[] = CONSOLE_COMMANDS): ConsoleCommand[] {
  if (!text.startsWith("/") || /\s/.test(text)) return [];
  const query = text.slice(1).toLowerCase();
  const starts = commands.filter((command) => command.name.startsWith(query));
  return (starts.length > 0 ? starts : commands.filter((command) => command.name.includes(query))).slice(0, MENU_ITEMS_MAX);
}

/** Reads what Enter submitted: a first word of `/` and letters is a command, known or not;
 * anything else, a path starting with `/` included, is a message. Inputs: the prompt text and
 * the command list. Output: the submission. */
export function submitted(text: string, commands: readonly ConsoleCommand[] = CONSOLE_COMMANDS): Submitted {
  const trimmed = text.trim();
  if (!trimmed) return { kind: "empty" };
  const word = trimmed.split(/\s+/, 1)[0];
  const name = COMMAND_WORD.exec(word)?.[1]?.toLowerCase();
  if (name === undefined) return { kind: "message", text: trimmed };
  const command = commands.find((item) => item.name === name);
  return command ? { kind: "command", command } : { kind: "unknown", name };
}

/** The line shown for an unknown command. Input: its name. Output: the line. */
export function unknownLine(name: string): string {
  return `Unknown command /${name}. Type /help to see the commands.`;
}

/** The lines /help shows, one per command. Input: the command list. Output: the lines. */
export function helpLines(commands: readonly ConsoleCommand[] = CONSOLE_COMMANDS): string[] {
  const width = Math.max(...commands.map((command) => command.name.length)) + 3;
  return commands.map((command) => `/${command.name}`.padEnd(width) + command.help);
}

/** The page a navigating command opens. Inputs: the target and the project id. Output: the route. */
export function commandRoute(target: ConsoleTarget, projectId: string): Route {
  if (target === "usage") return { name: "settings", tab: "usage" };
  return { name: "project", id: projectId, tab: target };
}

/** Maps a prompt key to its console action. Nothing acts while an input method is composing.
 * With a menu open the arrows move, Enter runs, Tab completes and Esc closes; otherwise Enter
 * sends, Shift+Enter or Ctrl+J adds a line, Esc clears and the arrows recall history while the
 * prompt is empty or shows an unedited recalled line. Inputs: the key event and whether a menu
 * is open and history may be browsed. Output: the action, or null. */
export function consoleKey(event: ComposerKeyEvent, state: { menu: boolean; browsing: boolean }): ConsoleKey {
  if (event.isComposing || event.keyCode === 229) return null;
  const bare = !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey;
  if ((event.key === "Enter" && event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey)
    || (event.key.toLowerCase() === "j" && event.ctrlKey && !event.shiftKey && !event.metaKey && !event.altKey)) return "newline";
  if (state.menu && bare) {
    if (event.key === "ArrowDown") return "menu-down";
    if (event.key === "ArrowUp") return "menu-up";
    if (event.key === "Enter") return "menu-pick";
    if (event.key === "Tab") return "menu-complete";
    if (event.key === "Escape") return "menu-close";
  }
  if (sendsMessage(event)) return "send";
  if (!bare) return null;
  if (event.key === "Escape") return "clear";
  if (state.browsing && event.key === "ArrowUp") return "history-back";
  if (state.browsing && event.key === "ArrowDown") return "history-forward";
  return null;
}

/** Moves a menu choice by one, wrapping at either end. Inputs: the choice, the item count and
 * the step. Output: the new choice, 0 for an empty menu. */
export function moveChoice(index: number, count: number, step: 1 | -1): number {
  if (count <= 0) return 0;
  return (index + step + count) % count;
}

/** Steps through sent lines, oldest first in `history`: back from the newest, stopping at the
 * oldest, and forward past the newest to an empty prompt. Inputs: the history, the line shown
 * (null when none) and the direction. Output: the line to show and its index. */
export function recall(history: readonly string[], index: number | null, direction: 1 | -1): { index: number | null; text: string } {
  if (history.length === 0) return { index: null, text: "" };
  if (direction === -1) {
    const next = index === null ? history.length - 1 : Math.max(0, index - 1);
    return { index: next, text: history[next] };
  }
  if (index === null || index + 1 >= history.length) return { index: null, text: "" };
  return { index: index + 1, text: history[index + 1] };
}

/** Adds a sent line to the history, skipping blanks and a repeat of the newest line, keeping
 * the newest HISTORY_MAX. Inputs: the history and the line. Output: the new history. */
export function remember(history: readonly string[], text: string): string[] {
  const line = text.trim();
  if (!line || history.at(-1) === line) return [...history];
  return [...history, line].slice(-HISTORY_MAX);
}

/** Turns free text into an agent handle the way the engine does: lower-case ASCII letters and
 * digits joined by single hyphens, at most HANDLE_CHARS_MAX characters. Input: the text, a
 * leading `@` allowed. Output: the handle, or null when no letter or digit is left. */
export function toHandle(text: string): string | null {
  let kept = "";
  for (const char of text.trim().replace(/^@/, "")) {
    if (kept.length >= HANDLE_CHARS_MAX) break;
    const lower = char.toLowerCase();
    if (/^[a-z0-9]$/.test(lower) && char.charCodeAt(0) < 128) kept += lower;
    else if (/^[\s\-_./]$/.test(char) && kept && !kept.endsWith("-")) kept += "-";
  }
  kept = kept.replace(/-+$/, "");
  return kept ? kept : null;
}

/** The agents `@` completes: the master, then each sub-agent by its role handle from a label
 * such as "Ada (@onboarding-fix)", or by its name made into a handle; a handle shared by two
 * agents is offered once. Input: the team graph's nodes. Output: the mentions. */
export function agentMentions(nodes: readonly GraphNode[]): Mention[] {
  const mentions: Mention[] = [{ handle: MASTER_HANDLE, label: "Master", kind: "agent" }];
  for (const node of nodes) {
    if (node.kind !== "agent") continue;
    const handle = /\(@([a-z0-9-]+)\)\s*$/.exec(node.label)?.[1] ?? toHandle(node.label);
    if (handle && !mentions.some((mention) => mention.handle === handle)) mentions.push({ handle, label: node.label, kind: "agent" });
  }
  return mentions;
}

/** The files `@` completes, at most FILES_MAX; a path with whitespace cannot be one tag and is
 * left out. Input: project-relative paths. Output: the mentions. */
export function fileMentions(paths: readonly string[]): Mention[] {
  return paths.filter((path) => path && !/\s/.test(path)).slice(0, FILES_MAX).map((path) => ({ handle: path, label: path, kind: "file" as const }));
}

/** The `@` word the caret is in: an `@` at the start of the prompt or after whitespace, with no
 * whitespace up to the caret. Inputs: the prompt text and the caret. Output: where the `@` is
 * and what follows it, or null. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  const before = text.slice(0, caret);
  const start = before.lastIndexOf("@");
  if (start < 0 || /\s/.test(before.slice(start + 1))) return null;
  if (start > 0 && !/\s/.test(before[start - 1])) return null;
  return { start, query: before.slice(start + 1) };
}

/** The mentions matching what follows the `@`: handles starting with it first, then handles or
 * names containing it, at most `limit`. Inputs: the mentions, the query and the limit. Output: the matches. */
export function matchMentions(mentions: readonly Mention[], query: string, limit = MENU_ITEMS_MAX): Mention[] {
  const wanted = query.toLowerCase();
  const starts = mentions.filter((mention) => mention.handle.toLowerCase().startsWith(wanted));
  const contains = mentions.filter((mention) => !mention.handle.toLowerCase().startsWith(wanted)
    && (mention.handle.toLowerCase().includes(wanted) || mention.label.toLowerCase().includes(wanted)));
  return [...starts, ...contains].slice(0, limit);
}

/** Replaces the `@` word with the chosen tag and puts the caret after a following space,
 * adding one when none follows. Inputs: the prompt text, the caret, the `@` word and the
 * chosen mention. Output: the new text and caret. */
export function insertMention(text: string, caret: number, at: { start: number }, mention: Mention): { text: string; caret: number } {
  const tag = `@${mention.handle}`;
  const rest = text.slice(caret);
  const spaced = /^\s/.test(rest);
  const next = `${text.slice(0, at.start)}${tag}${spaced ? "" : " "}${rest}`;
  return { text: next, caret: at.start + tag.length + 1 };
}

/** Splits a line into plain text and the tags naming a known agent or file, for highlighting.
 * Inputs: the line and the known handles. Output: the segments, adjacent plain text joined. */
export function tagSegments(text: string, known: ReadonlySet<string>): { text: string; tag: boolean }[] {
  const segments: { text: string; tag: boolean }[] = [];
  const plain = (part: string) => {
    if (!part) return;
    const last = segments.at(-1);
    if (last && !last.tag) last.text += part;
    else segments.push({ text: part, tag: false });
  };
  let at = 0;
  for (const match of text.matchAll(TAG)) {
    const word = match[0].replace(/\.+$/, "");
    const start = match.index ?? 0;
    plain(text.slice(at, start));
    if (known.has(word.slice(1))) {
      segments.push({ text: word, tag: true });
      plain(match[0].slice(word.length));
    } else plain(match[0]);
    at = start + match[0].length;
  }
  plain(text.slice(at));
  return segments;
}

/** How many rows the prompt shows: one per line, at most PROMPT_ROWS_MAX. Input: the prompt
 * text. Output: the rows. */
export function promptRows(text: string): number {
  return Math.min(PROMPT_ROWS_MAX, Math.max(1, text.split("\n").length));
}
