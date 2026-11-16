import { useEffect, useRef, useState, type FormEvent, type RefObject } from "react";
import type { Result } from "../../api";
import { pickAppPluginFile, problemText, type Problem } from "../../api-app-plugins";
import {
  CONNECTOR_DEFAULTS, CONNECTORS_PAGE, NATIVE_CHOICES, SIZE_CHOICES, addConnector, clearConnectorKey, connectorState, keyLine, loadConnectors,
  originLine, reachesLine, removeConnector, reviewConnector, previewConnector, saveConnectorSettings, setConnectorKey, settingsToSave, trustConnector, trustLines,
  type ConnectorPreview, type ConnectorView, type ConnectorsView, type ConnectorTrustView, type NativeConnectors, type SizeCap,
} from "../../api-connectors";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, Dialog, LoadError, NotReady, Notice, SecretField, SettingLine, SettingSection, Skeleton, StatusBadge, TextArea, formatRoute } from "../../ui";
import { ConnectorDetailView } from "./connector-detail";
import "../notifications/notifications.css";
import "../project/projects.css";

const NOT_READY = "The engine is not ready yet. Try again.";
const MANIFEST_CHARS_MAX = 16384;

/** What Settings, Connectors has open over the page: nothing, the paste box, a trust screen
 * (for a new manifest, or a review of a listed file), a refused manifest, the key entry for
 * one secret, or the remove confirm. */
export type ConnectorDialog =
  | { kind: "none" }
  | { kind: "add"; yaml: string }
  | { kind: "trust"; yaml: string | null; trust: ConnectorTrustView; review: boolean }
  | { kind: "refused"; yaml: string | null; problem: Problem }
  | { kind: "key"; connector: string; title: string; hosts: string[]; secret: string }
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

function CatalogRow({ connector }: { connector: ConnectorView }) {
  const state = connectorState(connector);
  return <li className="pj-task" data-connector={connector.name}>
    <div className="pj-row between">
      <div className="pj-stack-s">
        <a href={formatRoute({ name: "settings", tab: "connectors", connector: connector.name })}>{connector.title}</a>
        <span className="muted small">{originLine(connector)}</span>
      </div>
      <StatusBadge word={state.word} tone={state.tone} />
    </div>
    {state.detail && <p className="small">{state.detail}</p>}
    {connector.description && <p className="secondary">{connector.description}</p>}
    <p className="small">{reachesLine(connector.runs.hosts)}</p>
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
  if (dialog.kind === "remove") {
    return <ConfirmDialog open title={`Remove ${dialog.title}?`} confirmLabel="Remove" pending={busy} onConfirm={actions.onConfirmRemove} onCancel={actions.onClose}>
      {`${CONNECTORS_PAGE.removeBody} ${failure}`.trim()}
    </ConfirmDialog>;
  }
  return null;
}

/** The two connector settings this page owns as radio fieldsets, marked when away from the
 * default, the locked line with its reason, and Save changes, enabled only after a change.
 * Inputs: the page read, the picked values, a saving flag, the last problem and the
 * actions. Output: the form. */
function SettingsForm({ page, native, sizeCap, saving, problem, actions }: {
  page: ConnectorsView;
  native: NativeConnectors | null;
  sizeCap: SizeCap | null;
  saving: boolean;
  problem: string;
  actions: ConnectorActions;
}) {
  const shownNative = native ?? page.settings.native;
  const shownSize = sizeCap ?? page.settings.size_cap;
  assert(NATIVE_CHOICES.some((option) => option.choice === shownNative), `no native choice ${shownNative}`);
  assert(SIZE_CHOICES.some((option) => option.choice === shownSize), `no size cap choice ${shownSize}`);
  const changed = shownNative !== page.settings.native || shownSize !== page.settings.size_cap;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) actions.onSave();
  }
  return <form className="pj set-groups" aria-label={CONNECTORS_PAGE.settings} noValidate onSubmit={submit}>
    <SettingSection title={CONNECTORS_PAGE.settings}>
      <SettingLine wide label={CONNECTORS_PAGE.native} changed={shownNative !== CONNECTOR_DEFAULTS.native} help={CONNECTORS_PAGE.nativeHelp}>
        <fieldset className="notice-choices" disabled={saving} data-setting="native">
          <legend className="visually-hidden">{CONNECTORS_PAGE.native}</legend>
          {NATIVE_CHOICES.map((option) => <label key={option.choice} className={option.choice === shownNative ? "on" : undefined}>
            <input type="radio" name="connector-native" value={option.choice} checked={option.choice === shownNative} onChange={() => actions.onPickNative(option.choice)} />
            {option.label}
          </label>)}
        </fieldset>
      </SettingLine>
      <SettingLine wide label={CONNECTORS_PAGE.sizeCap} changed={shownSize !== CONNECTOR_DEFAULTS.size_cap} help={CONNECTORS_PAGE.sizeCapHelp}>
        <fieldset className="notice-choices" disabled={saving} data-setting="size_cap">
          <legend className="visually-hidden">{CONNECTORS_PAGE.sizeCap}</legend>
          {SIZE_CHOICES.map((option) => <label key={option.choice} className={option.choice === shownSize ? "on" : undefined}>
            <input type="radio" name="connector-size-cap" value={option.choice} checked={option.choice === shownSize} onChange={() => actions.onPickSize(option.choice)} />
            {option.label}
          </label>)}
        </fieldset>
      </SettingLine>
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
 * Inputs: the page read, the connector the route opened, the open dialog, the picked
 * settings, busy and saving flags, the last failure and save problem, the key field and
 * the actions. Output: the tab. */
