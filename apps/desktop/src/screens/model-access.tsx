import type { useOwnerBrowser } from "../state";
import type { ModelAccessForm } from "../model-access";
import type { ModelAccessRecord, ModelConnection } from "../engine-client";
import { recordId } from "../model-access";
import { NativeAuthPanel } from "./native-auth";
import { displayText } from "./conversation";
import { AccountOnboarding } from "./account-onboarding";

type State = ReturnType<typeof useOwnerBrowser>;

export function PoolConnectionPicker({ form, connections, current, hasMore, blocked, onLoad, onChange }: {
  form: ModelAccessForm;
  connections: ModelConnection[];
  current: boolean;
  hasMore: boolean;
  blocked: boolean;
  onLoad: (next?: boolean) => void;
  onChange: (id: string, selected: boolean) => void;
}) {
  const selected = form.members.split(",").map((value) => value.trim()).filter(Boolean);
  return <fieldset data-testid="pool-connection-picker">
    <legend>Choose saved connections</legend>
    <button type="button" disabled={blocked} onClick={() => onLoad(false)} data-testid="load-pool-connections">Load current saved connections</button>
    <p>{selected.length} selected. Selections on other pages are retained.</p>
    {!current && <p className="muted">Load current saved connections before changing these choices. Refresh and review the pool before saving.</p>}
    {connections.map((connection) => <label className="checkbox-label" key={connection.id}>
      <input type="checkbox" data-pool-connection={connection.id} checked={selected.includes(connection.id)} disabled={blocked || !current} onChange={(event) => onChange(connection.id, event.target.checked)} />
      <span>{displayText(connection.account_ref)} · {displayText(connection.model.id)} · {displayText(connection.id)}</span>
    </label>)}
    {hasMore && <button type="button" disabled={blocked || !current} onClick={() => onLoad(true)} data-testid="next-pool-connections">Next saved connections page</button>}
    <p className="muted">Saving a pool does not sign in its accounts or establish model readiness.</p>
  </fieldset>;
}

function RecordSummary({ record, state }: { record: ModelAccessRecord; state: State }) {
  if (record.kind === "account") return <span className="muted">Configured account reference</span>;
  if (record.kind === "pool") return <>
    <span className="muted">Connections: {record.pool.members.join(", ")}</span>
    <span className="muted">{record.pool.projects.length && record.pool.roles.length ? "Assigned to explicit projects and roles" : "Unassigned"}</span>
    <span className="muted">Projects: {record.pool.projects.join(", ")} · Roles: {record.pool.roles.join(", ")}</span>
    <span className="muted">Quota policy: {record.pool.provider_quota_policy ?? "bounded_attempt"}</span>
  </>;
  const connection = record.connection;
  const operation = state.nativeAuth?.operation;
  const sameSource = operation?.connection === connection.id &&
    operation.config_id === state.modelPage?.config_id &&
    operation.source_hash === state.modelPage?.source_hash;
  const current = sameSource && state.connected && state.modelPage?.fresh &&
    state.nativeAuth?.valid && !state.nativeAuth.invalidated;
  return <>
    <span className="muted">Account: {displayText(connection.account_ref)}</span>
    <span className="muted">{displayText(connection.harness)} · {displayText(connection.model.id)}</span>
    <span className="muted">{sameSource ?
      `${current ? "Owner observation" : "Previous owner observation"}: ${operation.phase}` :
      "Configured connection; authentication has not been observed here."}</span>
  </>;
}

