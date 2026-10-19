import { useOwnerBrowser } from "./state";
import { BaselineInspection } from "./screens/baseline";
import { Conversation, displayText } from "./screens/conversation";
import { ModelAccess } from "./screens/model-access";
import { Projects } from "./screens/projects";
import type { BaselinePage } from "./engine-client";

function Sidebar({ connected, view, approvals, conversation, settings, projects }: { connected: boolean; view: string; approvals: () => Promise<void>; conversation: () => Promise<void>; settings: () => Promise<void>; projects: () => Promise<void> }) {
  return <aside className="sidebar">
    <div className="brand"><span className="brand-mark" aria-hidden="true">C</span><strong>CrazyAgent</strong></div>
    <p className="eyebrow">Workspace</p>
    <nav aria-label="Workspace views">
      <button className={`nav-item ${view === "approvals" ? "selected" : ""}`} aria-current={view === "approvals" ? "page" : undefined} onClick={() => void approvals()}>Approvals</button>
      <button className={`nav-item ${view === "conversation" ? "selected" : ""}`} aria-current={view === "conversation" ? "page" : undefined} onClick={() => void conversation()}>Conversation</button>
      <button className={`nav-item ${view === "settings" ? "selected" : ""}`} aria-current={view === "settings" ? "page" : undefined} onClick={() => void settings()}>Settings</button>
      <button className={`nav-item ${view === "projects" ? "selected" : ""}`} aria-current={view === "projects" ? "page" : undefined} onClick={() => void projects()}>Projects</button>
      {["Knowledge", "Supervision"].map((name) => (
        <button className="nav-item" key={name} disabled title="Unavailable in this build">{name}</button>
      ))}
    </nav>
    <div className="sidebar-footer"><span className={`status-dot ${connected ? "connected" : ""}`} />{connected ? "Owner connected" : "Owner disconnected"}</div>
  </aside>;
}

function BaselineList({ page, selected, busy, connected, refresh, select }: {
  page: BaselinePage | undefined;
  selected: string | undefined;
  busy: boolean;
  connected: boolean;
  refresh: (next?: boolean) => Promise<void>;
  select: (id: string) => Promise<void>;
}) {
  return <div className="baseline-list">
    <div className="section-heading"><h2>Baselines</h2><button disabled={busy || !connected} onClick={() => void refresh()} data-testid="refresh-baselines">Refresh</button></div>
    <p className="muted">{page?.fresh ? "Snapshot from the engine" : "Refresh to read current state"}</p>
    {page?.items.length === 0 && <p className="empty">No baselines were returned by the engine.</p>}
    {page?.items.map((row) => (
      <button className={`baseline-row ${selected === row.id ? "active" : ""}`} key={row.id} disabled={busy || !connected} onClick={() => void select(row.id)} data-baseline-id={row.id}>
        <span>{row.project_id}</span><span className="mono">{row.id}</span><span className="muted">{row.revoked ? "Revoked" : row.approved ? "Approved" : "Awaiting approval"}</span>
      </button>
    ))}
    {page?.has_more && <button disabled={busy} onClick={() => void refresh(true)}>Next page</button>}
  </div>;
}

export function App() {
  const state = useOwnerBrowser();
  return <OwnerView state={state} />;
}

export function OwnerView({ state }: { state: ReturnType<typeof useOwnerBrowser> }) {
  return <div className="app-shell">
    <Sidebar connected={state.connected} view={state.view} approvals={state.showApprovals} conversation={state.showConversation} settings={state.showSettings} projects={state.showProjects} />
    <main>
      <header className="page-header">
        <div><p className="eyebrow">Human decisions</p><h1>{state.view === "settings" ? "Settings" : state.view === "projects" ? "Projects" : state.view === "conversation" ? "Project conversation" : "Approvals"}</h1><p className="muted">{state.view === "settings" ? "Model access metadata, published by the authenticated engine owner." : state.view === "projects" ? "Declare explicit local repositories without starting work." : state.view === "conversation" ? "Durable owner messages and explicit versioned briefs from the engine." : "Inspect the engine's verification contract before granting approval."}</p></div>
        <button onClick={() => void state.reconnect()} disabled={state.busy} data-testid="connect-owner">{state.connected ? "Reconnect" : "Connect to engine"}</button>
      </header>
      <div aria-live="polite" aria-atomic="true" className="notice-region">
        {state.error && <p role="alert" className="error">{state.error}</p>}
        {state.notice && <p role="status" className="notice">{state.notice}</p>}
        {state.pending && <div className="notice">
          <p>An unresolved owner intent is retained in this native process. New decisions are blocked. Retry preserves its original revision and content.</p>
          {state.intent && <p>Original intent: {state.intent.kind} · {state.intent.project_id ?? state.intent.baseline_id}</p>}
          {state.intent?.model_access && <pre data-testid="pending-model-access">{JSON.stringify(state.intent.model_access, null, 2)}</pre>}
          {state.intent?.project_spec && <pre data-testid="pending-project-declaration">{displayText(JSON.stringify(state.intent.project_spec, null, 2))}</pre>}
          <div className="button-row">
            <button disabled={state.busy || !state.connected} onClick={() => void state.reconcile()} data-testid="reconcile-owner">Reconcile intent</button>
            <button disabled={state.busy || !state.connected} onClick={() => void state.retry()} data-testid="retry-owner">Retry exact intent</button>
          </div>
        </div>}
        {state.busy && <p role="status" className="muted">Owner request in progress…</p>}
      </div>
      {state.view === "settings" ? <ModelAccess state={state} /> : state.view === "projects" ? <Projects state={state} /> : state.view === "conversation" ? <Conversation state={state} /> : <section className="baseline-browser" aria-busy={state.busy}>
        <BaselineList page={state.page} selected={state.detail?.id} busy={state.busy} connected={state.connected} refresh={state.refresh} select={state.select} />
        {state.detail ? <BaselineInspection detail={state.detail} blocked={state.busy || state.pending || !state.connected} approve={state.approve} revoke={state.revoke} /> : <div className="inspection empty"><h2>Inspect a baseline</h2><p>Select an engine baseline to read its requirements and checks.</p></div>}
      </section>}
    </main>
  </div>;
}
