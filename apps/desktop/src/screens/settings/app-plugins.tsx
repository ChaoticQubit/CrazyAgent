import { useRef, useState, type RefObject } from "react";
import { listAccounts, type AccountView, type Result } from "../../api";
import {
  addAppPlugin, appPluginRefusal, bindAppPluginAccount, keyAccounts, loadAppPlugins, megabytes, pickAppPluginFile, pluginState, previewAppPlugin, problemText,
  removeAppPlugin, reviewAppPlugin, setAppPluginEnabled, setAppPluginKey, trustAppPlugin, unbindAppPlugin,
  type Listing, type PluginView, type Preview, type Problem, type RunsView, type SecretView, type TrustView,
} from "../../api-app-plugins";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, LoadError, NotReady, Notice, RadioCard, SecretField, SettingSection, Skeleton, StatusBadge, Switch, TextArea } from "../../ui";
import "../project/projects.css";

const NOT_READY = "This isn't ready in this build yet.";
const MANIFEST_CHARS_MAX = 16384;
const KEY_SOURCE = "app-plugin-key-source";

/** What App plugins has open over the list: nothing, the paste box, a trust screen (for a new
 * manifest, or a review of a file in the owner's folder), a refused manifest, the key dialog
 * for one secret, or the remove confirm. */
export type PluginDialog =
  | { kind: "none" }
  | { kind: "add"; yaml: string }
  | { kind: "trust"; yaml: string | null; trust: TrustView; review: boolean }
  | { kind: "refused"; yaml: string | null; problem: Problem }
  | { kind: "bind"; plugin: string; title: string; secret: string; choice: string }
  | { kind: "remove"; plugin: string; title: string };

/** Everything the App plugins view asks its container to do. */
export interface AppPluginActions {
  onAdd: () => void;
  onYaml: (yaml: string) => void;
  onPick: () => void;
  onCheck: () => void;
  onConfirm: () => void;
  onEdit: () => void;
  onClose: () => void;
  onEnabled: (name: string, enabled: boolean) => void;
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
  if (runs.kind === "local") return `Runs on this computer with the built-in model, a ${megabytes(runs.download_bytes)} download`;
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
  <p>Plugins CrazyAgent itself runs, such as the embedding models search can rank with. Each one shows what it reaches before you trust it.</p>
</div></div>;

function PluginRow({ plugin, busy, actions }: { plugin: PluginView; busy: boolean; actions: AppPluginActions }) {
  const state = pluginState(plugin);
  const blocked = plugin.status.kind === "refused" ? "Refused plugins can't run." : plugin.status.kind === "needs_review" ? "Review it first." : undefined;
  const origin = plugin.origin === "bundled" ? "Built in" : "Added by you";
  return <li className="pj-task" aria-label={plugin.title}>
    <div className="pj-row between">
      <div className="pj-stack-s">
        <strong>{plugin.title}</strong>
        <span className="muted small">{`${plugin.name}, version ${plugin.version}. ${origin}.`}</span>
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
        onChecked={(on) => { if (!busy) actions.onEnabled(plugin.name, on); }} />
      {plugin.status.kind === "needs_review" && <Button size="small" aria-label={`Review ${plugin.title}`} disabled={busy} onClick={() => actions.onReview(plugin.name)}>Review</Button>}
      {plugin.origin === "user" && <Button size="small" variant="danger-text" aria-label={`Remove ${plugin.title}`} disabled={busy} onClick={() => actions.onRemove(plugin)}>Remove</Button>}
    </div>
  </li>;
}

function TrustScreen({ trust }: { trust: TrustView }) {
  return <div className="pj-stack-s">
    {trust.description && <p>{trust.description}</p>}
    <ul className="pj-rows" aria-label="What this plugin is and reaches">
      <li>{`Kind: ${trust.kind === "embedding" ? "Embedding" : trust.kind}`}</li>
      <li>{`Version ${trust.version}`}</li>
      <li>{runsLine(trust.runs)}</li>
      {trust.runs.kind === "http" && <li>{`Address: ${trust.runs.url}`}</li>}
      <li>{`Secrets it needs: ${trust.secrets.length > 0 ? trust.secrets.join(", ") : "none"}`}</li>
      <li className="mono small">{`SHA-256 ${trust.sha256}`}</li>
    </ul>
    {trust.replaces && <Notice tone="warn">{`Replaces your plugin named ${trust.name}.`}</Notice>}
  </div>;
}

function PluginDialogs({ dialog, accounts, busy, failure, keyRef, actions }: {
  dialog: PluginDialog;
  accounts: readonly AccountView[];
  busy: boolean;
  failure: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: AppPluginActions;
}) {
  const problem = failure ? <Notice tone="danger" alert>{failure}</Notice> : null;
  if (dialog.kind === "add") {
    return <Dialog open wide title="Add an app plugin" onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" disabled={dialog.yaml.trim() === ""} pending={busy} pendingLabel="Checking..." onClick={actions.onCheck}>Check</Button></>}>
      <div className="pj-stack-s">
        {problem}
        <TextArea id="app-plugin-yaml" label="Plugin manifest (YAML)" rows={12} value={dialog.yaml} maxLength={MANIFEST_CHARS_MAX} onValue={actions.onYaml}
          helper="Paste a manifest or pick a .yaml file. Nothing runs until you confirm its trust screen." />
        <span><Button disabled={busy} onClick={actions.onPick}>Pick a .yaml file</Button></span>
      </div>
    </Dialog>;
  }
  if (dialog.kind === "trust") {
    return <Dialog open wide title={dialog.review ? `Review ${dialog.trust.title}` : `Trust ${dialog.trust.title}?`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onConfirm}>Confirm</Button></>}>
      {problem}
      <TrustScreen trust={dialog.trust} />
    </Dialog>;
  }
  if (dialog.kind === "refused") {
    return <Dialog open title={dialog.yaml === null ? "This plugin can't be trusted" : "This manifest can't be added"} onClose={actions.onClose}
      footer={<>{dialog.yaml !== null && <Button onClick={actions.onEdit}>Edit manifest</Button>}<Button onClick={actions.onClose}>Cancel</Button></>}>
      <Notice tone="danger">{problemText(dialog.problem)}</Notice>
    </Dialog>;
  }
  if (dialog.kind === "bind") {
    return <Dialog open title={`Key for ${dialog.secret}`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onSaveKey}>Save</Button></>}>
      <div className="pj-stack-s">
        <p className="secondary">{`Where ${dialog.title} gets its ${dialog.secret}.`}</p>
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
      {`Its manifest is deleted from your plugins folder. ${failure}`.trim()}
    </ConfirmDialog>;
  }
  return null;
}

