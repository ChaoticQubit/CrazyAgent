import type { useOwnerBrowser } from "../state";
import type { ModelAccessForm } from "../model-access";
import { recordId } from "../model-access";

type State = ReturnType<typeof useOwnerBrowser>;

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
      {text("members", "Connection references, comma separated", 8318)}{text("projects", "Project references, comma separated", 8318)}{text("roles", "Role references, comma separated", 8318)}
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
    <p className="notice" data-testid="model-auth-unavailable">Actual sign-in and authentication are unavailable. Stored references are metadata, not qualified accounts. This editor makes no provider calls.</p>
    <nav className="metadata-tabs" aria-label="Model metadata collections">
      {(["accounts", "connections", "pools"] as const).map((collection) => <button key={collection} aria-current={state.modelCollection === collection ? "page" : undefined} onClick={() => void state.showSettings(collection)} data-testid={`model-tab-${collection}`}>{collection[0].toUpperCase() + collection.slice(1)}</button>)}
    </nav>
    <p className="muted" data-testid="model-access-authority">{basis?.fresh && state.connected ? basis.writable ? "Writable: host-granted metadata sources" : "Read-only: no writable metadata grant" : "Stale metadata: reconnect or refresh"}</p>
    {basis && <dl className="identity-grid" data-testid="model-access-basis"><div><dt>Configuration</dt><dd className="mono">{basis.config_id}</dd></div><div><dt>Owner revision</dt><dd>{basis.revision}</dd></div><div><dt>Settings source hash</dt><dd className="mono">{basis.source_hash}</dd></div></dl>}
    <div className="metadata-browser">
      <div className="metadata-records">
        <button disabled={blocked || !state.modelPage?.fresh || !state.modelPage.writable || Boolean(state.modelForm)} onClick={() => void state.createModel()} data-testid="create-model-access">{state.modelCollection === "accounts" ? "Add account reference" : state.modelCollection === "connections" ? "Create connection" : "Create pool"}</button>
        {state.modelPage?.items.length === 0 && <p className="empty">No {state.modelCollection} on this owner page.</p>}
        {state.modelPage?.items.map((record) => <button className={`baseline-row ${state.modelDetail && recordId(state.modelDetail.record) === recordId(record) ? "active" : ""}`} key={recordId(record)} disabled={state.busy || !state.connected} onClick={() => void state.selectModel(recordId(record))} data-model-id={recordId(record)}>{recordId(record)}</button>)}
        {state.modelPage?.has_more && <button disabled={state.busy || !state.connected} onClick={() => void state.refreshModels(true)} data-testid="next-model-access">Next metadata page</button>}
      </div>
      <div className="metadata-inspection">
        {state.modelDetail && <section>
          <h3>Inspected metadata: {recordId(state.modelDetail.record)}</h3>
          <pre data-testid="model-access-record">{JSON.stringify(state.modelDetail.record, null, 2)}</pre>
          <div className="button-row">
            {state.modelDetail.record.kind !== "account" && <button disabled={blocked || !state.modelDetail.fresh || !state.modelDetail.writable || Boolean(state.modelForm)} onClick={() => state.editModel()} data-testid="replace-model-access">Replace metadata</button>}
            <button disabled={blocked || !state.modelDetail.fresh || !state.modelDetail.writable || Boolean(state.modelForm)} onClick={() => state.editModel(true)} data-testid="remove-model-access">Remove metadata…</button>
          </div>
        </section>}
        <MetadataForm state={state} />
        {!state.modelDetail && !state.modelForm && <p className="empty">Select a record to inspect its complete metadata or create a new reference.</p>}
      </div>
    </div>
  </section>;
}
