import { useEffect, useRef, useState, type FormEvent, type RefObject } from "react";
import type { Result } from "../../api";
import { pickAppPluginFile, problemText, type Problem } from "../../api-app-plugins";
import {
  CONNECTOR_DEFAULTS, CONNECTORS_PAGE, NATIVE_CHOICES, SIGN_IN_READ_MS, SIGN_IN_READS_MAX, SIZE_CHOICES, WAIT_CHOICES, WRITE_CHOICES, addConnector, cancelConnectorSignIn,
  clearConnectorKey, clearConnectorOwnApp, connectorState, keyLine, loadConnectorReach, loadConnectorSignIn, loadConnectors, originLine, ownAppHeading, ownAppRemovedLine,
  ownAppSavedLine, reachesLine, redirectLine, removeConnector, reviewConnector, previewConnector, saveConnectorSettings, setConnectorKey, setConnectorOwnApp, setConnectorSignInWith,
  settingsToSave, signInConnector, signInStep, signOutConnector, signedOutLine, signsInWithLine,
  trustConnector, trustLines, type ConnectorPreview, type ConnectorView, type ConnectorsView, type ConnectorTrustView, type NativeConnectors, type SettingPicks, type SignInWith,
  type SizeCap, type Wait, type WritePolicy,
} from "../../api-connectors";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, LoadError, NotReady, Notice, SecretField, SettingLine, SettingSection, Skeleton, StatusBadge, TextArea, TextField, formatRoute, navigate } from "../../ui";
import { ConnectorDetailView } from "./connector-detail";
import "../notifications/notifications.css";
import "../project/projects.css";

const NOT_READY = "The engine is not ready yet. Try again.";
const MANIFEST_CHARS_MAX = 16384;
const CLIENT_ID_CHARS_MAX = 255;

/** What Settings, Connectors has open over the page: nothing, the paste box, a trust screen
 * (for a new manifest, or a review of a listed file), a refused manifest, the key entry for
 * one secret, the owner's own OAuth app entry, or the remove confirm. */
export type ConnectorDialog =
  | { kind: "none" }
  | { kind: "add"; yaml: string }
  | { kind: "trust"; yaml: string | null; trust: ConnectorTrustView; review: boolean }
  | { kind: "refused"; yaml: string | null; problem: Problem }
  | { kind: "key"; connector: string; title: string; hosts: string[]; secret: string }
  | { kind: "own_app"; connector: string; title: string; redirect: string; clientId: string }
  | { kind: "remove"; connector: string; title: string };

/** Everything the Connectors views ask their container to do. */
export interface ConnectorActions {
  onAdd: () => void;
  onYaml: (yaml: string) => void;
  onPick: () => void;
  onCheck: () => void;
  onConfirm: () => void;
  onEdit: () => void;
  onClose: () => void;
  onReview: (name: string) => void;
  onRemove: (connector: ConnectorView) => void;
  onConfirmRemove: () => void;
  onSetKey: (connector: ConnectorView, secret: string) => void;
  onSaveKey: () => void;
  onClearKey: (name: string, secret: string) => void;
  onPickWrite: (write: WritePolicy) => void;
  onPickWait: (wait: Wait) => void;
  onSignInWith: (connector: ConnectorView, way: SignInWith) => void;
  onSignIn: (connector: ConnectorView) => void;
  onCancelSignIn: (connector: ConnectorView) => void;
  onSignOut: (connector: ConnectorView) => void;
  onUseOwnApp: (connector: ConnectorView) => void;
  onClientId: (clientId: string) => void;
  onSaveApp: () => void;
  onRemoveApp: (connector: ConnectorView) => void;
  onPickNative: (native: NativeConnectors) => void;
  onPickSize: (sizeCap: SizeCap) => void;
  onSave: () => void;
  onRetry: () => void;
}

const TOP = <div className="settings-top"><div>
  <h2>{CONNECTORS_PAGE.title}</h2>
  <p>{CONNECTORS_PAGE.purpose}</p>
</div></div>;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** One catalog row: the title, which opens the connector's page unless its file is
 * refused (a refused file shadowed by a built-in one would open the built-in page), where
 * it came from, its state with its line, the description and every host. Input: the
 * connector. Output: the row. */
