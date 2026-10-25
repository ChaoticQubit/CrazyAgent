import { useEffect, useRef, useState } from "react";
import { allMasterTurns, closeMaster, errorMessage, masterState, openMaster, submitMaster } from "../engine-client";
import type { IntentReceipt, MasterState, MasterTurn } from "../engine-client";
import type { useOwnerBrowser } from "../state";

export function displayText(text: string): string {
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/gu,
    (value) => `\\u${value.codePointAt(0)!.toString(16).padStart(4, "0")}`);
}

export function ProjectList({ state }: { state: ReturnType<typeof useOwnerBrowser> }) {
  return <div className="baseline-list">
      <div className="section-heading"><h2>Projects</h2><button disabled={state.busy || !state.connected} onClick={() => void state.refreshProjects()}>Refresh projects</button></div>
      <p className="muted">{state.projectPage?.fresh ? "Projects from the engine" : "Project list is stale or unavailable"}</p>
      {state.projectPage?.items.map((row) => <button className={`baseline-row ${state.projectId === row.id ? "active" : ""}`} key={row.id} disabled={!state.connected} data-project-id={row.id} onClick={() => void state.selectProject(row.id)}><span>{displayText(row.name)}</span><span className="mono">{row.id}</span><span className="muted">Declared repositories: {row.repository_count}</span></button>)}
      {state.projectPage?.items.length === 0 && <p>No projects were returned.</p>}
      {state.projectPage?.has_more && <button disabled={state.busy || !state.connected} onClick={() => void state.refreshProjects(true)}>Next project page</button>}
    </div>;
}

export function Conversation({ state }: { state: ReturnType<typeof useOwnerBrowser> }) {
  const draft = state.drafts[state.projectId];
  const project = state.project?.project_id === state.projectId ? state.project : undefined;
  const entries = state.historyPage?.project_id === state.projectId ? state.historyPage : undefined;
  const blocked = state.busy || state.pending || !state.connected || !project?.fresh;
  const bytes = (text: string) => new TextEncoder().encode(text).length;
  return <section className="baseline-browser conversation-browser" aria-busy={state.busy}>
    <ProjectList state={state} />
    <div className="inspection">
      {!state.projectId ? <><h2>Select a project</h2><p className="muted">Read its actual conversation and versioned brief.</p></> : <>
        <div className="section-heading"><h2>{displayText(state.projectPage?.items.find((row) => row.id === state.projectId)?.name ?? state.projectId)}</h2><button disabled={state.busy || !state.connected} onClick={() => void state.refreshProject()} data-testid="inspect-project">Inspect current brief</button></div>
        <p className="muted" data-testid="conversation-freshness">{project?.fresh ? `Brief read at revision ${project.revision}` : "Brief data is stale or unavailable. Inspect again before submitting."}</p>
        <MasterPanel state={state} ticket={project?.ticket ?? ""} />
        <section><h3>Current brief {project?.brief ? `(version ${project.brief.version})` : ""}</h3><pre data-testid="current-project-brief">{project ? project.brief ? displayText(project.brief.text) : "No brief has been recorded." : "Current brief unavailable."}</pre></section>
        <section className="conversation-history">
          <div className="section-heading"><h3>Recorded history</h3><button disabled={state.busy || !state.connected || !project} onClick={() => void state.refreshHistory()} data-testid="refresh-conversation">First history page</button></div>
          <p className="muted">{entries?.fresh ? `History read at revision ${entries.revision}; one complete entry per page` : "History is stale or unavailable"}</p>
          {entries?.entries.length === 0 && <p>No entries were returned.</p>}
          {entries?.entries.map((entry) => <article key={entry.event} className="check"><p className="muted">{entry.kind === "brief" ? `Brief version ${entry.version}` : "Owner message"} · {displayText(entry.origin)} · event {entry.event} · revision {entry.revision}</p><pre data-testid="conversation-entry-text">{displayText(entry.text)}</pre></article>)}
          {entries?.has_more && <button disabled={state.busy || !state.connected} onClick={() => void state.refreshHistory(true)} data-testid="next-conversation">Next history page</button>}
        </section>
        <section className="conversation-editor"><h3>Owner message</h3><label htmlFor="conversation-message">Message for this project</label><textarea id="conversation-message" data-testid="conversation-message" value={draft?.message.text ?? ""} onChange={(event) => state.edit("message", event.target.value)} rows={5} /><p className="muted">{bytes(draft?.message.text ?? "")} / 8192 UTF-8 bytes. Sending a message does not change the brief, grant approval or start work.</p><button disabled={blocked || !draft?.message.text.trim()} onClick={() => void state.submit("message")} data-testid="send-conversation-message">Send owner message</button></section>
        <section className="conversation-editor"><h3>Explicit brief replacement</h3><button disabled={state.busy || !project?.fresh} onClick={state.loadBrief} data-testid="load-current-brief">Load current brief for editing</button><p className="muted">{draft?.brief.ticket ? `Editor based on version ${draft.brief.version}.` : "Load the current brief to bind this editor to its inspected version."} Refreshing never rebases this draft automatically.</p><label htmlFor="conversation-brief">Replacement brief</label><textarea id="conversation-brief" data-testid="conversation-brief" value={draft?.brief.text ?? ""} onChange={(event) => state.edit("brief", event.target.value)} rows={6} /><p className="muted">{bytes(draft?.brief.text ?? "")} / 8192 UTF-8 bytes. This explicit action replaces only the inspected brief version.</p><button disabled={blocked || !draft?.brief.text.trim() || draft.brief.ticket !== project?.ticket} onClick={() => void state.submit("brief")} data-testid="replace-project-brief">Replace inspected brief</button></section>
        <button disabled={state.pending} onClick={state.discardDrafts}>Discard this project's local drafts</button>
        <p className="muted">Drafts live in this desktop UI process. Exact pending intent lives in this native process; neither survives full process loss by this guarantee. Unsafe controls are displayed as visible Unicode escapes.</p>
      </>}
    </div>
  </section>;
}

