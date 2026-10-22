import { useEffect, useRef } from "react";
import type { useOwnerBrowser } from "../state";

type State = ReturnType<typeof useOwnerBrowser>;

export function ConfiguredAccountFields({ provider, url, model, disabled, onChange }: {
  provider: "api_key" | "local"; url: string; model: string; disabled: boolean;
  onChange?: (patch: { url?: string; model?: string }) => void;
}) {
  return <>
    <label>Endpoint URL<input type="url" value={url} maxLength={2048} disabled={disabled} required autoComplete="off" spellCheck={false} onChange={(event) => onChange?.({ url: event.target.value })} data-testid="account-onboarding-url" /></label>
    <label>Model<input value={model} maxLength={256} disabled={disabled} required autoComplete="off" spellCheck={false} onChange={(event) => onChange?.({ model: event.target.value })} data-testid="account-onboarding-model" /></label>
    <p>{provider === "local" ? "No-key setup accepts HTTP loopback endpoints only. Saving a profile does not contact the model or establish readiness." : "Save the endpoint configuration first. Private key entry is separate and enabled only when the owner reports a qualified private writer."}</p>
  </>;
}

export function AccountSecretForm({ disabled, onSubmit }: {
  disabled: boolean; onSubmit?: (secret: string) => Promise<void> | void;
}) {
  const secret = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const input = secret.current;
    return () => { if (input) input.value = ""; };
  }, []);
  return <form data-testid="account-profile-secret-form" onSubmit={(event) => {
    event.preventDefault();
    const input = secret.current;
    if (!input) return;
    const value = input.value;
    input.value = "";
    if (!disabled && value) void onSubmit?.(value);
  }}>
    <label>Private API key<input ref={secret} type="password" maxLength={8192} autoComplete="off" spellCheck={false} required disabled={disabled} data-testid="account-profile-secret" /></label>
    <button type="submit" disabled={disabled} data-testid="account-profile-save-key">Save private key</button>
    <p>The key is cleared from this field before submission and is never automatically resent. An uncertain result requires reading the original operation before any new submission.</p>
  </form>;
}

export function AccountOnboarding({ state }: { state: State }) {
  const draft = state.accountDraft;
  const managed = draft.provider === "codex" || draft.provider === "claude";
  const setup = state.managedSetup;
  const profile = state.accountProfile;
  const blocked = state.busy || state.pending || !state.connected || Boolean(state.modelForm);
  const profileBlocked = blocked || !state.modelDetail?.fresh || state.modelDetail.ticket !== profile?.detail.ticket;
  return <section aria-label="Add an account">
    <form className="metadata-form" data-testid="account-onboarding-form" onSubmit={(event) => { event.preventDefault(); void state.submitManagedAccount(); }}>
      <h3>Sign in to a provider</h3>
      <label>Account type<select value={draft.provider} disabled={Boolean(setup) || state.busy} onChange={(event) => state.changeAccountDraft({ provider: event.target.value })} data-testid="account-onboarding-provider">
        <option value="codex">Codex</option>
        <option value="claude">Claude</option>
        <option value="api_key">API key or compatible endpoint</option>
        <option value="local">Local endpoint</option>
      </select></label>
      <label>Account label (optional)<input value={draft.label} maxLength={96} disabled={Boolean(setup) || state.busy} onChange={(event) => state.changeAccountDraft({ label: event.target.value })} autoComplete="off" data-testid="account-onboarding-label" /></label>
      {managed ? <p>Save a separate account, connection and pool, then use the official sign-in panel. Configuration alone does not sign you in or establish model readiness.</p> : <ConfiguredAccountFields provider={draft.provider === "local" ? "local" : "api_key"} url={draft.url} model={draft.model} disabled={Boolean(setup) || blocked} onChange={state.changeAccountDraft} />}
      {!setup && <button type="submit" disabled={blocked} data-testid="account-onboarding-submit">{managed ? "Continue to official sign-in" : "Save endpoint configuration"}</button>}
      {!state.connected && <p className="muted">Connect to the engine to save this account. You can choose a provider and retain a label now.</p>}
    </form>
    {setup && <section aria-label="Retained account setup" className="notice" data-testid="account-onboarding-progress">
      <h3>{setup.plan.label || ({ codex: "Codex", claude: "Claude", api_key: "API key", local: "Local endpoint" }[setup.plan.provider])} account setup</h3>
      <p>{setup.next} of 3 configuration records verified by the owner.</p>
      <p role="status">{setup.message}</p>
      <dl className="identity-grid"><div><dt>Account</dt><dd>{setup.plan.connection.account_ref}</dd></div><div><dt>Pool</dt><dd>{setup.plan.pool.id}</dd></div></dl>
      <div className="button-row">
        {setup.next < 3 && <button type="button" disabled={blocked || setup.waiting !== undefined} onClick={() => void state.continueManagedAccount()} data-testid="account-onboarding-continue">Review current owner state and continue</button>}
        {setup.next === 3 && <button type="button" disabled={blocked} onClick={() => void state.inspectManagedAccount()} data-testid="account-onboarding-sign-in">{setup.plan.provider === "codex" || setup.plan.provider === "claude" ? "Open official sign-in panel" : "Read private setup capability"}</button>}
        <button type="button" disabled={state.busy || state.pending || setup.waiting !== undefined} onClick={state.closeManagedAccount} data-testid="account-onboarding-close">{setup.next === 0 ? "Discard setup" : "Keep saved records and close setup"}</button>
      </div>
      <p className="muted">An uncertain save retains its original request. Use Reconcile or Retry for that request before continuing. Closing setup never removes saved records.</p>
    </section>}
    {profile && <section aria-label="Private account profile" className="notice" data-testid="account-profile-panel">
      <h3>Private profile for {setup?.plan.label || (profile.detail.record.kind === "connection" ? profile.detail.record.connection.model.id : "saved connection")}</h3>
      <p role="status">{profile.message}</p>
      {profile.capabilities.configured !== "available" && <p data-testid="account-profile-unavailable">Private configuration unavailable: {profile.capabilities.configured.unavailable.code}. No key is requested.</p>}
      {!profile.result && !profile.uncertain && profile.capabilities.configured === "available" && profile.detail.record.kind === "connection" && (profile.detail.record.connection.auth.method === "api_key" ?
        <AccountSecretForm key={profile.detail.ticket} disabled={profileBlocked || profile.submitting} onSubmit={state.submitAccountSecret} /> :
        <button type="button" disabled={profileBlocked || profile.submitting} onClick={() => void state.configureLocalAccount()} data-testid="account-profile-save-local">Save no-key local profile</button>)}
      {profile.uncertain && <button type="button" disabled={state.busy || !state.connected} onClick={() => void state.reconcileAccountProfile()} data-testid="account-profile-reconcile">Read original private operation</button>}
      <p>Configured only is separate from sign-in, authentication, readiness and independent engine admission.</p>
    </section>}
  </section>;
}
