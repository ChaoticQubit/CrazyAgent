import { useEffect, useRef, useState } from "react";
import { failureCopy, readMessages } from "../../api-projects";
import type { HarnessEntry, MasterMessagePage, MasterMode, MasterTurn, SessionMessage } from "../../engine-client";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import type { ConsoleLine } from "./console";

/** The most stored messages one read asks the engine for. */
export const MESSAGES_PER_READ = 64;
/** The most stored messages of one session a view holds. */
export const MESSAGES_HELD_MAX = 4096;
/** What an owner row of a terminal turn shows when its typed words were not read. */
export const NO_TEXT = "Typed in the master's terminal. The words were not read.";
/** What the master view says when a session holds more messages than the view reads. */
export const TOO_LONG = "This session has more messages than this view holds. Open it in Sessions to read the earlier ones.";

const NOT_IN_BUILD = "Not ready yet: stored messages aren't in this build.";
const BACKENDS: Record<string, string> = { rcli: "RCLI", codex: "Codex", claude: "Claude" };

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Names the master that ran a message or a session: the harness name for a live master,
 * "One-turn master (RCLI)", "(Codex)" or "(Claude)" for the master the engine runs itself.
 * Inputs: the master mode, the stored harness, if any, and the harness catalog, if read.
 * Output: the name. */
export function ranBy(mode: MasterMode, harness: string | null, harnesses: HarnessEntry[] | null): string {
  if (mode === "live") return harness === null ? "Live master" : harnessName(harnesses, harness);
  const backend = harness === null ? undefined : BACKENDS[harness];
  return backend === undefined ? "One-turn master" : `One-turn master (${backend})`;
}

/** Reads a message page out of a reply, so a reply of another shape counts as a failed read
 * and never as an empty conversation. Input: the reply. Output: the page, or null. */
export function pageOf(reply: unknown): MasterMessagePage | null {
  const page = typeof reply === "object" && reply !== null && "page" in reply ? (reply as { page: unknown }).page : null;
  if (typeof page !== "object" || page === null) return null;
  const shaped = page as Partial<MasterMessagePage>;
  if (!Array.isArray(shaped.messages) || typeof shaped.count !== "number") return null;
  if (shaped.next !== null && typeof shaped.next !== "number") return null;
  return shaped as MasterMessagePage;
}

/** Adds the rows of a read to the rows a view holds: a row with a held sequence number
 * replaces the held one, since a terminal turn's owner row gains its words when the turn
 * ends, and the result is in sequence order. Inputs: the held rows and the read rows.
 * Output: the merged rows, oldest first. */
export function mergeMessages(held: readonly SessionMessage[], read: readonly SessionMessage[]): SessionMessage[] {
  const bySeq = new Map<number, SessionMessage>();
  for (const row of held) bySeq.set(row.seq, row);
  for (const row of read) bySeq.set(row.seq, row);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  assert(merged.length <= held.length + read.length, "merged rows outnumber the held and read rows");
  return merged;
}

/** One stored message as a console line: the owner's words, the engine's request, the
 * master's reply, or the divider where another master took the conversation over. An owner
 * row with no text says the words were typed in the terminal. Input: the row. Output: the
 * line, or null for a row of any other speaker with no text. */
export function messageLine(row: SessionMessage): ConsoleLine | null {
  if (row.speaker === "owner") return row.text === null ? { id: `m-${row.seq}`, kind: "engine", text: NO_TEXT } : { id: `m-${row.seq}`, kind: "owner", text: row.text };
  if (row.text === null) return null;
  if (row.speaker === "switch") return { id: `m-${row.seq}-switch`, kind: "engine", text: row.text };
  return { id: `m-${row.seq}`, kind: row.speaker === "master" ? "reply" : "engine", text: row.text };
}

/** The console lines of stored messages alone, oldest first. Input: the rows. Output: the lines. */
export function messageLines(rows: readonly SessionMessage[]): ConsoleLine[] {
  return rows.map(messageLine).filter((line): line is ConsoleLine => line !== null);
}

/** The one conversation of a session whose live master is the one-turn master: every stored
 * message in order, with each one-turn turn shown through its turn, so its state, tasks and
 * accounts stay under it, and the rows of a live terminal master and the "Switched to"
 * dividers between them where the store holds them. The rows that open a one-turn turn are
 * matched to the turns in order from the session's first message, so `rows` must be all of
 * the session's rows. A turn the store has no row for yet comes last; a one-turn row with no
 * turn left shows as its stored text. Inputs: all stored rows, the turns and what turns a
 * turn into lines. Output: the lines in order. */