const settled: MasterTurn["phase"][] = ["succeeded", "failed", "cancelled", "uncertain", "blocked"];

/**
 * Chooses which master lifecycle buttons the panel shows.
 * Input: the current session phase, or undefined when no session exists.
 * Output: whether Start master and Close master are offered.
 */
export function masterControls(phase: string | undefined): { start: boolean; close: boolean } {
  const open = phase === "configured";
  return { start: !open, close: open };
}

/** Plain-language label for one master turn phase.
 * Input: the engine turn phase and optional reason. Output: a short status sentence. */
export function turnStatus(turn: MasterTurn): string {
  if (turn.phase === "blocked" && turn.reason === "readiness_unknown")
    return "Blocked: not signed in. Sign in to this account in Settings, then send again";
  if (turn.phase === "blocked" && turn.reason === "readiness_stale")
    return "Blocked: sign-in is out of date. Sign in again in Settings, then send again";
  if (turn.phase === "blocked" && turn.reason === "connection_changed")
    return "Blocked: settings or the engine changed since the master opened. Close it, open it again, then send again";
  const reason = turn.reason ? ` (${turn.reason.replaceAll("_", " ")})` : "";
  switch (turn.phase) {
    case "queued": case "prepared": case "delivering": return "Waiting to start";
    case "running": return "Thinking";
    case "waiting_approval": return "Waiting for your approval";
    case "succeeded": return "Replied";
    case "failed": return `Failed${reason}`;
    case "cancelled": return "Cancelled";
    case "blocked": return `Blocked${reason}`;
    case "uncertain": return `Outcome unclear${reason}`;
  }
}

/** Project master conversation: start the master, send messages and read replies.
 * Inputs: the owner browser state and the selected project ticket. Output: the panel,
 * which polls the engine every two seconds while a turn is still in progress. */