export function ConnectorsSettingsView({ view, open, dialog, native, sizeCap, busy, saving, failure, problem, keyRef, actions }: {
  view: Loaded<ConnectorsView>;
  open: string | null;
  dialog: ConnectorDialog;
  native: NativeConnectors | null;
  sizeCap: SizeCap | null;
  busy: boolean;
  saving: boolean;
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
    return <>
      <ConnectorDetailView name={open} connector={connector} busy={busy} failure={dialog.kind === "none" ? failure : ""} actions={actions} />
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
          <ul className="pj-rows" aria-label={CONNECTORS_PAGE.catalog}>{page.connectors.map((connector) => <CatalogRow key={connector.name} connector={connector} />)}</ul>
          {page.more && <p className="muted small">{`Showing the first ${page.connectors.length} connectors.`}</p>}
        </div>
      </SettingSection>
    </div>
    <SettingsForm page={page} native={native} sizeCap={sizeCap} saving={saving} problem={problem} actions={actions} />
    {dialogs}
  </>;
}

/** Settings, Connectors: reads the page, adds a pasted or picked manifest only after its
 * trust screen is confirmed, reviews a changed file, removes the owner's own, sets and
 * clears each secret's key, and saves the two connector settings at the version read. A
 * change reads the page again, so the version a save names is always the last read.
 * Input: the connector whose page the route opened, if any. Output: the tab. */
export function ConnectorsSettings({ connector }: { connector?: string }) {
  const session = useAppSession();
  const [view, reload] = useLoad(() => loadConnectors(null));
  const [dialog, setDialog] = useState<ConnectorDialog>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [native, setNative] = useState<NativeConnectors | null>(null);
  const [sizeCap, setSizeCap] = useState<SizeCap | null>(null);
  const [saving, setSaving] = useState(false);
  const [reading, setReading] = useState(false);
  const [problem, setProblem] = useState("");
  const keyRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => { setReading(false); }, [view]);
  useEffect(() => {
    if (view.kind !== "ok") return;
    if (native === view.value.settings.native) setNative(null);
    if (sizeCap === view.value.settings.size_cap) setSizeCap(null);
  }, [view, native, sizeCap]);

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

  async function changed(work: () => Promise<Result<ConnectorsView>>, words: string) {
    const saved = await attempt(work);
    if (saved === undefined) return;
    setDialog({ kind: "none" });
    session.showToast(words);
    reload();
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

  async function save() {
    if (view.kind !== "ok" || (native === null && sizeCap === null)) return;
    const read = view.value.settings;
    setSaving(true);
    setProblem("");
    const result = await saveConnectorSettings(settingsToSave(read, native ?? read.native, sizeCap ?? read.size_cap), view.value.version);
    setSaving(false);
    if (result.kind === "error") { setProblem(`${CONNECTORS_PAGE.notSaved} ${result.message}`); return; }
    if (result.kind === "not_ready") { setProblem(NOT_READY); return; }
    setReading(true);
    if (result.value.kind === "stale") { setNative(null); setSizeCap(null); setProblem(CONNECTORS_PAGE.stale); reload(); return; }
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
    onConfirmRemove: () => { if (dialog.kind === "remove" && !busy) void changed(() => removeConnector(dialog.connector), `${dialog.title} was removed.`); },
    onSetKey: (shown, secret) => { setFailure(""); setDialog({ kind: "key", connector: shown.name, title: shown.title, hosts: shown.runs.hosts, secret }); },
    onSaveKey: () => { if (!busy) void saveKey(); },
    onClearKey: (name, secret) => { if (!busy) void changed(() => clearConnectorKey(name, secret), `${secret} no longer has a key.`); },
    onPickNative: (choice) => { setNative(choice); setProblem(""); },
    onPickSize: (choice) => { setSizeCap(choice); setProblem(""); },
    onSave: () => { if (!saving && !reading) void save(); },
    onRetry: reload,
  };

  return <div className="stack">
    <ConnectorsSettingsView view={view} open={connector ?? null} dialog={dialog} native={native} sizeCap={sizeCap} busy={busy} saving={saving || reading}
      failure={failure} problem={problem} keyRef={keyRef} actions={actions} />
  </div>;
}