function MetadataForm({ state }: { state: State }) {
  const retained = state.modelForm;
  if (!retained) return null;
  const form = retained.value;
  const basis = form.action === "create" ? state.modelPage : state.modelDetail;
  const current = Boolean(state.connected && basis?.fresh && basis.writable && basis.collection === form.collection && basis.ticket === form.ticket);
  const removing = form.action === "remove";
  function text(field: keyof ModelAccessForm, label: string, maxLength = 128) {
    return <label>{label}<input value={String(form[field])} maxLength={maxLength} spellCheck={false} autoComplete="off" disabled={removing || (field === "id" && form.action !== "create")} onChange={(event) => state.changeModelForm({ [field]: event.target.value })} data-testid={`model-field-${field}`} /></label>;
  }
  return <form className="metadata-form" onSubmit={(event) => { event.preventDefault(); void state.submitModel(); }} data-testid="model-access-form">
    <h3>{removing ? "Confirm removal" : form.action === "create" ? "Create metadata" : "Replace metadata"}: {form.collection}</h3>
    <p className="muted">References and metadata only. Never enter a token, password or API key. Saving does not authenticate an account.</p>
    {text("id", form.collection === "accounts" ? "Account reference" : "Identity")}
    {!removing && form.collection === "connections" && <>
      {text("provider", "Provider reference")}{text("harness", "Harness reference")}{text("protocol", "Protocol reference")}{text("account", "Account reference")}
      <label>Endpoint type<select value={form.endpoint} onChange={(event) => state.changeModelForm({ endpoint: event.target.value as ModelAccessForm["endpoint"] })} data-testid="model-field-endpoint"><option value="http">HTTP</option><option value="native">Native</option></select></label>
      {form.endpoint === "http" && text("url", "Endpoint URL without credentials, query or fragment", 2048)}
      {text("model", "Model ID", 256)}{text("author", "Model author reference")}
      <label>Authentication metadata<select value={form.auth} onChange={(event) => state.changeModelForm({ auth: event.target.value as ModelAccessForm["auth"] })} data-testid="model-field-auth"><option value="none">None</option><option value="api_key">Stored API-key reference</option><option value="oauth">Stored OAuth reference</option><option value="native_managed">Native-managed profile reference</option></select></label>
      {form.auth !== "none" && text("credential", form.auth === "native_managed" ? "Existing profile reference" : "Existing credential reference")}
    </>}
    {!removing && form.collection === "pools" && <>
      <PoolConnectionPicker form={form}
        connections={state.poolConnections?.items.flatMap((record) => record.kind === "connection" ? [record.connection] : []) ?? []}
        current={Boolean(state.connected && state.poolConnections?.fresh)}
        hasMore={Boolean(state.poolConnections?.has_more)}
        blocked={state.busy || state.pending || !state.connected}
        onLoad={(next) => void state.loadPoolConnections(next)} onChange={state.changePoolConnection} />
      <fieldset data-testid="pool-project-assignment">
        <legend>Use this pool for a project master</legend>
        <p>{form.projects.trim() && form.roles.trim() ? "Assigned to explicit projects and roles." : "Unassigned. This pool cannot be used until its project and role are explicitly assigned."}</p>
        <p>Create or open a project, then choose it here. Confirming saves this pool for project_leader and preserves its other assignments and settings.</p>
        <button type="button" disabled={state.busy || state.pending || !state.connected || form.action !== "replace"}
          onClick={() => void state.loadPoolProjects()} data-testid="load-pool-projects">Load saved projects</button>
        <label>Project<select value={state.poolProjectId} disabled={state.busy || state.pending || !state.connected || !state.projectPage?.fresh || form.action !== "replace"}
          onChange={(event) => state.changePoolProject(event.target.value)} data-testid="pool-assignment-project">
          <option value="">Choose a project</option>
          {state.projectPage?.items.map((project) => <option key={project.id} value={project.id}>{displayText(project.name)} · {project.id}</option>)}
        </select></label>
        {state.projectPage?.has_more && <button type="button" disabled={state.busy || state.pending || !state.connected || !state.projectPage.fresh}
          onClick={() => void state.loadPoolProjects(true)} data-testid="next-pool-projects">Next projects page</button>}
        <button type="button" disabled={state.busy || state.pending || !state.connected || Boolean(state.poolAssignment) || form.action !== "replace" || !state.projectPage?.fresh || !state.poolProjectId}
          onClick={() => void state.confirmPoolProject()} data-testid="confirm-pool-project">Confirm assignment and save pool</button>
        <p className="muted">Save a new pool as Unassigned first. Confirmation requires an actual project read and exact saved-pool readback. It does not start a master or establish authentication or readiness.</p>
      </fieldset>
      {text("members", "Connection references, comma separated", 8318)}{text("projects", "Project references, comma separated", 8318)}{text("roles", "Role references, comma separated", 8318)}
      <p className="muted">Blank project or role assignments keep the pool Unassigned. A pool needs at least one connection.</p>
      <label>Provider quota policy<select value={form.providerQuotaPolicy} aria-describedby="model-quota-policy-help" onChange={(event) => state.changeModelForm({ providerQuotaPolicy: event.target.value as ModelAccessForm["providerQuotaPolicy"] })} data-testid="model-field-providerQuotaPolicy"><option value="bounded_attempt">Bounded attempt (default)</option><option value="strict">Strict quota</option></select></label>
      <p id="model-quota-policy-help" className="muted">A bounded attempt allows an initial selection when numeric provider quota is unknown. Strict quota blocks that selection; numeric quota is currently unavailable. Both modes require genuine fresh readiness and independent engine admission for execution.</p>
      <label className="checkbox-label"><input type="checkbox" checked={form.fallback} onChange={(event) => state.changeModelForm({ fallback: event.target.checked })} data-testid="model-field-fallback" />Allow configured fallback</label>
    </>}
    {removing && <p>Remove this inspected identity from the configuration? The engine checks dependencies before publication.</p>}
    {!current && <p role="status" className="notice">This retained form needs a current owner snapshot. Refresh and review its values before saving.</p>}
    <div className="button-row">
      <button type="submit" disabled={!current || state.busy || state.pending} data-testid="save-model-access">{removing ? "Confirm removal" : "Save metadata"}</button>
      <button type="button" disabled={state.busy || state.pending || !state.connected} onClick={() => void state.reviewModelForm()} data-testid="review-model-access">Refresh and review retained form</button>
      <button type="button" onClick={state.discardModelForm} data-testid="discard-model-access">Discard form</button>
    </div>
  </form>;
}

