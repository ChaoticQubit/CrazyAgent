import { useEffect, useRef, useState, type RefObject } from "react";
import { listAccounts, pickFolder, type AccountView, type Result } from "../../api";
import {
  addAppPlugin, appPluginRefusal, appPluginUsers, failureTone, bindAppPluginAccount, changedLine, ESSENTIAL_NOTE, grantSummary, keyAccounts, keyHosts, loadAppPlugins,
  loadPluginGrants, permissionWords, pluginState, previewAppPlugin, problemText, PROJECT_OVERRIDE_LINE, removeAppPlugin, REMOVE_KEY_LINE, reviewAppPlugin,
  reviewHosts, setAppPluginEnabled, setAppPluginKey, setAppPluginWhenOff, trustAppPlugin, TURN_OFF_WORDS, unbindAppPlugin,
  type AppPluginsPage, type PermissionWords, type PluginGrants, type PluginUser, type PluginView, type Preview, type Problem, type RunsView, type SecretView,
  type TrustView, type TurnOff,
} from "../../api-app-plugins";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, LoadError, NotReady, Notice, RadioCard, SecretField, SettingSection, Skeleton, StatusBadge, Switch } from "../../ui";
import "../project/projects.css";

const NOT_READY = "This isn't ready in this build yet.";
const KEY_SOURCE = "app-plugin-key-source";
const WHEN_OFF = "app-plugin-when-off";
const ONCE = "app-plugin-when-off-once";
const TURN_OFF_CHOICES: TurnOff[] = ["refuse_new_calls", "stop_runs"];

/** What App plugins has open over the list: nothing, a trust screen (for a picked folder or
 * a review of a plugin in the plugins folder), a refused plugin, the key dialog for one
 * secret, the remove confirm, or the turn-off confirm with the runs using the plugin. */
export type PluginDialog =
  | { kind: "none" }
  | { kind: "trust"; folder: string | null; trust: TrustView; review: boolean }
  | { kind: "refused"; problem: Problem }
  | { kind: "bind"; plugin: string; title: string; secret: string; hosts: string[]; choice: string }
  | { kind: "remove"; plugin: string; title: string; keyed: boolean }
  | { kind: "off"; plugin: string; title: string; users: PluginUser[]; choice: TurnOff };

/** Everything the App plugins view asks its container to do. */
export interface AppPluginActions {
  onAdd: () => void;
  onConfirm: () => void;
  onClose: () => void;
  onEnabled: (name: string, title: string, enabled: boolean) => void;
  onConfirmOff: () => void;
  onOffChoice: (choice: TurnOff) => void;
  onWhenOff: (choice: TurnOff) => void;
  onReview: (name: string) => void;
  onRemove: (plugin: PluginView) => void;
  onConfirmRemove: () => void;
  onBind: (plugin: PluginView, secret: string) => void;
  onKeyChoice: (choice: string) => void;
  onSaveKey: () => void;
  onUnbind: (name: string, secret: string) => void;
  onRetry: () => void;
}

/** How a plugin runs, in one line the row and the trust screen share. Input: how it runs.
 * Output: the line, naming the download or every host it reaches. */
export function runsLine(runs: RunsView): string {
  if (runs.kind === "local") return `Runs on this computer with the built-in model, a ${Math.round(runs.download_bytes / 1_000_000)} MB download`;
  return `Reaches ${runs.hosts.join(", ")}${runs.local_unencrypted ? " (local, not encrypted)" : ""}`;
}

/** Where one secret's key comes from, never the key. Input: the secret. Output: the line. */
export function secretLine(secret: SecretView): string {
  const binding = secret.binding;
  if (binding.kind === "account") return `${secret.name}: Key from ${binding.name}`;
  if (binding.kind === "key") return `${secret.name}: Key you entered`;
  return `${secret.name}: Not set`;
}

const TOP = <div className="settings-top"><div>
  <h2>App plugins</h2>
  <p>Plugins CrazyAgent itself runs. Each one shows what it reaches and what it may do before you trust it.</p>
</div></div>;