export function conversationLines(rows: readonly SessionMessage[], turns: readonly MasterTurn[], turnLines: (turn: MasterTurn) => ConsoleLine[]): ConsoleLine[] {
  const lines: ConsoleLine[] = [];
  let next = 0;
  let stored = true;
  for (const row of rows) {
    const oneTurn = row.mode === "one_turn" && row.speaker !== "switch";
    const opens = oneTurn && row.speaker !== "master";
    if (opens && next < turns.length) {
      lines.push(...turnLines(turns[next]));
      next += 1;
      stored = false;
      continue;
    }
    if (opens) stored = true;
    if (oneTurn && !stored) continue;
    const line = messageLine(row);
    if (line !== null) lines.push(line);
  }
  assert(next <= turns.length, "more turns were placed than the session has");
  for (const turn of turns.slice(next)) lines.push(...turnLines(turn));
  return lines;
}

/** One row of the plain message list: a divider, or who said it and the words. */
export type MessageRow = { key: string; kind: "switch"; text: string } | { key: string; kind: "owner" | "engine" | "master"; who: string; text: string };

/** The rows of the plain message list a session shows when opened: each message with who
 * said it, the owner as "You", the engine as "CrazyAgent" and the master by `ranBy`, and
 * each master switch as a divider. Inputs: the stored rows and the harness catalog, if read.
 * Output: the rows, oldest first. */
export function messageRows(rows: readonly SessionMessage[], harnesses: HarnessEntry[] | null): MessageRow[] {
  const shown: MessageRow[] = [];
  for (const row of rows) {
    const key = `m-${row.seq}`;
    if (row.speaker === "switch") {
      if (row.text !== null) shown.push({ key, kind: "switch", text: row.text });
    } else if (row.speaker === "owner") shown.push({ key, kind: "owner", who: "You", text: row.text ?? NO_TEXT });
    else if (row.text !== null) shown.push({ key, kind: row.speaker, who: row.speaker === "engine" ? "CrazyAgent" : ranBy(row.mode, row.harness, harnesses), text: row.text });
  }
  assert(shown.length <= rows.length, "more list rows than stored rows");
  return shown;
}

/** How many stored messages a session has, in words. Input: the count. Output: the words. */
export function storedCount(count: number): string {
  return count === 1 ? "1 stored message" : `${count} stored messages`;
}

/** The stored messages of one session as a view holds them: the rows oldest first, the count
 * of all stored rows, whether older rows exist that are not held, whether a read arrived,
 * whether every row of the session is held, whether a whole-session read stopped at
 * `MESSAGES_HELD_MAX`, a read problem and a read of the next older page. */
export type StoredMessages = { rows: SessionMessage[]; count: number; older: boolean; loaded: boolean; whole: boolean; capped: boolean; problem: string; earlier: () => Promise<void> };

/** Reads a session's stored messages and keeps them current: the newest page when the
 * session or the ticket changes and again on every change of `mark`, merged by sequence
 * number. With `all` it reads back to the session's first message, up to
 * `MESSAGES_HELD_MAX` rows. A failed read keeps the held rows and says why. Inputs: the
 * master ticket, the session id or null, a mark of the session's activity and whether to
 * read the whole session. Output: the held messages, empty until a read of this session
 * arrives, so one session's rows never show under another. */
export function useSessionMessages(ticket: string, sessionId: string | null, mark: string | null, all: boolean): StoredMessages {
  const [rows, setRows] = useState<SessionMessage[]>([]);
  const [source, setSource] = useState<string | null>(null);
  const [capped, setCapped] = useState(false);
  const [count, setCount] = useState(0);
  const [cursor, setCursor] = useState<number | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [problem, setProblem] = useState("");
  const run = useRef(0);
  const held = useRef<SessionMessage[]>([]);
  const oldest = useRef<number | null>(null);
  const marked = useRef(mark);

  const page = async (mine: number, before: number | null): Promise<MasterMessagePage | null> => {
    if (!ticket || sessionId === null) return null;
    const read = await readMessages(ticket, sessionId, before, MESSAGES_PER_READ);
    if (mine !== run.current) return null;
    const got = read.kind === "ok" ? pageOf(read.value) : null;
    if (got === null) {
      setProblem(read.kind === "error" ? failureCopy(read) : NOT_IN_BUILD);
      return null;
    }
    held.current = mergeMessages(held.current, got.messages).slice(-MESSAGES_HELD_MAX);
    setRows(held.current);
    setSource(sessionId);
    setCount(got.count);
    setProblem("");
    return got;
  };

  const older = async (mine: number, from: number | null) => {
    let before = from;
    for (let reads = 0; before !== null && held.current.length < MESSAGES_HELD_MAX && reads < MESSAGES_HELD_MAX / MESSAGES_PER_READ; reads += 1) {
      const got = await page(mine, before);
      if (got === null) return;
      before = got.next;
      oldest.current = before;
      setCursor(before);
      if (!all) return;
    }
    if (all && mine === run.current) setCapped(before !== null);
  };

  const newest = async (mine: number, first: boolean) => {
    const got = await page(mine, null);
    if (got === null) return;
    setLoaded(true);
    const joined = first || got.messages.length === 0 || held.current.some((row) => row.seq === got.messages[0].seq - 1) || got.messages[0].seq === 1;
    if (first || !joined) {
      if (!joined) held.current = got.messages;
      oldest.current = got.next;
      setRows(held.current);
      setCursor(got.next);
      if (all) await older(mine, got.next);
    }
  };

  useEffect(() => {
    run.current += 1;
    held.current = [];
    oldest.current = null;
    setRows([]);
    setCount(0);
    setCursor(null);
    setLoaded(false);
    setCapped(false);
    setProblem("");
    void newest(run.current, true);
  }, [ticket, sessionId]);

  useEffect(() => {
    if (marked.current === mark) return;
    marked.current = mark;
    void newest(run.current, false);
  }, [mark]);

  const mine = source === sessionId && sessionId !== null;
  return {
    rows: mine ? rows : [], count: mine ? count : 0, older: mine && cursor !== null, loaded: mine && loaded,
    whole: mine && loaded && cursor === null, capped: mine && capped, problem,
    earlier: () => older(run.current, oldest.current),
  };
}