/** Settings, App plugins as a pure view: the title in every state, then every plugin with its
 * one status label, where it came from, how it runs, where each secret's key comes from, its
 * switch, Review and Remove, and Add plugin; over it the open dialog. Inputs: the listing, the
 * open dialog, the API-key accounts, a busy flag, the last failure, the key field and the
 * actions. Output: the tab. */
export function AppPluginsView({ listing, dialog, accounts, busy, failure, keyRef, actions }: {
  listing: Loaded<Listing>;
  dialog: PluginDialog;
  accounts: readonly AccountView[];
  busy: boolean;
  failure: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: AppPluginActions;
}) {
  if (listing.kind === "loading") return <>{TOP}<Skeleton shape="row" count={3} /></>;
  if (listing.kind === "not_ready") return <>{TOP}<NotReady what="App plugins" /></>;
  if (listing.kind === "error") return <>{TOP}<LoadError message={appPluginRefusal(listing)} onRetry={actions.onRetry} /></>;
  const { plugins, more } = listing.value;
  return <>
    {TOP}
    <div className="pj set-groups">
      <SettingSection title="Plugins" about="Built-in plugins ship with CrazyAgent. Yours live in your plugins folder and run only while their file matches what you trusted."
        actions={<Button variant="primary" disabled={busy} onClick={actions.onAdd}>Add plugin</Button>}>
        <div className="set-body">
          {dialog.kind === "none" && failure && <Notice tone="danger" alert>{failure}</Notice>}
          <ul className="pj-rows" aria-label="App plugins">{plugins.map((plugin) => <PluginRow key={plugin.name} plugin={plugin} busy={busy} actions={actions} />)}</ul>
          {more && <p className="muted small">{`Showing the first ${plugins.length} plugins.`}</p>}
        </div>
      </SettingSection>
    </div>
    <PluginDialogs dialog={dialog} accounts={accounts} busy={busy} failure={failure} keyRef={keyRef} actions={actions} />
  </>;
}

/** Settings, App plugins: reads the plugins, adds a pasted or picked manifest only after its
 * trust screen is confirmed, reviews a changed file, turns plugins on and off, removes the
 * owner's own, and binds each secret to an API-key account or a key sent once. */