function PluginRow({ plugin, busy, actions }: { plugin: PluginView; busy: boolean; actions: AppPluginActions }) {
  const state = pluginState(plugin);
  const blocked = plugin.status.kind === "refused" ? "Refused plugins can't run." : state.review ? "Review it first." : undefined;
  const origin = plugin.origin === "bundled" ? "Built in" : "Added by you";
  return <li className="pj-task" aria-label={plugin.title}>
    <div className="pj-row between">
      <div className="pj-stack-s">
        <strong>{plugin.title}</strong>
        <span className="muted small">{`${plugin.name}, version ${plugin.version}. ${origin}. ${grantSummary(null, plugin)}.`}</span>
      </div>
      <StatusBadge word={state.word} tone={state.tone} />
    </div>
    {plugin.description && <p className="secondary">{plugin.description}</p>}
    {plugin.runs && <p className="small">{runsLine(plugin.runs)}</p>}
    {state.detail && <p className="small">{state.detail}</p>}
    {plugin.secrets.length > 0 && <ul className="pj-rows" aria-label={`Secrets of ${plugin.title}`}>{plugin.secrets.map((secret) => <li key={secret.name} className="pj-row between">
      <span>{secretLine(secret)}</span>
      {secret.binding.kind === "none"
        ? <Button size="small" aria-label={`Set key for ${secret.name} of ${plugin.title}`} disabled={busy} onClick={() => actions.onBind(plugin, secret.name)}>Set key</Button>
        : <Button size="small" variant="quiet" aria-label={`Unbind ${secret.name} of ${plugin.title}`} disabled={busy} onClick={() => actions.onUnbind(plugin.name, secret.name)}>Unbind</Button>}
    </li>)}</ul>}
    <div className="pj-row">
      <Switch id={`app-plugin-${plugin.name}`} label={`Use ${plugin.title}`} checked={plugin.enabled} disabledReason={blocked}
        onChecked={(on) => { if (!busy) actions.onEnabled(plugin.name, plugin.title, on); }} />
      {state.review && <Button size="small" aria-label={`Review ${plugin.title}`} disabled={busy} onClick={() => actions.onReview(plugin.name)}>Review</Button>}
      {plugin.origin === "user" && <Button size="small" variant="danger-text" aria-label={`Remove ${plugin.title}`} disabled={busy} onClick={() => actions.onRemove(plugin)}>Remove</Button>}
    </div>
  </li>;
}

function CodePluginRow({ grants, words, busy, actions }: { grants: PluginGrants; words: readonly PermissionWords[]; busy: boolean; actions: AppPluginActions }) {
  const on = grants.state === "on";
  return <li className="pj-task" aria-label={grants.plugin}>
    <div className="pj-row between">
      <div className="pj-stack-s">
        <strong>{grants.plugin}</strong>
        <span className="muted small">{`Built in. ${grantSummary(grants, null)}.`}</span>
      </div>
      <StatusBadge word={on ? "Enabled" : "Off"} tone={on ? "ok" : "neutral"} />
    </div>
    {grants.capabilities.length > 0 && <ul className="pj-rows" aria-label={`What ${grants.plugin} may do`}>
      {grants.capabilities.map((capability) => <li key={capability.name}>{permissionWords(words, capability.name)}</li>)}
    </ul>}
    <Switch id={`app-plugin-${grants.plugin}`} label={`Use ${grants.plugin}`} checked={on} disabledReason={grants.essential ? ESSENTIAL_NOTE : undefined}
      onChecked={(value) => { if (!busy && !grants.essential) actions.onEnabled(grants.plugin, grants.plugin, value); }} />
  </li>;
}

function TrustScreen({ trust }: { trust: TrustView }) {
  const hosts = reviewHosts(trust);
  return <div className="pj-stack-s">
    {trust.changed && <Notice tone="warn">Its files changed since you trusted it. What grew is listed first.</Notice>}
    {trust.description && <p>{trust.description}</p>}
    <ul className="pj-rows" aria-label="What this plugin is and reaches">
      {hosts.map((host) => <li key={host}>{trust.grew_hosts.includes(host) ? `New host: ${host}` : `Host: ${host}`}</li>)}
      <li>{`Kind: ${trust.kind === "embedding" ? "Embedding" : trust.kind}`}</li>
      <li>{`Version ${trust.version}`}</li>
      <li>{runsLine(trust.runs)}</li>
      {trust.runs.kind === "http" && <li>{`Address: ${trust.runs.url}`}</li>}
      <li>{`Keys it needs: ${trust.secrets.length > 0 ? trust.secrets.join(", ") : "none"}`}</li>
      <li className="mono small">{`SHA-256 ${trust.sha256}`}</li>
    </ul>
    {trust.replaces && <Notice tone="warn">{`Replaces your plugin named ${trust.name}.`}</Notice>}
  </div>;
}

