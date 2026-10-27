import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Route } from "../../ui";
import { displayText } from "../conversation";
import {
  CONSOLE_COMMANDS, commandRoute, consoleKey, fileMentions, filterCommands, helpLines, insertMention, matchMentions,
  mentionAt, moveChoice, promptRows, recall, remember, submitted, tagSegments, unknownLine, type ConsoleCommand, type Mention,
} from "./console-model";
import "./console.css";

/** What a transcript line is: the owner's message, the master's reply, an engine note, a tool
 * call, a hand-off to a sub-agent or a failure. */
export type ConsoleLineKind = "owner" | "reply" | "engine" | "tool" | "delegate" | "error";

/** One transcript line, with an id that stays the same while the line is shown. */
export interface ConsoleLine {
  id: string;
  kind: ConsoleLineKind;
  text: string;
}

/** The console's inputs: the project, its transcript, the `@` agents (see agentMentions) and
 * files (null when no file list exists), whether Enter may send now, a status line, and the
 * send, stop and navigate actions. */
export interface ConsoleProps {
  projectId: string;
  lines: readonly ConsoleLine[];
  agents: readonly Mention[];
  files?: readonly string[] | null;
  canSend?: boolean;
  status?: string;
  onSend: (text: string) => void;
  onStop: () => void;
  onNavigate: (route: Route) => void;
}

export const CONSOLE_HINT = "Enter to send, Shift+Enter for a new line, / for commands, @ to tag.";
export const AGENTS_ONLY = "@ completes agents only: this build has no project file list.";
export const HELD = "The master can't take a message right now. Your text stays here.";

const MARKS: Record<ConsoleLineKind, string> = { owner: "> ", reply: "", engine: "", tool: "$ ", delegate: "→ ", error: "! " };
const STICK_PX = 24;

/** A console note the owner's own commands add, shown after the transcript line it followed. */
interface Note {
  id: number;
  after: string | null;
  text: string;
}

/** One menu row: a command or a mention, with the text it shows. */
type Choice = { kind: "command"; command: ConsoleCommand } | { kind: "mention"; mention: Mention };

/** The transcript in order, the owner's notes placed after the line they followed, and lines up
 * to the last `/clear` hidden. Inputs: the lines, the notes and the id of the line `/clear`
 * hid through. Output: the shown entries. */
function shown(lines: readonly ConsoleLine[], notes: readonly Note[], cleared: string | null): { key: string; kind: ConsoleLineKind; text: string }[] {
  const from = cleared === null ? 0 : lines.findIndex((line) => line.id === cleared) + 1;
  const visible = lines.slice(from);
  const ids = new Set(visible.map((line) => line.id));
  const entry = (note: Note): { key: string; kind: ConsoleLineKind; text: string } => ({ key: `note-${note.id}`, kind: "engine", text: note.text });
  const leads = (note: Note) => note.after === null || note.after === cleared;
  const entries = notes.filter(leads).map(entry);
  for (const line of visible) entries.push({ key: line.id, kind: line.kind, text: line.text }, ...notes.filter((note) => note.after === line.id).map(entry));
  entries.push(...notes.filter((note) => !leads(note) && !ids.has(note.after ?? "")).map(entry));
  return entries;
}

/** A line's text with known `@` tags highlighted. Inputs: the text and the known handles. Output: the nodes. */
function tagged(text: string, known: ReadonlySet<string>): ReactNode[] {
  return tagSegments(displayText(text), known).map((segment, index) => segment.tag ? <span key={index} className="cs-tag">{segment.text}</span> : segment.text);
}

/** The master console: a terminal-style transcript that scrolls on its own and one `> ` prompt
 * growing to eight lines. Enter sends, Shift+Enter or Ctrl+J adds a line, Up and Down recall
 * sent lines on an empty prompt, Esc clears; `/` at the start opens the command menu and `@`
 * completes agent handles and files. Input: ConsoleProps. Output: the console. */