export function AppPluginsSettings() {
  const session = useAppSession();
  const [listing, reload] = useLoad(loadAppPlugins);
  const [accountList] = useLoad(listAccounts);
  const [dialog, setDialog] = useState<PluginDialog>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const keyRef = useRef<HTMLInputElement | null>(null);
  const accounts = keyAccounts(accountList.kind === "ok" ? accountList.value : []);

  async function attempt<T>(work: () => Promise<Result<T>>): Promise<T | undefined> {
    setBusy(true);
    setFailure("");
    const result = session.noteResult(await work());
    setBusy(false);
    if (result.kind === "ok") return result.value;
    setFailure(result.kind === "not_ready" ? NOT_READY : appPluginRefusal(result));
    return undefined;
  }

  function opened(preview: Preview, yaml: string | null, review: boolean) {
    setDialog(preview.kind === "ready" ? { kind: "trust", yaml, trust: preview.trust, review } : { kind: "refused", yaml, problem: preview.problem });
  }

  async function check(yaml: string) {
    const preview = await attempt(() => previewAppPlugin(yaml));
    if (preview !== undefined) opened(preview, yaml, false);
  }

  async function pick() {
    const picked = await attempt(pickAppPluginFile);
    if (picked === undefined || picked === null) return;
    setDialog({ kind: "add", yaml: picked });
    await check(picked);
  }

  async function confirm() {
    if (dialog.kind !== "trust") return;
    const { trust, yaml, review } = dialog;
    if (!review && yaml === null) return;
    const saved = review ? await attempt(() => trustAppPlugin(trust.name, trust.sha256)) : await attempt(() => addAppPlugin(yaml ?? "", trust.sha256, trust.replaces));
    if (saved === undefined) return;
    setDialog({ kind: "none" });
    session.showToast(review ? `${trust.title} is trusted.` : `${trust.title} was added.`);
    reload();
  }

  async function review(name: string) {
    const preview = await attempt(() => reviewAppPlugin(name));
    if (preview !== undefined) opened(preview, null, true);
  }

  async function changed(work: () => Promise<Result<Listing>>, words: string) {
    const saved = await attempt(work);
    if (saved === undefined) return;
    setDialog({ kind: "none" });
    session.showToast(words);
    reload();
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
      setFailure("Enter a key.");
      return;
    }
    await changed(() => setAppPluginKey(plugin, secret, key), `The key for ${secret} was saved.`);
  }

  function close() {
    if (keyRef.current !== null) keyRef.current.value = "";
    setDialog({ kind: "none" });
    setFailure("");
  }

  const actions: AppPluginActions = {
    onAdd: () => { setFailure(""); setDialog({ kind: "add", yaml: "" }); },
    onYaml: (yaml) => { if (dialog.kind === "add") setDialog({ kind: "add", yaml }); },
    onPick: () => { if (!busy) void pick(); },
    onCheck: () => { if (dialog.kind === "add" && !busy) void check(dialog.yaml); },
    onConfirm: () => { if (!busy) void confirm(); },
    onEdit: () => { if (dialog.kind === "refused" && dialog.yaml !== null) setDialog({ kind: "add", yaml: dialog.yaml }); },
    onClose: close,
    onEnabled: (name, enabled) => { if (!busy) void changed(() => setAppPluginEnabled(name, enabled), enabled ? "Plugin turned on." : "Plugin turned off."); },
    onReview: (name) => { if (!busy) void review(name); },
    onRemove: (plugin) => { setFailure(""); setDialog({ kind: "remove", plugin: plugin.name, title: plugin.title }); },
    onConfirmRemove: () => { if (dialog.kind === "remove" && !busy) void changed(() => removeAppPlugin(dialog.plugin), `${dialog.title} was removed.`); },
    onBind: (plugin, secret) => { setFailure(""); setDialog({ kind: "bind", plugin: plugin.name, title: plugin.title, secret, choice: accounts.length > 0 ? `account:${accounts[0].id}` : "key" }); },
    onKeyChoice: (choice) => { if (dialog.kind === "bind") setDialog({ ...dialog, choice }); },
    onSaveKey: () => { if (!busy) void saveKey(); },
    onUnbind: (name, secret) => { if (!busy) void changed(() => unbindAppPlugin(name, secret), `${secret} no longer has a key.`); },
    onRetry: reload,
  };

  return <div className="stack">
    <AppPluginsView listing={listing} dialog={dialog} accounts={accounts} busy={busy} failure={failure} keyRef={keyRef} actions={actions} />
  </div>;
}