function PluginDialogs({ dialog, accounts, busy, failure, failureCode, keyRef, actions }: {
  dialog: PluginDialog;
  accounts: readonly AccountView[];
  busy: boolean;
  failure: string;
  failureCode: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: AppPluginActions;
}) {
  const problem = failure ? <Notice tone={failureTone(failureCode)} alert>{failure}</Notice> : null;
  if (dialog.kind === "trust") {
    return <Dialog open wide title={dialog.review ? `Review ${dialog.trust.title}` : `Trust ${dialog.trust.title}?`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onConfirm}>Approve</Button></>}>
      {problem}
      <TrustScreen trust={dialog.trust} />
    </Dialog>;
  }
  if (dialog.kind === "refused") {
    return <Dialog open title="This plugin can't be trusted" onClose={actions.onClose} footer={<Button onClick={actions.onClose}>Close</Button>}>
      <Notice tone="danger">{problemText(dialog.problem)}</Notice>
    </Dialog>;
  }
  if (dialog.kind === "bind") {
    const sent = dialog.hosts.length > 0 ? `The key is sent only to ${dialog.hosts.join(", ")}.` : "The key stays on this computer.";
    return <Dialog open title={`Key for ${dialog.secret}`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onSaveKey}>Save</Button></>}>
      <div className="pj-stack-s">
        <p className="secondary">{`Where ${dialog.title} gets its ${dialog.secret}. ${sent}`}</p>
        {problem}
        <fieldset className="field" disabled={busy}>
          <legend className="visually-hidden">{`Key for ${dialog.secret}`}</legend>
          {accounts.map((account) => <RadioCard key={account.id} name={KEY_SOURCE} value={`account:${account.id}`} checked={dialog.choice === `account:${account.id}`}
            onChecked={actions.onKeyChoice} title={`Key from ${account.name}`} details={["Uses this account's saved API key."]} />)}
          <RadioCard name={KEY_SOURCE} value="key" checked={dialog.choice === "key"} onChecked={actions.onKeyChoice} title="Enter a key for this plugin"
            details={["Kept for this plugin only."]} />
        </fieldset>
        {dialog.choice === "key" && <SecretField id="app-plugin-key" label="Key" inputRef={keyRef ?? { current: null }} helper="Sent to the engine once and never shown again." />}
      </div>
    </Dialog>;
  }
  if (dialog.kind === "remove") {
    return <ConfirmDialog open title={`Remove ${dialog.title}?`} confirmLabel="Remove" pending={busy} onConfirm={actions.onConfirmRemove} onCancel={actions.onClose}>
      {[`It is deleted from your plugins folder.`, dialog.keyed ? REMOVE_KEY_LINE : "", failure].filter((line) => line !== "").join(" ")}
    </ConfirmDialog>;
  }
  if (dialog.kind === "off") {
    return <Dialog open title={`Turn off ${dialog.title}?`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Turning off..." onClick={actions.onConfirmOff}>Turn off</Button></>}>
      <div className="pj-stack-s">
        {problem}
        {dialog.users.length === 0
          ? <p>Nothing is using it now.</p>
          : <><p>{`${dialog.users.length === 1 ? "1 run uses" : `${dialog.users.length} runs use`} it now:`}</p>
            <ul className="pj-rows" aria-label={`Runs using ${dialog.title}`}>{dialog.users.map((user) => <li key={user.run_id}>{user.label}</li>)}</ul></>}
        <fieldset className="field" disabled={busy}>
          <legend>This time</legend>
          {TURN_OFF_CHOICES.map((choice) => <RadioCard key={choice} name={ONCE} value={choice} checked={dialog.choice === choice}
            onChecked={(value) => actions.onOffChoice(value as TurnOff)} title={TURN_OFF_WORDS[choice].title} details={[TURN_OFF_WORDS[choice].detail]} />)}
        </fieldset>
      </div>
    </Dialog>;
  }
  return null;
}

/** Settings, App plugins as a pure view: the title in every state, the "When a plugin is
 * turned off" setting, the built-in code plugins with what each may do, then every plugin
 * with its one status label, where it came from, how it runs, where each secret's key comes
 * from, its switch, Review and Remove, and Add plugin; over it the open dialog. Inputs: the
 * page, the grants, the open dialog, the API-key accounts, a busy flag, the last failure
 * and its code, the key field and the actions. Output: the tab. */