function CatalogRow({ connector }: { connector: ConnectorView }) {
  const state = connectorState(connector);
  const refused = connector.status.kind === "refused";
  return <li className="pj-task" data-connector={connector.name} data-origin={connector.origin}>
    <div className="pj-row between">
      <div className="pj-stack-s">
        {refused ? <strong>{connector.title}</strong> : <a href={formatRoute({ name: "settings", tab: "connectors", connector: connector.name })}>{connector.title}</a>}
        <span className="muted small">{originLine(connector)}</span>
      </div>
      <StatusBadge word={state.word} tone={state.tone} />
    </div>
    {state.detail && <p className="small">{state.detail}</p>}
    {refused && <p className="small">{CONNECTORS_PAGE.refusedFile}</p>}
    {connector.description && <p className="secondary">{connector.description}</p>}
    {connector.runs && <p className="small">{reachesLine(connector.runs.hosts)}</p>}
  </li>;
}

/** The trust screen of a connector manifest: its description, then every line the engine
 * acts on as plain text, and whether it replaces the owner's own. Input: the trust view.
 * Output: the screen. */
function ConnectorTrustScreen({ trust }: { trust: ConnectorTrustView }) {
  return <div className="pj-stack-s">
    {trust.description && <p>{trust.description}</p>}
    <ul className="pj-rows" aria-label="What this connector is and reaches">
      {trustLines(trust).map((line) => <li key={line} className={line.startsWith("SHA-256 ") ? "mono small" : undefined}>{line}</li>)}
    </ul>
    {trust.replaces && <Notice tone="warn">{`Replaces your connector named ${trust.name}.`}</Notice>}
  </div>;
}

function ConnectorDialogs({ dialog, busy, failure, keyRef, actions }: {
  dialog: ConnectorDialog;
  busy: boolean;
  failure: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: ConnectorActions;
}) {
  const problem = failure ? <Notice tone="danger" alert>{failure}</Notice> : null;
  if (dialog.kind === "add") {
    return <Dialog open wide title={CONNECTORS_PAGE.addTitle} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" disabled={dialog.yaml.trim() === ""} pending={busy} pendingLabel="Checking..." onClick={actions.onCheck}>Check</Button></>}>
      <div className="pj-stack-s">
        {problem}
        <TextArea id="connector-yaml" label={CONNECTORS_PAGE.manifest} rows={12} value={dialog.yaml} maxLength={MANIFEST_CHARS_MAX + 1} onValue={actions.onYaml}
          helper={CONNECTORS_PAGE.manifestHelp} />
        <span><Button disabled={busy} onClick={actions.onPick}>{CONNECTORS_PAGE.pick}</Button></span>
      </div>
    </Dialog>;
  }
  if (dialog.kind === "trust") {
    return <Dialog open wide title={dialog.review ? `Review ${dialog.trust.title}` : `Trust ${dialog.trust.title}?`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onConfirm}>Confirm</Button></>}>
      {problem}
      <ConnectorTrustScreen trust={dialog.trust} />
    </Dialog>;
  }
  if (dialog.kind === "refused") {
    return <Dialog open title={dialog.yaml === null ? CONNECTORS_PAGE.refusedReview : CONNECTORS_PAGE.refusedAdd} onClose={actions.onClose}
      footer={<>{dialog.yaml !== null && <Button onClick={actions.onEdit}>Edit manifest</Button>}<Button onClick={actions.onClose}>Cancel</Button></>}>
      <Notice tone="danger">{problemText(dialog.problem)}</Notice>
    </Dialog>;
  }
  if (dialog.kind === "key") {
    return <Dialog open title={`Key for ${dialog.secret}`} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onSaveKey}>Save</Button></>}>
      <div className="pj-stack-s">
        <p className="secondary">{keyLine(dialog.title, dialog.hosts)}</p>
        {problem}
        <SecretField id="connector-key" label="Key" inputRef={keyRef ?? { current: null }} helper={CONNECTORS_PAGE.keyHelp} />
      </div>
    </Dialog>;
  }
  if (dialog.kind === "own_app") {
    return <Dialog open title={ownAppHeading(dialog.title)} onClose={actions.onClose} busy={busy}
      footer={<><Button disabled={busy} onClick={actions.onClose}>Cancel</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." onClick={actions.onSaveApp}>Save</Button></>}>
      <div className="pj-stack-s">
        <p className="secondary">{redirectLine(dialog.redirect)}</p>
        <p className="small muted">{CONNECTORS_PAGE.ownAppHelp}</p>
        {problem}
        <TextField id="connector-client-id" label={CONNECTORS_PAGE.clientId} value={dialog.clientId} maxLength={CLIENT_ID_CHARS_MAX} onValue={actions.onClientId} autoComplete="off" spellCheck={false} />
        <SecretField id="connector-app-secret" label={CONNECTORS_PAGE.clientSecret} inputRef={keyRef ?? { current: null }} helper={CONNECTORS_PAGE.clientSecretHelp} />
      </div>
    </Dialog>;
  }
  if (dialog.kind === "remove") {
    return <ConfirmDialog open title={`Remove ${dialog.title}?`} confirmLabel="Remove" pending={busy} onConfirm={actions.onConfirmRemove} onCancel={actions.onClose}>
      {`${CONNECTORS_PAGE.removeBody} ${failure}`.trim()}
    </ConfirmDialog>;
  }
  return null;
}

