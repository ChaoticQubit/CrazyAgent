import type { NativeAuthCommand, NativeAuthState, NativeAuthView } from "../native-auth";
import {
  nativeAuthActive, nativeAuthAllowed, nativeAuthError, nativeAuthText, safeChallenge,
} from "../native-auth";

export { reduceNativeAuth } from "../native-auth";
export type PanelAction = NativeAuthCommand | "confirm_logout" | "back" | "refresh";

function Field({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd className="mono">{nativeAuthText(value)}</dd></div>;
}

function Challenge({ value }: { value: NonNullable<NativeAuthView["challenge"]> }) {
  if (!safeChallenge(value)) return <p>{nativeAuthError("native_auth_cannot_display_safely")}</p>;
  if (value.kind === "codex_browser") return <dl className="identity-grid">
    <Field label="Browser sign-in URL" value={value.auth_url} />
  </dl>;
  return <dl className="identity-grid">
    <Field label="Device code" value={value.user_code} />
    <Field label="Verification URL" value={value.verification_url} />
  </dl>;
}

function Account({ value }: { value: NonNullable<NativeAuthView["account"]> }) {
  if (value.kind === "codex_api_key") return <p>Codex API key observed.</p>;
  if (value.kind === "codex_chatgpt") return <>
    <h3>Codex ChatGPT observation</h3><dl className="identity-grid">
      <Field label="Email" value={value.email ?? "null"} />
      <Field label="Plan type" value={value.plan_type} />
    </dl>
  </>;
  if (value.kind === "codex_amazon_bedrock") return <>
    <h3>Codex Amazon Bedrock observation</h3>
    <p>Uses Codex-managed credentials: {String(value.uses_codex_managed_credentials)}</p>
  </>;
  const observed = [value.email, value.org_id, value.org_name, value.subscription_type];
  const labels = ["Email", "Organization ID", "Organization name", "Subscription type"];
  return <>
    <h3>Claude observation</h3><dl className="identity-grid">
      <Field label="Logged in" value={String(value.logged_in)} />
      <Field label="Authentication method" value={value.auth_method} />
      <Field label="API provider" value={value.api_provider} />
      <Field label="Configuration directory" value={value.config_directory} />
      {observed.map((field, index) => <Field key={labels[index]} label={labels[index]!}
        value={field.state === "value" ? field.text : field.state} />)}
    </dl>
  </>;
}

function Observation({ state }: { state: NativeAuthState }) {
  const value = state.presentation;
  const operation = state.operation;
  return <>
    {operation && <dl className="identity-grid">
      <Field label="Original operation" value={operation.operation_id} />
      <Field label="Original connection" value={operation.connection} />
      <Field label="Owner phase" value={operation.phase} />
      <Field label="Owner generation" value={operation.generation} />
      <Field label="Owner database" value={operation.database_instance} />
      <Field label="Engine epoch" value={operation.engine_epoch} />
    </dl>}
    {operation && (operation.connection !== state.context.connection ||
      operation.config_id !== state.context.config_id ||
      operation.source_hash !== state.context.source_hash) &&
      <p>Read or Cancel requires the original connection and settings source.
        Return to that connection in Settings.</p>}
    {operation?.code && <p>{nativeAuthText(operation.code)}</p>}
    {state.error && <p role="alert">{nativeAuthError(state.error)} Reason: {state.error}</p>}
    {value?.challenge && <Challenge value={value.challenge} />}
    {value?.account && <Account value={value.account} />}
    {state.in_flight && <p role="status">Owner authentication request in progress.</p>}
  </>;
}

function Actions({ state, onAction }: {
  state: NativeAuthState; onAction: (action: PanelAction) => void;
}) {
  if (state.confirming_logout) return <div className="button-row">
    <button type="button" autoFocus onClick={() => onAction("back")}>Back</button>
    <button type="button" disabled={!nativeAuthAllowed(state, "logout")}
      onClick={() => onAction("logout")}>Confirm logout</button>
  </div>;
  const starts: [NativeAuthCommand, string][] = state.context.harness === "codex" ?
    [["codex_browser", "Start browser sign-in"],
      ["codex_device_code", "Start device-code sign-in"]] :
    state.context.harness === "claude" ? [["claude_official", "Start official sign-in"]] : [];
  return <div className="button-row">
    {starts.map(([action, label]) => <button key={action} type="button"
      disabled={!nativeAuthAllowed(state, action)}
      onClick={() => onAction(action)}>{label}</button>)}
    <button type="button" disabled={!nativeAuthAllowed(state, "read")}
      data-testid="native-auth-read" onClick={() => onAction("read")}>
      Read original operation</button>
    <button type="button" disabled={!nativeAuthAllowed(state, "cancel")}
      data-testid="native-auth-cancel" onClick={() => onAction("cancel")}>
      Cancel original operation</button>
    <button type="button" disabled={!state.valid || nativeAuthActive(state)}
      onClick={() => onAction("confirm_logout")}>Logout…</button>
  </div>;
}

export function NativeAuthPanel({ state, onAction, onClose, blocked = false }: {
  state: NativeAuthState | undefined; onAction: (action: PanelAction) => void; onClose: () => void;
  blocked?: boolean;
}) {
  if (!state?.open) return null;
  return <section className="metadata-form native-auth-panel" role="dialog"
    aria-label="Native authentication" aria-busy={Boolean(state.in_flight)}
    data-testid="native-auth-panel" onKeyDown={(event) => {
      if (event.key === "Escape") { event.stopPropagation(); onClose(); }
    }}>
    <h2>Native authentication</h2>
    <p>Account observation does not establish native delivery or execution.</p>
    <p>Closing hides this panel. Cancel sends an owner request; it does not claim completion.</p>
    <dl className="identity-grid">
      <Field label="Selected connection" value={state.context.connection} />
      <Field label="Configuration" value={state.context.config_id} />
      <Field label="Settings source" value={state.context.source_hash} />
    </dl>
    <Observation state={state} />
    <Actions state={blocked ? { ...state, valid: false } : state} onAction={onAction} />
    <div className="button-row">
      <button type="button" disabled={blocked || Boolean(state.in_flight)}
        onClick={() => onAction("refresh")}>Refresh selected connection in Settings</button>
      <button type="button" onClick={onClose}>Close</button>
    </div>
  </section>;
}
