import { useEffect, useRef, useState } from "react";
import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { deleteSession, failureCopy, readOwnerLines, readSessions, receiptState, resumeSession, startNewSession } from "../../api-projects";
import type { ReceiptState } from "../../api-projects";
import type { IntentReceipt, MasterSessionSummary, MasterTurn, TeamStatus } from "../../engine-client";
import { displayText } from "../conversation";
import { Badge } from "./shared";

/** When a session started, in words. Inputs: its start time in milliseconds, absent for
 * sessions older than the engine's marks, and now. Output: a short label. */
export function startedLabel(ms: number | null, now: number): string {
  if (ms === null) return "Started earlier";
  const minutes = Math.floor(Math.max(0, now - ms) / 60000);
  if (minutes < 1) return "Started just now";
  if (minutes < 60) return `Started ${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Started ${hours} h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "Started yesterday" : `Started ${days} days ago`;
}

/** The line a session shows in the list: its newest line, else its first owner line.
 * Input: the session. Output: the line, or a placeholder before any message. */
export function sessionLine(session: MasterSessionSummary): string {
  return session.last_line ?? session.first_line ?? "No messages yet";
}

/** Names the agents a session used. Input: their names. Output: one line, or null for none. */
export function agentsLine(agents: string[]): string | null {
  return agents.length === 0 ? null : `Agents: ${agents.join(", ")}`;
}

/** The badge for sub-agent results queued for a parked session. Input: the count.
 * Output: the badge words, or null when nothing waits. */
export function waitingBadge(count: number): string | null {
  if (count <= 0) return null;
  return count === 1 ? "1 result waiting" : `${count} results waiting`;
}

/** The live session of a list, the one in the engine's `configured` phase.
 * Input: the sessions. Output: the live one, or undefined. */
export function liveSession(sessions: MasterSessionSummary[]): MasterSessionSummary | undefined {
  return sessions.find((session) => session.phase === "configured");
}

/** Tells whether the live session has no owner turn yet: the view shows a live session the
 * engine's Sessions list, which leaves out sessions with no owner turn, does not hold.
 * Inputs: the live session id as the view read it, the listed sessions and whether a list
 * read arrived. Output: true only after a read that leaves the live session out. */
export function liveUnused(liveId: string | null, sessions: MasterSessionSummary[], listed: boolean): boolean {
  return listed && liveId !== null && !sessions.some((session) => session.session_id === liveId);
}

/** Sessions the owner can resume: every listed one but the live and the closed.
 * Input: the sessions. Output: the resumable ones, in list order. */
export function resumable(sessions: MasterSessionSummary[]): MasterSessionSummary[] {
  return sessions.filter((session) => session.phase === "parked");
}

/** What a New session or Resume receipt means for the view: done, the owner must confirm
 * stopping the running reply first, or a plain problem. Input: the receipt state. */
export function switchOutcome(state: ReceiptState): { kind: "done" } | { kind: "confirm" } | { kind: "problem"; code: string } {
  if (state.kind === "applied") return { kind: "done" };
  if (state.kind === "rejected" && state.code === "master_turn_running") return { kind: "confirm" };
  if (state.kind === "rejected") return { kind: "problem", code: state.code };
  return { kind: "problem", code: "master_outcome_pending" };
}

/** The most owner lines one read asks the engine for. */
export const OWNER_LINES_PER_READ = 32;

/** Input cursors of the turns whose owner message the view holds in neither the lines this
 * window sent nor the lines read before, newest kept, at most `OWNER_LINES_PER_READ`.
 * Inputs: the turns, the sent lines by turn id and the read lines by input cursor. */
export function missingInputs(turns: MasterTurn[], sent: Record<string, string>, lines: Record<string, string>): string[] {
  const missing = turns
    .filter((turn) => sent[turn.id] === undefined && turn.input_cursor !== undefined && lines[turn.input_cursor] === undefined)
    .map((turn) => turn.input_cursor as string);
  return missing.slice(-OWNER_LINES_PER_READ);
}

/** The owner's message of a turn: the one this window sent, else the one read from the
 * engine. Inputs: the turn, the sent lines and the read lines. Output: the text or undefined. */
export function ownerText(turn: MasterTurn, sent: Record<string, string>, lines: Record<string, string>): string | undefined {
  return sent[turn.id] ?? (turn.input_cursor === undefined ? undefined : lines[turn.input_cursor]);
}

/** Reads the owner lines a session's turns are missing and returns the held lines with them.
 * Inputs: the master ticket, the session id, its turns, the sent and the held lines.
 * Output: the lines by input cursor; the held ones when nothing is missing or the read fails. */
export async function withOwnerLines(ticket: string, session: string, turns: MasterTurn[], sent: Record<string, string>, lines: Record<string, string>): Promise<Record<string, string>> {
  const missing = missingInputs(turns, sent, lines);
  if (missing.length === 0) return lines;
  const read = await readOwnerLines(ticket, session, missing);
  if (read.kind !== "ok") return lines;
  const next = { ...lines };
  for (const entry of read.value.entries) next[entry.input_cursor] = entry.text;
  return next;
}

/** The words for a refused New session or Resume. A session opened before the engine or
 * its settings changed cannot continue, so Resume says to start a new one. Inputs: which
 * switch was refused and the engine's code. Output: one plain sentence. */
export function switchProblem(kind: "new" | "resume" | "delete", code: string): string {
  if (kind === "resume" && code === "master_source_conflict") {
    return "This session started before the engine or its settings changed, so it can't continue. Start a new session.";
  }
  return failureCopy({ code });
}

/** The divider a session's conversation shows where its master harness switched: the
 * engine's note for the live session's latest switch. Input: the latest team status read,
 * if any. Output: the note, or null when the live session never switched or no read came. */
export function switchDivider(team: Result<TeamStatus> | undefined): string | null {
  return team?.kind === "ok" ? team.value.live_switch?.note ?? null : null;
}

/** The "Switched to <harness>" divider, for the live master area above the new harness's
 * terminal. Input: the latest team status read. Output: the divider, or nothing when the
 * live session never switched. */
export function SwitchDivider({ team }: { team: Result<TeamStatus> | undefined }) {
  const note = switchDivider(team);
  if (note === null) return null;
  return <div className="pj-row pj-small pj-muted" role="separator" aria-label={displayText(note)} data-testid="harness-switch">
    <span>{displayText(note)}</span>
  </div>;
}

type Pending = { kind: "new" } | { kind: "resume"; session: MasterSessionSummary } | { kind: "delete"; session: MasterSessionSummary };

/** The master view's session header: the live session, a visible New session button and
 * the Sessions list, where clicking a parked session resumes it and Delete removes it after
 * the owner confirms. A running reply is stopped only after the owner confirms. New session
 * does nothing while the live session has no owner turn; only the newest list read shows,
 * and a list read that works clears the problem a refused list read showed, never the
 * problem of a switch. Inputs: the master ticket, the live session id as the view read it, a mark of the
 * master's turns as the view last read them, whose every change reads the list again so
 * its message count follows each turn, and a refresh for the view after a switch. */
export function SessionsBar({ ticket, liveId, mark = null, onSwitched }: { ticket: string; liveId: string | null; mark?: string | null; onSwitched: () => Promise<void> }) {
  const [sessions, setSessions] = useState<MasterSessionSummary[]>([]);
  const [listed, setListed] = useState(false);
  const [shown, setShown] = useState(false);
  const [working, setWorking] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  const [problem, setProblem] = useState("");
  const [readProblem, setReadProblem] = useState("");
  const reads = useRef(0);
  const marked = useRef(mark);
  const now = Date.now();

  async function load() {
    const mine = ++reads.current;
    if (!ticket) return;
    const read = await readSessions(ticket);
    if (mine !== reads.current) return;
    if (read.kind === "ok") {
      setSessions(read.value.sessions);
      setListed(true);
      setReadProblem("");
    } else if (read.kind === "error") setReadProblem(failureCopy(read));
  }

  useEffect(() => {
    setSessions([]);
    setListed(false);
    void load();
  }, [ticket, liveId]);

  useEffect(() => {
    if (marked.current === mark) return;
    marked.current = mark;
    void load();
  }, [mark]);

  async function run(next: Pending, cancelRunning: boolean) {
    if (working || !ticket) return;
    setWorking(true);
    setProblem("");
    const sent: Result<IntentReceipt> = next.kind === "new"
      ? await startNewSession(ticket, cancelRunning, nextGeneration())
      : next.kind === "resume"
        ? await resumeSession(ticket, next.session.session_id, cancelRunning, nextGeneration())
        : await deleteSession(ticket, next.session.session_id, nextGeneration());
    if (sent.kind === "ok") {
      const outcome = switchOutcome(receiptState(sent.value));
      if (outcome.kind === "confirm") setPending(next);
      else {
        setPending(null);
        if (outcome.kind === "problem") setProblem(switchProblem(next.kind, outcome.code));
        else if (next.kind !== "delete") setShown(false);
        if (next.kind !== "delete") await onSwitched().catch(() => undefined);
        await load();
      }
    } else if (sent.kind === "error") setProblem(failureCopy(sent));
    else setProblem("Not ready yet: sessions aren't in this build.");
    setWorking(false);
  }

  const live = liveSession(sessions);
  const unused = liveUnused(liveId, sessions, listed);
  const others = resumable(sessions);
  return <div className="pj-sessions" data-testid="master-sessions">
    <div className="pj-row between">
      <span className="pj-small pj-muted" data-testid="master-live-session">
        {live ? `Live session · ${startedLabel(live.started_ms, now)} · ${live.turns} ${live.turns === 1 ? "message" : "messages"}` : unused ? "Live session · no messages yet" : "No live session"}
      </span>
      <span className="pj-row">
        <button type="button" className="btn" disabled={working || !ticket || unused} title={unused ? "This session has no messages yet, so it already is a new session." : undefined} onClick={() => void run({ kind: "new" }, false)} data-testid="new-session">New session</button>
        <button type="button" className="btn" aria-expanded={shown} disabled={!ticket} onClick={() => { setShown(!shown); if (!shown) void load(); }} data-testid="sessions-toggle">
          Sessions{others.length > 0 ? ` (${others.length})` : ""}</button>
      </span>
    </div>
    {pending && pending.kind !== "delete" && <div className="pj-note warn" role="alert">
      <p>The master is still answering. Stop that reply and {pending.kind === "new" ? "start a new session" : "switch sessions"}? Its earlier turns stay in the session.</p>
      <span className="pj-row">
        <button type="button" className="btn primary" disabled={working} onClick={() => void run(pending, true)} data-testid="confirm-switch">Stop reply and continue</button>
        <button type="button" className="btn" disabled={working} onClick={() => setPending(null)}>Keep it running</button>
      </span>
    </div>}
    {pending && pending.kind === "delete" && <div className="pj-note warn" role="alert">
      <p>Delete the session {startedLabel(pending.session.started_ms, now).toLowerCase()}? Its messages and its transcript are removed for good.</p>
      <span className="pj-row">
        <button type="button" className="btn primary" disabled={working} onClick={() => void run(pending, false)} data-testid="confirm-delete">Delete session</button>
        <button type="button" className="btn" disabled={working} onClick={() => setPending(null)}>Keep it</button>
      </span>
    </div>}
    {(problem || readProblem) && <p className="pj-error" role="alert">{problem || readProblem}</p>}
    {shown && <ul className="pj-session-list" aria-label="Sessions">
      {others.length === 0 && <li className="pj-small pj-muted">No other sessions yet.</li>}
      {others.map((session) => {
        const waiting = waitingBadge(session.results_waiting);
        const agents = agentsLine(session.agents);
        return <li key={session.session_id} className="pj-session-item">
          <button type="button" className="pj-session" disabled={working} onClick={() => void run({ kind: "resume", session }, false)} data-testid="resume-session">
            <span className="pj-row between"><span className="pj-small">{startedLabel(session.started_ms, now)}</span>
              {waiting && <Badge word={waiting} tone="info" />}</span>
            <span className="pj-small">{displayText(sessionLine(session))}</span>
            {agents && <span className="pj-small pj-muted">{displayText(agents)}</span>}
          </button>
          <button type="button" className="btn quiet small" disabled={working} aria-label={`Delete the session ${startedLabel(session.started_ms, now).toLowerCase()}`} onClick={() => setPending({ kind: "delete", session })} data-testid="delete-session">Delete</button>
        </li>;
      })}
    </ul>}
  </div>;
}