/** One connector setting as a radio fieldset, marked when away from its default. Inputs: its
 * label, help, setting key and input name, the choices, the value shown, the default, a
 * saving flag and the pick handler. Output: the setting line. */
function SettingChoices<T extends string>({ label, help, setting, name, choices, shown, fallback, saving, onPick }: {
  label: string;
  help: string;
  setting: string;
  name: string;
  choices: { choice: T; label: string }[];
  shown: T;
  fallback: T;
  saving: boolean;
  onPick: (choice: T) => void;
}) {
  assert(choices.some((option) => option.choice === shown), `no ${setting} choice ${shown}`);
  return <SettingLine wide label={label} changed={shown !== fallback} help={help}>
    <fieldset className="notice-choices" disabled={saving} data-setting={setting}>
      <legend className="visually-hidden">{label}</legend>
      {choices.map((option) => <label key={option.choice} className={option.choice === shown ? "on" : undefined}>
        <input type="radio" name={name} value={option.choice} checked={option.choice === shown} onChange={() => onPick(option.choice)} />
        {option.label}
      </label>)}
    </fieldset>
  </SettingLine>;
}

/** The four connector settings this page owns as radio fieldsets, marked when away from the
 * default, the locked line with its reason, and Save changes, enabled only while a pick
 * differs from the settings read. Inputs: the page read, the picks, a saving flag, the last
 * problem and the actions. Output: the form. */
function SettingsForm({ page, picks, saving, problem, actions }: {
  page: ConnectorsView;
  picks: SettingPicks;
  saving: boolean;
  problem: string;
  actions: ConnectorActions;
}) {
  const read = page.settings;
  const changed = settingsToSave(read, picks) !== null;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) actions.onSave();
  }
  return <form className="pj set-groups" aria-label={CONNECTORS_PAGE.settings} noValidate onSubmit={submit}>
    <SettingSection title={CONNECTORS_PAGE.settings}>
      <SettingChoices label={CONNECTORS_PAGE.write} help={CONNECTORS_PAGE.writeHelp} setting="write" name="connector-write" choices={WRITE_CHOICES}
        shown={picks.write ?? read.write} fallback={CONNECTOR_DEFAULTS.write} saving={saving} onPick={actions.onPickWrite} />
      <SettingChoices label={CONNECTORS_PAGE.wait} help={CONNECTORS_PAGE.waitHelp} setting="wait" name="connector-wait" choices={WAIT_CHOICES}
        shown={picks.wait ?? read.wait} fallback={CONNECTOR_DEFAULTS.wait} saving={saving} onPick={actions.onPickWait} />
      <SettingChoices label={CONNECTORS_PAGE.native} help={CONNECTORS_PAGE.nativeHelp} setting="native" name="connector-native" choices={NATIVE_CHOICES}
        shown={picks.native ?? read.native} fallback={CONNECTOR_DEFAULTS.native} saving={saving} onPick={actions.onPickNative} />
      <SettingChoices label={CONNECTORS_PAGE.sizeCap} help={CONNECTORS_PAGE.sizeCapHelp} setting="size_cap" name="connector-size-cap" choices={SIZE_CHOICES}
        shown={picks.size_cap ?? read.size_cap} fallback={CONNECTOR_DEFAULTS.size_cap} saving={saving} onPick={actions.onPickSize} />
      <SettingLine label={CONNECTORS_PAGE.locked} help={CONNECTORS_PAGE.lockedReason}>
        <span data-setting="locked">{CONNECTORS_PAGE.lockedValue}</span>
      </SettingLine>
      <div className="set-body">
        {problem && <Notice tone="danger" alert>{problem}</Notice>}
        <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
      </div>
    </SettingSection>
  </form>;
}