/** The plain list of a session's messages. Input: the rows. Output: the list. */
export function MessageList({ rows }: { rows: readonly MessageRow[] }) {
  return <ol className="pj-message-list" aria-label="Session messages">
    {rows.map((row) => row.kind === "switch"
      ? <li key={row.key} className="pj-message switch pj-small pj-muted" role="separator" aria-label={displayText(row.text)} data-testid="message-switch">{displayText(row.text)}</li>
      : <li key={row.key} className={`pj-message ${row.kind}`} data-testid="session-message">
        <span className="pj-small pj-muted">{displayText(row.who)}</span>
        <span className="pj-note-text">{displayText(row.text)}</span>
      </li>)}
  </ol>;
}

/** The stored messages of one session, read when shown: the newest page first, with Show
 * earlier messages while older ones exist. Inputs: the master ticket, the session id, a mark
 * whose change reads the newest messages again and the harness catalog, if read. Output:
 * the list, a line when the session has no stored message, or the read's problem. */
export function SessionMessages({ ticket, sessionId, mark = null, harnesses }: { ticket: string; sessionId: string; mark?: string | null; harnesses: HarnessEntry[] | null }) {
  const stored = useSessionMessages(ticket, sessionId, mark, false);
  return <div className="pj-messages" data-testid="session-messages">
    {stored.older && <button type="button" className="btn quiet small" onClick={() => void stored.earlier()} data-testid="messages-earlier">Show earlier messages</button>}
    {stored.problem && <p className="pj-error" role="alert">{stored.problem}</p>}
    {!stored.loaded && !stored.problem && <p className="pj-small pj-muted" role="status">Reading the messages...</p>}
    {stored.loaded && stored.rows.length === 0 && <p className="pj-small pj-muted">No stored messages in this session.</p>}
    {stored.rows.length > 0 && <MessageList rows={messageRows(stored.rows, harnesses)} />}
  </div>;
}

/** Whether the conversation of a live terminal session starts shown: only when the store
 * holds something the terminal does not show, a master switch or a one-turn message.
 * Input: the held rows. Output: true to start shown. */
export function startsShown(rows: readonly SessionMessage[]): boolean {
  return rows.some((row) => row.speaker === "switch" || row.mode === "one_turn");
}

/** The stored conversation of the live terminal master's session, above its terminal, so
 * the turns of an earlier master stay readable after a switch: a toggle that names the
 * count, then the list. Nothing shows for a session with no stored message. Inputs: the
 * master ticket, the live session id or null, a mark of the master's state and the harness
 * catalog, if read. Output: the panel or nothing. */
export function LiveConversation({ ticket, sessionId, mark, harnesses }: { ticket: string; sessionId: string | null; mark: string | null; harnesses: HarnessEntry[] | null }) {
  const stored = useSessionMessages(ticket, sessionId, mark, false);
  const [chosen, setChosen] = useState<boolean | null>(null);
  if (sessionId === null || !stored.loaded || stored.count === 0) return null;
  const shown = chosen ?? startsShown(stored.rows);
  return <div className="pj-live-conversation" data-testid="live-conversation">
    <button type="button" className="btn quiet small" aria-expanded={shown} onClick={() => setChosen(!shown)} data-testid="live-conversation-toggle">
      {shown ? "Hide" : "Show"} conversation ({storedCount(stored.count)})</button>
    {shown && <div className="pj-messages">
      {stored.older && <button type="button" className="btn quiet small" onClick={() => void stored.earlier()} data-testid="messages-earlier">Show earlier messages</button>}
      <MessageList rows={messageRows(stored.rows, harnesses)} />
    </div>}
  </div>;
}