export function ModelAccess({ state }: { state: State }) {
  const basis = state.modelDetail ?? state.modelPage;
  const blocked = state.busy || state.pending || !state.connected;
  return <section className="metadata-settings" aria-busy={state.busy}>
    <div className="section-heading"><h2>Model access</h2><button disabled={state.busy || !state.connected} onClick={() => void state.refreshModels()} data-testid="refresh-model-access">Refresh metadata</button></div>
    <p>Configure account references, model connections and account pools through the engine.</p>
    {state.poolAssignment && <section role="status" className="notice" data-testid="pool-assignment-recovery">
      <p>Pool assignment for {state.poolAssignment.projectId} is retained. Reconcile any pending intent, then verify the exact saved record before use.</p>
      <button disabled={blocked} onClick={() => void state.verifyPoolAssignment()} data-testid="verify-pool-assignment">Verify saved pool assignment</button>
    </section>}
    <AccountOnboarding state={state} />
    <section aria-label="Account setup" className="notice">
      <h3>Sign in with a configured connection</h3>
      <p>Add an account reference, create its harness/model connection with a native-managed
        profile, then inspect the connection and open Native authentication. Complete the
        official sign-in shown there. Never enter credentials in these metadata fields.</p>
      <p>Account pools list the connections available to selected projects and roles.
        Choose the project leader role for a master's pool. A configured pool does not
        establish authentication, numeric quota or execution readiness.</p>
      <div className="button-row">
        <button disabled={state.busy || !state.connected}
          onClick={() => void state.showSettings("accounts")}>Account references</button>
        <button disabled={state.busy || !state.connected}
          onClick={() => void state.showSettings("connections")}>Connections and sign-in</button>
        <button disabled={state.busy || !state.connected}
          onClick={() => void state.showSettings("pools")}>Account pools</button>
      </div>
    </section>
    <p className="notice" data-testid="model-auth-unavailable">Stored references are metadata.
      Native authentication uses explicit owner actions on an inspected connection.
      Account observation does not qualify native delivery or execution.</p>
    <nav className="metadata-tabs" aria-label="Model metadata collections">
      {(["accounts", "connections", "pools"] as const).map((collection) => <button key={collection} aria-current={state.modelCollection === collection ? "page" : undefined} onClick={() => void state.showSettings(collection)} data-testid={`model-tab-${collection}`}>{collection[0].toUpperCase() + collection.slice(1)}</button>)}
    </nav>
    <p className="muted" data-testid="model-access-authority">{basis?.fresh && state.connected ? basis.writable ? "Writable: host-granted metadata sources" : "Read-only: no writable metadata grant" : "Stale metadata: reconnect or refresh"}</p>
    {basis && <dl className="identity-grid" data-testid="model-access-basis"><div><dt>Configuration</dt><dd className="mono">{basis.config_id}</dd></div><div><dt>Owner revision</dt><dd>{basis.revision}</dd></div><div><dt>Settings source hash</dt><dd className="mono">{basis.source_hash}</dd></div></dl>}
    <div className="metadata-browser">
      <div className="metadata-records">
        <button disabled={blocked || !state.modelPage?.fresh || !state.modelPage.writable || Boolean(state.modelForm)} onClick={() => void state.createModel()} data-testid="create-model-access">{state.modelCollection === "accounts" ? "Add account reference" : state.modelCollection === "connections" ? "Create connection" : "Create pool"}</button>
        {state.modelPage?.items.length === 0 && <p className="empty">No {state.modelCollection} on this owner page.</p>}
        {state.modelPage?.items.map((record) => <button className={`baseline-row ${state.modelDetail && recordId(state.modelDetail.record) === recordId(record) ? "active" : ""}`} key={recordId(record)} disabled={state.busy || !state.connected} onClick={() => void state.selectModel(recordId(record))} data-model-id={recordId(record)}><span>{displayText(recordId(record))}</span><RecordSummary record={record} state={state} /></button>)}
        {state.modelPage?.has_more && <button disabled={state.busy || !state.connected} onClick={() => void state.refreshModels(true)} data-testid="next-model-access">Next metadata page</button>}
      </div>
      <div className="metadata-inspection">
        {state.modelDetail && <section>
          <h3>Inspected metadata: {recordId(state.modelDetail.record)}</h3>
          <pre data-testid="model-access-record">{JSON.stringify(state.modelDetail.record, null, 2)}</pre>
          <div className="button-row">
            {state.modelDetail.record.kind === "connection" && <button
              disabled={blocked || !state.modelDetail.fresh || Boolean(state.modelForm)}
              onClick={() => void state.openNativeAuth()}
              data-testid="native-auth-open">Native authentication…</button>}
            {state.modelDetail.record.kind !== "account" && <button disabled={blocked || !state.modelDetail.fresh || !state.modelDetail.writable || Boolean(state.modelForm)} onClick={() => state.editModel()} data-testid="replace-model-access">Replace metadata</button>}
            <button disabled={blocked || !state.modelDetail.fresh || !state.modelDetail.writable || Boolean(state.modelForm)} onClick={() => state.editModel(true)} data-testid="remove-model-access">Remove metadata…</button>
          </div>
        </section>}
        <MetadataForm state={state} />
        <NativeAuthPanel state={state.nativeAuth} blocked={blocked || Boolean(state.modelForm)}
          onAction={(action) => void state.nativeAuthAction(action)}
          onClose={state.closeNativeAuth} onClaudeCode={state.submitNativeClaudeCode} />
        {!state.modelDetail && !state.modelForm && <p className="empty">Select a record to inspect its complete metadata or create a new reference.</p>}
      </div>
    </div>
  </section>;
}