export function Console({ projectId, lines, agents, files = null, canSend = true, status, onSend, onStop, onNavigate }: ConsoleProps) {
  const [text, setTextState] = useState("");
  const [caret, setCaret] = useState(0);
  const [history, setHistory] = useState<string[]>([]);
  const [recalled, setRecalled] = useState<number | null>(null);
  const [choice, setChoice] = useState(0);
  const [dismissed, setDismissed] = useState(false);
  const [notes, setNotes] = useState<Note[]>([]);
  const [cleared, setCleared] = useState<string | null>(null);
  const [held, setHeld] = useState(false);
  const field = useRef<HTMLTextAreaElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const pending = useRef<number | null>(null);
  const noteId = useRef(0);
  const menuId = useId();
  const fieldId = useId();

  const mentions = [...agents, ...fileMentions(files ?? [])];
  const known = new Set(mentions.map((mention) => mention.handle));
  const mention = dismissed ? null : mentionAt(text, caret);
  const choices: Choice[] = dismissed ? [] : mention
    ? matchMentions(mentions, mention.query).map((item) => ({ kind: "mention" as const, mention: item }))
    : filterCommands(text).map((command) => ({ kind: "command" as const, command }));
  const open = choices.length > 0;
  const picked = open ? Math.min(choice, choices.length - 1) : 0;
  const entries = shown(lines, notes, cleared);
  const lastEntry = entries.at(-1);

  useLayoutEffect(() => {
    if (pending.current === null || !field.current) return;
    field.current.setSelectionRange(pending.current, pending.current);
    pending.current = null;
  }, [text]);

  useLayoutEffect(() => {
    const box = scroller.current;
    if (box && stick.current) box.scrollTop = box.scrollHeight;
  }, [entries.length, lastEntry?.text]);

  const setText = (next: string, at = next.length) => {
    setTextState(next);
    setCaret(at);
    setDismissed(false);
    setChoice(0);
    setHeld(false);
    pending.current = at;
  };
  const lastLineId = () => lines.at(-1)?.id ?? null;
  const note = (texts: string[]) => setNotes((old) => [...old, ...texts.map((value) => {
    noteId.current += 1;
    return { id: noteId.current, after: lastLineId(), text: value };
  })]);

  const run = (command: ConsoleCommand) => {
    const action = command.action;
    if (action.kind === "help") note(helpLines());
    else if (action.kind === "clear") { setCleared(lastLineId()); setNotes([]); }
    else if (action.kind === "stop") onStop();
    else onNavigate(commandRoute(action.target, projectId));
  };
  const submit = (value: string) => {
    const result = submitted(value, CONSOLE_COMMANDS);
    if (result.kind === "empty") return;
    if (result.kind === "message" && !canSend) { setHeld(true); return; }
    setHistory((old) => remember(old, value));
    setRecalled(null);
    setText("");
    if (result.kind === "message") onSend(result.text);
    else if (result.kind === "command") run(result.command);
    else note([unknownLine(result.name)]);
  };
  const choose = (item: Choice, complete: boolean) => {
    if (item.kind === "mention" && mention) {
      const next = insertMention(text, caret, mention, item.mention);
      setText(next.text, next.caret);
    } else if (item.kind === "command") {
      if (complete) setText(`/${item.command.name} `);
      else submit(`/${item.command.name}`);
    }
  };
  const insertLine = () => {
    const box = field.current;
    const start = box?.selectionStart ?? text.length;
    const end = box?.selectionEnd ?? text.length;
    setText(`${text.slice(0, start)}\n${text.slice(end)}`, start + 1);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    const browsing = text === "" || (recalled !== null && history[recalled] === text);
    const action = consoleKey(event.nativeEvent, { menu: open, browsing });
    if (action === null) return;
    event.preventDefault();
    if (action === "send") submit(text);
    else if (action === "newline") insertLine();
    else if (action === "clear") { setText(""); setRecalled(null); }
    else if (action === "history-back" || action === "history-forward") {
      const step = recall(history, recalled, action === "history-back" ? -1 : 1);
      setRecalled(step.index);
      setText(step.text);
    } else if (action === "menu-up" || action === "menu-down") setChoice(moveChoice(picked, choices.length, action === "menu-up" ? -1 : 1));
    else if (action === "menu-pick" || action === "menu-complete") choose(choices[picked], action === "menu-complete");
    else setDismissed(true);
  };

  const hint = held ? HELD : mention && open && files === null ? AGENTS_ONLY : status ?? CONSOLE_HINT;
  const optionId = (index: number) => `${menuId}-${index}`;

  return <section className="cs" aria-label="Master console">
    <div className="cs-scroll" ref={scroller} role="log" aria-live="polite"
      onScroll={(event) => { const box = event.currentTarget; stick.current = box.scrollHeight - box.scrollTop - box.clientHeight < STICK_PX; }}>
      {entries.map((entry) => <p key={entry.key} className={`cs-line cs-${entry.kind}`}>
        {MARKS[entry.kind] ? <span className="cs-mark" aria-hidden="true">{MARKS[entry.kind]}</span> : null}
        {entry.kind === "owner" || entry.kind === "delegate" ? tagged(entry.text, known) : displayText(entry.text)}
      </p>)}
    </div>
    <div className="cs-entry">
      {open && <ul className="cs-menu" id={menuId} role="listbox" aria-label={mention ? "Tags" : "Commands"}>
        {choices.map((item, index) => <li key={item.kind === "command" ? item.command.name : `${item.mention.kind}-${item.mention.handle}`}
          id={optionId(index)} role="option" aria-selected={index === picked} className="cs-option"
          onMouseDown={(event) => { event.preventDefault(); choose(item, false); }}>
          {item.kind === "command"
            ? <><span className="cs-name">/{item.command.name}</span><span className="cs-detail">{item.command.help}</span></>
            : <><span className="cs-name">@{displayText(item.mention.handle)}</span><span className="cs-detail">{displayText(item.mention.label)}</span></>}
        </li>)}
      </ul>}
      <div className="cs-input">
        <span className="cs-mark" aria-hidden="true">&gt;</span>
        <textarea ref={field} id={fieldId} aria-label="Message the master" rows={promptRows(text)} value={text} spellCheck={false}
          aria-autocomplete="list" aria-expanded={open} aria-controls={open ? menuId : undefined}
          aria-activedescendant={open ? optionId(picked) : undefined}
          onChange={(event) => { pending.current = null; setTextState(event.target.value); setCaret(event.target.selectionStart); setDismissed(false); setChoice(0); setHeld(false); setRecalled(null); }}
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onKeyDown={onKeyDown} />
      </div>
      <p className="cs-status" role="status">{hint}</p>
    </div>
  </section>;
}