/** Settings, Connectors as a view of its state: the title in every state, then the catalog
 * with each connector's state, hosts and page, Add connector, and the connector settings
 * with the locked line; or the open connector's own page; over either the open dialog.
 * Inputs: the page read, the harness rows, the connector the route opened, the open dialog, the picked
 * settings, busy and saving flags, the connector whose sign-in waits, the last failure and
 * save problem, the key field and the actions. Output: the tab. */
export function ConnectorsSettingsView({ view, reach, open, dialog, write, wait, native, sizeCap, busy, saving, signing = null, failure, problem, keyRef, actions }: {
  view: Loaded<ConnectorsView>;
  reach: Loaded<string[]>;
  open: string | null;
  dialog: ConnectorDialog;
  write: WritePolicy | null;
  wait: Wait | null;
  native: NativeConnectors | null;
  sizeCap: SizeCap | null;
  busy: boolean;
  saving: boolean;
  signing?: string | null;
  failure: string;
  problem: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: ConnectorActions;
}) {
  if (view.kind === "loading") return <>{TOP}<Skeleton shape="row" count={3} /></>;
  if (view.kind === "not_ready") return <>{TOP}<NotReady what={CONNECTORS_PAGE.title} /></>;
  if (view.kind === "error") return <>{TOP}<LoadError message={`Couldn't load the connectors. ${view.message}`} onRetry={actions.onRetry} /></>;
  const page = view.value;
  const dialogs = <ConnectorDialogs dialog={dialog} busy={busy} failure={failure} keyRef={keyRef} actions={actions} />;
  if (open !== null) {
    const connector = page.connectors.find((listed) => listed.name === open) ?? null;
    const elsewhere = signing === null || signing === open ? null : page.connectors.find((listed) => listed.name === signing)?.title ?? signing;
    return <>
      <ConnectorDetailView name={open} connector={connector} busy={busy} waiting={signing === open} elsewhere={elsewhere} failure={dialog.kind === "none" ? failure : ""} actions={actions} />
      {dialogs}
    </>;
  }
  return <>
    {TOP}
    <div className="pj set-groups">
      <SettingSection title={CONNECTORS_PAGE.catalog} about={CONNECTORS_PAGE.catalogAbout}
        actions={<Button variant="primary" disabled={busy} onClick={actions.onAdd}>{CONNECTORS_PAGE.add}</Button>}>
        <div className="set-body">
          {dialog.kind === "none" && failure && <Notice tone="danger" alert>{failure}</Notice>}
          <ul className="pj-rows" aria-label={CONNECTORS_PAGE.catalog}>{page.connectors.map((connector) => <CatalogRow key={`${connector.origin}:${connector.name}`} connector={connector} />)}</ul>
          {page.more && <p className="muted small">{`Showing the first ${page.connectors.length} connectors.`}</p>}
        </div>
      </SettingSection>
    </div>
    {reach.kind === "ok" && reach.value.length > 0 && <div className="pj set-groups">
      <SettingSection title={CONNECTORS_PAGE.harnesses} about={CONNECTORS_PAGE.harnessesAbout}>
        <ul className="set-body" aria-label={CONNECTORS_PAGE.harnesses}>{reach.value.map((row) => <li key={row}>{row}</li>)}</ul>
      </SettingSection>
    </div>}
    <SettingsForm page={page} picks={{ write, wait, native, size_cap: sizeCap }} saving={saving} problem={problem} actions={actions} />
    {dialogs}
  </>;
}

/** Settings, Connectors: reads the page, adds a pasted or picked manifest only after its
 * trust screen is confirmed, reviews a changed file, removes the owner's own, sets and
 * clears each secret's key, and saves the four connector settings at the version read. A
 * change reads the page again, so the version a save names is always the last read, and
 * removing the connector whose page is open goes back to the catalog.
 * Input: the connector whose page the route opened, if any. Output: the tab. */