export function AppPluginsView({ page, grants, dialog, accounts, busy, failure, failureCode = "", keyRef, actions }: {
  page: Loaded<AppPluginsPage>;
  grants: readonly PluginGrants[];
  dialog: PluginDialog;
  accounts: readonly AccountView[];
  busy: boolean;
  failure: string;
  failureCode?: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: AppPluginActions;
}) {
  if (page.kind === "loading") return <>{TOP}<Skeleton shape="row" count={3} /></>;
  if (page.kind === "not_ready") return <>{TOP}<NotReady what="App plugins" /></>;
  if (page.kind === "error") return <>{TOP}<LoadError message={appPluginRefusal(page)} onRetry={actions.onRetry} /></>;
  const { listing: { plugins, more }, words, when_turned_off: whenOff } = page.value;
  return <>
    {TOP}
    <div className="pj set-groups">
      {dialog.kind === "none" && failure && <Notice tone={failureTone(failureCode)} alert>{failure}</Notice>}
      <SettingSection title="When a plugin is turned off" about="What happens to work already using a plugin when you turn it off. The confirm lets you pick differently for one time.">
        <div className="set-body">
          <fieldset className="field" disabled={busy}>
            <legend className="visually-hidden">When a plugin is turned off</legend>
            {TURN_OFF_CHOICES.map((choice) => <RadioCard key={choice} name={WHEN_OFF} value={choice} checked={whenOff.value === choice}
              onChecked={(value) => actions.onWhenOff(value as TurnOff)} title={TURN_OFF_WORDS[choice].title} details={[TURN_OFF_WORDS[choice].detail]} />)}
          </fieldset>
          {whenOff.project_override && <p className="muted small">{PROJECT_OVERRIDE_LINE}</p>}
        </div>
      </SettingSection>
      {grants.length > 0 && <SettingSection title="Built-in code plugins" about="Plugins that ship with CrazyAgent and run parts of the app itself.">
        <div className="set-body">
          <ul className="pj-rows" aria-label="Built-in code plugins">{grants.map((row) => <CodePluginRow key={row.plugin} grants={row} words={words} busy={busy} actions={actions} />)}</ul>
        </div>
      </SettingSection>}
      <SettingSection title="Plugins" about="Built-in plugins ship with CrazyAgent. Yours are copied into your plugins folder when you approve them, and run only while their files match what you trusted."
        actions={<Button variant="primary" disabled={busy} onClick={actions.onAdd}>Add plugin</Button>}>
        <div className="set-body">
          <ul className="pj-rows" aria-label="App plugins">{plugins.map((plugin) => <PluginRow key={plugin.name} plugin={plugin} busy={busy} actions={actions} />)}</ul>
          {more && <p className="muted small">{`Showing the first ${plugins.length} plugins.`}</p>}
        </div>
      </SettingSection>
    </div>
    <PluginDialogs dialog={dialog} accounts={accounts} busy={busy} failure={failure} failureCode={failureCode} keyRef={keyRef} actions={actions} />
  </>;
}

/** Settings, App plugins: reads the page and the built-in grants, adds a picked folder only
 * after its trust screen is approved, reviews a changed plugin, turns plugins on and off
 * after a confirm naming what uses them, saves the turn-off setting, removes the owner's
 * own, and binds each secret to an API-key account or a key sent once. */
