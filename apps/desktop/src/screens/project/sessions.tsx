import { useEffect, useState } from "react";
import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { failureCopy, readSessions, receiptState, resumeSession, startNewSession } from "../../api-projects";
import type { ReceiptState } from "../../api-projects";
import type { IntentReceipt, MasterSessionSummary } from "../../engine-client";
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

type Pending = { kind: "new" } | { kind: "resume"; session: MasterSessionSummary };

/** The master view's session header: the live session, a visible New session button and
 * the Sessions list, where clicking a parked session resumes it. A running reply is
 * stopped only after the owner confirms. Inputs: the master ticket, the live session id
 * as the view read it, and a refresh for the view after a switch. */
export function SessionsBar({ ticket, liveId, onSwitched }: { ticket: string; liveId: string | null; onSwitched: () => Promise<void> }) {
  const [sessions, setSessions] = useState<MasterSessionSummary[]>([]);
  const [shown, setShown] = useState(false);
  const [working, setWorking] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  const [problem, setProblem] = useState("");
  const now = Date.now();

  async function load() {
    if (!ticket) return;
    const read = await readSessions(ticket);
    if (read.kind === "ok") setSessions(read.value.sessions);
    else if (read.kind === "error") setProblem(failureCopy(read));
  }

  useEffect(() => {
    setSessions([]);
    void load();
  }, [ticket, liveId]);

  async function run(next: Pending, cancelRunning: boolean) {
    if (working || !ticket) return;
    setWorking(true);
    setProblem("");
    const sent: Result<IntentReceipt> = next.kind === "new"
      ? await startNewSession(ticket, cancelRunning, nextGeneration())
      : await resumeSession(ticket, next.session.session_id, cancelRunning, nextGeneration());
    if (sent.kind === "ok") {
      const outcome = switchOutcome(receiptState(sent.value));
      if (outcome.kind === "confirm") setPending(next);
      else {
        setPending(null);
        if (outcome.kind === "problem") setProblem(failureCopy(outcome));
        else setShown(false);
        await onSwitched().catch(() => undefined);
        await load();
      }
    } else if (sent.kind === "error") setProblem(failureCopy(sent));
    else setProblem("Not ready yet: sessions aren't in this build.");
    setWorking(false);
  }

  const live = liveSession(sessions);
  const others = resumable(sessions);
  return <div className="pj-sessions" data-testid="master-sessions">
    <div className="pj-row between">
      <span className="pj-small pj-muted" data-testid="master-live-session">
        {live ? `Live session · ${startedLabel(live.started_ms, now)} · ${live.turns} ${live.turns === 1 ? "message" : "messages"}` : "No live session"}
      </span>
      <span className="pj-row">
        <button type="button" className="btn" disabled={working || !ticket} onClick={() => void run({ kind: "new" }, false)} data-testid="new-session">New session</button>
        <button type="button" className="btn" aria-expanded={shown} disabled={!ticket} onClick={() => { setShown(!shown); if (!shown) void load(); }} data-testid="sessions-toggle">
          Sessions{others.length > 0 ? ` (${others.length})` : ""}</button>
      </span>
    </div>
    {pending && <div className="pj-note warn" role="alert">
      <p>The master is still answering. Stop that reply and {pending.kind === "new" ? "start a new session" : "switch sessions"}? Its earlier turns stay in the session.</p>
      <span className="pj-row">
        <button type="button" className="btn primary" disabled={working} onClick={() => void run(pending, true)} data-testid="confirm-switch">Stop reply and continue</button>
        <button type="button" className="btn" disabled={working} onClick={() => setPending(null)}>Keep it running</button>
      </span>
    </div>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    {shown && <ul className="pj-session-list" aria-label="Sessions">
      {others.length === 0 && <li className="pj-small pj-muted">No other sessions yet.</li>}
      {others.map((session) => {
        const waiting = waitingBadge(session.results_waiting);
        const agents = agentsLine(session.agents);
        return <li key={session.session_id}>
          <button type="button" className="pj-session" disabled={working} onClick={() => void run({ kind: "resume", session }, false)} data-testid="resume-session">
            <span className="pj-row between"><span className="pj-small">{startedLabel(session.started_ms, now)}</span>
              {waiting && <Badge word={waiting} tone="info" />}</span>
            <span className="pj-small">{displayText(sessionLine(session))}</span>
            {agents && <span className="pj-small pj-muted">{displayText(agents)}</span>}
          </button>
        </li>;
      })}
    </ul>}
  </div>;
}