export function ConnectorsSettings({ connector }: { connector?: string }) {
  const session = useAppSession();
  const [view, reload] = useLoad(() => loadConnectors(null));
  const [reach] = useLoad(loadConnectorReach);
  const [dialog, setDialog] = useState<ConnectorDialog>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [write, setWrite] = useState<WritePolicy | null>(null);
  const [wait, setWait] = useState<Wait | null>(null);
  const [native, setNative] = useState<NativeConnectors | null>(null);
  const [sizeCap, setSizeCap] = useState<SizeCap | null>(null);
  const [saving, setSaving] = useState(false);
  const [reading, setReading] = useState(false);
  const [problem, setProblem] = useState("");
  const [signing, setSigning] = useState<{ name: string; title: string } | null>(null);
  const keyRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => { setReading(false); }, [view]);
  useEffect(() => {
    if (signing === null) return undefined;
    let live = true;
    void (async () => {
      for (let reads = 0; reads < SIGN_IN_READS_MAX; reads += 1) {
        await new Promise((done) => { window.setTimeout(done, SIGN_IN_READ_MS); });
        if (!live) return;
        const result = session.noteResult(await loadConnectorSignIn(signing.name));
        if (!live) return;
        if (result.kind !== "ok") {
          setFailure(result.kind === "not_ready" ? NOT_READY : `${CONNECTORS_PAGE.notReadSignIn} ${result.message}`);
          break;
        }
        const step = signInStep(result.value, signing.title);
        if (step.reading) continue;
        if (step.toast !== null) session.showToast(step.toast);
        if (step.failure !== null) setFailure(step.failure);
        break;
      }
      if (!live) return;
      setSigning(null);
      reload();
    })();
    return () => { live = false; };
  }, [signing]);
  useEffect(() => {
    if (view.kind !== "ok") return;
    if (write === view.value.settings.write) setWrite(null);
    if (wait === view.value.settings.wait) setWait(null);
    if (native === view.value.settings.native) setNative(null);
    if (sizeCap === view.value.settings.size_cap) setSizeCap(null);
  }, [view, write, wait, native, sizeCap]);

  async function attempt<T>(work: () => Promise<Result<T>>): Promise<T | undefined> {
    setBusy(true);
    setFailure("");
    const result = session.noteResult(await work());
    setBusy(false);
    if (result.kind === "ok") return result.value;
    setFailure(result.kind === "not_ready" ? NOT_READY : result.message);
    return undefined;
  }

  function opened(preview: ConnectorPreview, yaml: string | null, review: boolean) {
    setDialog(preview.kind === "ready" ? { kind: "trust", yaml, trust: preview.trust, review } : { kind: "refused", yaml, problem: preview.problem });
  }

  async function check(yaml: string) {
    const preview = await attempt(() => previewConnector(yaml));
    if (preview !== undefined) opened(preview, yaml, false);
  }

  async function pick() {
    const picked = await attempt(pickAppPluginFile);
    if (picked === undefined || picked === null) return;
    setDialog({ kind: "add", yaml: picked });
    await check(picked);
  }

  async function changed(work: () => Promise<Result<ConnectorsView>>, words: string): Promise<boolean> {
    const saved = await attempt(work);
    if (saved === undefined) return false;
    setDialog({ kind: "none" });
    session.showToast(words);
    reload();
    return true;
  }

  async function signIn(shown: ConnectorView) {
    if (await attempt(() => signInConnector(shown.name)) !== undefined) setSigning({ name: shown.name, title: shown.title });
  }

  async function remove() {
    if (dialog.kind !== "remove") return;
    const { connector: name, title } = dialog;
    if (await changed(() => removeConnector(name), `${title} was removed.`) && connector === name) navigate({ name: "settings", tab: "connectors" }, true);
  }

  async function confirm() {
    if (dialog.kind !== "trust") return;
    const { trust, yaml, review } = dialog;
    if (review) await changed(() => trustConnector(trust.name, trust.sha256), `${trust.title} is trusted.`);
    else if (yaml !== null) await changed(() => addConnector(yaml, trust.sha256, trust.replaces), `${trust.title} was added.`);
  }

  async function review(name: string) {
    const preview = await attempt(() => reviewConnector(name));
    if (preview !== undefined) opened(preview, null, true);
  }

  async function saveKey() {
    if (dialog.kind !== "key" || keyRef.current === null) return;
    const key = keyRef.current.value;
    keyRef.current.value = "";
    if (key.trim() === "") {
      setFailure(CONNECTORS_PAGE.enterKey);
      return;
    }
    const { connector: name, secret } = dialog;
    await changed(() => setConnectorKey(name, secret, key), `The key for ${secret} was saved.`);
  }

  async function saveApp() {
    if (dialog.kind !== "own_app" || keyRef.current === null) return;
    const clientId = dialog.clientId.trim();
    if (clientId === "") {
      setFailure(CONNECTORS_PAGE.enterClientId);
      return;
    }
    const secret = keyRef.current.value;
    keyRef.current.value = "";
    const { connector: name, title } = dialog;
    await changed(() => setConnectorOwnApp(name, clientId, secret), ownAppSavedLine(title));
  }

  async function save() {
    if (view.kind !== "ok") return;
    const read = view.value.settings;
    const settings = settingsToSave(read, { write, wait, native, size_cap: sizeCap });
    if (settings === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveConnectorSettings(settings, view.value.version);
    setSaving(false);
    if (result.kind === "error") { setProblem(`${CONNECTORS_PAGE.notSaved} ${result.message}`); return; }
    if (result.kind === "not_ready") { setProblem(NOT_READY); return; }
    setReading(true);
    if (result.value.kind === "stale") { setWrite(null); setWait(null); setNative(null); setSizeCap(null); setProblem(CONNECTORS_PAGE.stale); reload(); return; }
    session.showToast(CONNECTORS_PAGE.saved);
    reload();
  }

  function close() {
    if (keyRef.current !== null) keyRef.current.value = "";
    setDialog({ kind: "none" });
    setFailure("");
  }

  const actions: ConnectorActions = {
    onAdd: () => { setFailure(""); setDialog({ kind: "add", yaml: "" }); },
    onYaml: (yaml) => { if (dialog.kind === "add") setDialog({ kind: "add", yaml }); },
    onPick: () => { if (!busy) void pick(); },
    onCheck: () => { if (dialog.kind === "add" && !busy) void check(dialog.yaml); },
    onConfirm: () => { if (!busy) void confirm(); },
    onEdit: () => { if (dialog.kind === "refused" && dialog.yaml !== null) setDialog({ kind: "add", yaml: dialog.yaml }); },
    onClose: close,
    onReview: (name) => { if (!busy) void review(name); },
    onRemove: (shown) => { setFailure(""); setDialog({ kind: "remove", connector: shown.name, title: shown.title }); },
    onConfirmRemove: () => { if (!busy) void remove(); },
    onSetKey: (shown, secret) => {
      assert(shown.runs !== null, "only a connector with an address offers Set key");
      setFailure("");
      setDialog({ kind: "key", connector: shown.name, title: shown.title, hosts: shown.runs.hosts, secret });
    },
    onSaveKey: () => { if (!busy) void saveKey(); },
    onClearKey: (name, secret) => { if (!busy) void changed(() => clearConnectorKey(name, secret), `${secret} no longer has a key.`); },
    onPickWrite: (choice) => { setWrite(choice); setProblem(""); },
    onPickWait: (choice) => { setWait(choice); setProblem(""); },
    onSignInWith: (shown, way) => { if (!busy) void changed(() => setConnectorSignInWith(shown.name, way), signsInWithLine(shown.title, way)); },
    onSignIn: (shown) => { if (!busy && signing === null) void signIn(shown); },
    onCancelSignIn: (shown) => { if (!busy && signing?.name === shown.name) void attempt(() => cancelConnectorSignIn(shown.name)); },
    onSignOut: (shown) => { if (!busy) void changed(() => signOutConnector(shown.name), signedOutLine(shown.title)); },
    onUseOwnApp: (shown) => {
      assert(shown.oauth !== null && shown.oauth.own_app_allowed, "only a connector whose file offers an own app opens its entry");
      setFailure("");
      setDialog({ kind: "own_app", connector: shown.name, title: shown.title, redirect: shown.oauth.redirect, clientId: "" });
    },
    onClientId: (clientId) => { if (dialog.kind === "own_app") setDialog({ ...dialog, clientId }); },
    onSaveApp: () => { if (!busy) void saveApp(); },
    onRemoveApp: (shown) => { if (!busy) void changed(() => clearConnectorOwnApp(shown.name), ownAppRemovedLine(shown.title)); },
    onPickNative: (choice) => { setNative(choice); setProblem(""); },
    onPickSize: (choice) => { setSizeCap(choice); setProblem(""); },
    onSave: () => { if (!saving && !reading) void save(); },
    onRetry: reload,
  };

  return <div className="stack">
    <ConnectorsSettingsView view={view} reach={reach} open={connector ?? null} dialog={dialog} write={write} wait={wait} native={native} sizeCap={sizeCap} busy={busy} saving={saving || reading}
      signing={signing?.name ?? null} failure={failure} problem={problem} keyRef={keyRef} actions={actions} />
  </div>;
}