export function AppPluginsSettings() {
  const session = useAppSession();
  const [page, setPage] = useState<Loaded<AppPluginsPage>>({ kind: "loading" });
  const [grantList, reloadGrants] = useLoad(loadPluginGrants);
  const [accountList] = useLoad(listAccounts);
  const [dialog, setDialog] = useState<PluginDialog>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [failureCode, setFailureCode] = useState("");
  const keyRef = useRef<HTMLInputElement | null>(null);
  const accounts = keyAccounts(accountList.kind === "ok" ? accountList.value : []);
  const grants = grantList.kind === "ok" ? grantList.value : [];

  function shown(next: AppPluginsPage) {
    setPage({ kind: "ok", value: next });
    const line = changedLine(next);
    if (line) session.showToast(line);
  }

  async function reload() {
    const result = session.noteResult(await loadAppPlugins());
    if (result.kind === "ok") shown(result.value);
    else setPage(result);
    reloadGrants();
  }

  useEffect(() => { void reload(); }, []);

  function fail(line: string, code = "") {
    setFailure(line);
    setFailureCode(code);
  }

  async function attempt<T>(work: () => Promise<Result<T>>): Promise<T | undefined> {
    setBusy(true);
    fail("");
    const result = session.noteResult(await work());
    setBusy(false);
    if (result.kind === "ok") return result.value;
    if (result.kind === "not_ready") fail(NOT_READY);
    else fail(appPluginRefusal(result), result.code);
    return undefined;
  }

  function opened(preview: Preview, folder: string | null, review: boolean) {
    setDialog(preview.kind === "ready" ? { kind: "trust", folder, trust: preview.trust, review } : { kind: "refused", problem: preview.problem });
  }

  async function add() {
    const picked = await attempt(() => pickFolder("app_plugin"));
    if (picked === undefined || picked === null) return;
    const preview = await attempt(() => previewAppPlugin(picked));
    if (preview !== undefined) opened(preview, picked, false);
  }

  async function changed(work: () => Promise<Result<AppPluginsPage>>, words: string) {
    const saved = await attempt(work);
    if (saved === undefined) return;
    setDialog({ kind: "none" });
    shown(saved);
    reloadGrants();
    session.showToast(words);
  }

  async function confirm() {
    if (dialog.kind !== "trust") return;
    const { trust, folder, review } = dialog;
    if (review) await changed(() => trustAppPlugin(trust.name, trust.sha256), `${trust.title} is trusted.`);
    else if (folder !== null) await changed(() => addAppPlugin(folder, trust.sha256, trust.replaces), `${trust.title} was added.`);
  }

  async function review(name: string) {
    const preview = await attempt(() => reviewAppPlugin(name));
    if (preview !== undefined) opened(preview, null, true);
  }

  async function turnOff(name: string, title: string) {
    const users = await attempt(() => appPluginUsers(name));
    if (users === undefined) return;
    const saved = page.kind === "ok" ? page.value.when_turned_off.value : "refuse_new_calls";
    setDialog({ kind: "off", plugin: name, title, users, choice: saved });
  }

  async function confirmOff() {
    if (dialog.kind !== "off") return;
    const saved = page.kind === "ok" ? page.value.when_turned_off.value : null;
    const once = dialog.choice === saved ? null : dialog.choice;
    await changed(() => setAppPluginEnabled(dialog.plugin, false, once), `${dialog.title} is off.`);
  }

  async function saveKey() {
    if (dialog.kind !== "bind") return;
    const { plugin, secret, choice } = dialog;
    if (choice.startsWith("account:")) {
      await changed(() => bindAppPluginAccount(plugin, secret, choice.slice("account:".length)), `${secret} now uses that account's key.`);
      return;
    }
    if (keyRef.current === null) return;
    const key = keyRef.current.value;
    keyRef.current.value = "";
    if (key.trim() === "") {
      fail("Enter a key.");
      return;
    }
    await changed(() => setAppPluginKey(plugin, secret, key), `The key for ${secret} was saved.`);
  }

  function close() {
    if (keyRef.current !== null) keyRef.current.value = "";
    setDialog({ kind: "none" });
    fail("");
  }

  const actions: AppPluginActions = {
    onAdd: () => { if (!busy) void add(); },
    onConfirm: () => { if (!busy) void confirm(); },
    onClose: close,
    onEnabled: (name, title, enabled) => {
      if (busy) return;
      if (enabled) void changed(() => setAppPluginEnabled(name, true, null), `${title} is on.`);
      else void turnOff(name, title);
    },
    onConfirmOff: () => { if (!busy) void confirmOff(); },
    onOffChoice: (choice) => { if (dialog.kind === "off") setDialog({ ...dialog, choice }); },
    onWhenOff: (choice) => { if (!busy) void changed(() => setAppPluginWhenOff(choice), `Turning a plugin off now uses "${TURN_OFF_WORDS[choice].title}".`); },
    onReview: (name) => { if (!busy) void review(name); },
    onRemove: (plugin) => {
      fail("");
      setDialog({ kind: "remove", plugin: plugin.name, title: plugin.title, keyed: plugin.secrets.some((secret) => secret.binding.kind === "key") });
    },
    onConfirmRemove: () => { if (dialog.kind === "remove" && !busy) void changed(() => removeAppPlugin(dialog.plugin), `${dialog.title} was removed.`); },
    onBind: (plugin, secret) => {
      fail("");
      setDialog({ kind: "bind", plugin: plugin.name, title: plugin.title, secret, hosts: keyHosts(plugin), choice: accounts.length > 0 ? `account:${accounts[0].id}` : "key" });
    },
    onKeyChoice: (choice) => { if (dialog.kind === "bind") setDialog({ ...dialog, choice }); },
    onSaveKey: () => { if (!busy) void saveKey(); },
    onUnbind: (name, secret) => { if (!busy) void changed(() => unbindAppPlugin(name, secret), `${secret} no longer has a key.`); },
    onRetry: () => { void reload(); },
  };

  return <div className="stack">
    <AppPluginsView page={page} grants={grants} dialog={dialog} accounts={accounts} busy={busy} failure={failure} failureCode={failureCode} keyRef={keyRef} actions={actions} />
  </div>;
}