export function MasterPanel({ state, ticket }: { state: ReturnType<typeof useOwnerBrowser>; ticket: string }) {
  const [master, setMaster] = useState<MasterState>();
  const [turns, setTurns] = useState<MasterTurn[]>([]);
  const [sent, setSent] = useState<Record<string, string>>({});
  const [text, setText] = useState("");
  const [working, setWorking] = useState(false);
  const [problem, setProblem] = useState("");
  const current = useRef(ticket);
  current.current = ticket;
  const session = master?.ticket === ticket ? master.session : null;
  const active = session?.phase === "configured";
  const waiting = turns.some((turn) => !settled.includes(turn.phase));
  const unseen = Object.keys(sent).some((id) => !turns.some((turn) => turn.id === id));

  async function refresh() {
    if (!ticket || !state.connected) return;
    const next = await masterState(ticket);
    if (current.current !== ticket) return;
    setMaster(next);
    if (next.session) {
      const session = next.session.session_id;
      const read = await allMasterTurns(ticket, session);
      if (current.current === ticket) setTurns(read);
    } else setTurns([]);
  }

  async function act(action: () => Promise<void>) {
    if (working) return;
    setWorking(true);
    setProblem("");
    try { await action(); } catch (failure) { setProblem(errorMessage(failure)); }
    setWorking(false);
  }

  useEffect(() => {
    setMaster(undefined); setTurns([]); setSent({}); setProblem("");
    if (ticket && state.connected) void act(refresh);
  }, [ticket, state.connected]);

  useEffect(() => {
    if (!waiting && !unseen && !(master && !session && working)) return;
    const timer = window.setInterval(() => { void refresh().catch((failure) => setProblem(errorMessage(failure))); }, 2000);
    return () => window.clearInterval(timer);
  }, [waiting, unseen, ticket, session?.session_id]);

  function confirm(receipt: IntentReceipt) {
    if (receipt.rejection) throw { code: receipt.rejection };
    if (receipt.outcome !== "applied") throw { code: receipt.pending ? "master_outcome_pending" : receipt.outcome };
  }

  const start = () => act(async () => {
    const generation = state.nextGeneration();
    const receipt = await openMaster(ticket, generation);
    confirm(receipt);
    await refresh();
  });

  const close = () => act(async () => {
    if (!session) return;
    const generation = state.nextGeneration();
    const receipt = await closeMaster(ticket, session.session_id, generation);
    confirm(receipt);
    await refresh();
  });

  const send = () => act(async () => {
    if (!session) return;
    const message = text.trim();
    const generation = state.nextGeneration();
    const receipt = await submitMaster(ticket, session.session_id, message, generation);
    confirm(receipt);
    setSent((prior) => ({ ...prior, [receipt.operation_id]: message }));
    setText("");
    await refresh();
  });

  const blocked = working || !state.connected || !ticket;
  const controls = masterControls(session?.phase);
  return <section aria-label="Project master" className="master-panel">
    <div className="section-heading"><h3>Master conversation</h3>
      <button type="button" disabled={blocked} onClick={() => void act(refresh)} data-testid="refresh-master">Refresh</button></div>
    {!session && <p role="status" data-testid="master-availability">The master has not started for this project. It uses the account pool you assigned to this project.</p>}
    {session && !active && <p role="status" data-testid="master-availability">This master conversation is {session.phase}. Start master opens a new one.</p>}
    {controls.start && <button type="button" disabled={blocked} onClick={() => void start()} data-testid="select-project-master">Start master</button>}
    {controls.close && <button type="button" disabled={blocked} onClick={() => void close()} data-testid="close-project-master">Close master</button>}
    {turns.map((turn) => <article key={turn.id} className="check" data-testid="master-turn">
      {sent[turn.id] && <p><strong>You:</strong> {displayText(sent[turn.id])}</p>}
      <p className="muted">Message {turn.ordinal} · {turnStatus(turn)}</p>
      {turn.reply && <pre data-testid="master-reply">{displayText(turn.reply.text)}</pre>}
    </article>)}
    {active && <div className="conversation-editor">
      <label htmlFor="master-message">Message the master</label>
      <textarea id="master-message" data-testid="master-message" value={text} rows={4} onChange={(event) => setText(event.target.value)} />
      <button type="button" disabled={blocked || waiting || !text.trim() || new TextEncoder().encode(text).length > 8192} onClick={() => void send()} data-testid="send-master-message">{waiting ? "Waiting for reply" : "Send to master"}</button>
    </div>}
    {problem && <p role="alert" className="error" data-testid="master-problem">{problem}</p>}
  </section>;
}
