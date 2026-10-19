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
        <div className="section-heading"><h2>{state.projectId}</h2><button disabled={state.busy || !state.connected} onClick={() => void state.refreshProject()} data-testid="inspect-project">Inspect current brief</button></div>
        <p className="muted" data-testid="conversation-freshness">{project?.fresh ? `Brief read at revision ${project.revision}` : "Brief data is stale or unavailable. Inspect again before submitting."}</p>
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
        <p className="muted">Drafts live in this desktop UI process. Exact pending intent lives in this native process; neither survives full process loss by this guarantee. Leader replies and execution are unavailable until a real leader is connected. Unsafe controls are displayed as visible Unicode escapes.</p>
      </>}
    </div>
  </section>;
}
