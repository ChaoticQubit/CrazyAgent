import { useEffect, useRef, useState } from "react";
import { harnessCatalog, type HarnessAccountKind, type HarnessEntry, type Result } from "../../api";
import { openLink } from "../../api-projects";
import {
  harnessVersions, installedWith, installHarness, loadHarnessUpdateSettings, pollWhileRunning, readForConfirm, setHarnessUpdateSettings, updateCopy, updateHarness, versionLine,
  UPDATE_SETTINGS_DEFAULT, type CheckWhen, type HarnessJob, type HarnessVersions, type InstallFromApp, type OnlineLookup, type UpdateMode, type UpdateSettings,
} from "../../api-harness-updates";
import { useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, NotReady, ProviderMark, Segmented, SettingLine, SettingSection, Skeleton, StatusBadge } from "../../ui";
import { SettingRow } from "../../ui/setting";
import "../project/projects.css";

const KIND_LABEL: Record<HarnessAccountKind, string> = {
  subscription: "Subscription",
  api_key: "API key",
  open_ai_compatible_url: "URL",
  local: "Local",
};

const GROUPS: { kind: HarnessEntry["status"]["kind"]; title: string }[] = [
  { kind: "ready", title: "Ready" },
  { kind: "limited", title: "Limited control" },
  { kind: "not_installed", title: "Not installed" },
];

const MODES: { value: UpdateMode; label: string }[] = [
  { value: "check_and_update", label: "Check and update" },
  { value: "check_only", label: "Check only" },
  { value: "off", label: "Off" },
];
const WHENS: { value: CheckWhen; label: string }[] = [
  { value: "on_button", label: "Only when I choose Check for updates" },
  { value: "on_open", label: "Each time this page opens" },
];
const LOOKUPS: { value: OnlineLookup; label: string }[] = [
  { value: "on", label: "Look them up on the vendor's site" },
  { value: "off", label: "Don't look them up" },
];
const INSTALLS: { value: InstallFromApp; label: string }[] = [
  { value: "on", label: "Offer Install for harnesses that are not installed" },
  { value: "off", label: "Don't offer Install" },
];
const INSTALL_HELP = "Install runs Homebrew when it is on this Mac, else npm. It never uses sudo or a script from a website.";
const LOOKUP_HELP = "Claude Code's own installer and Codex's standalone installer have no check command, so CrazyAgent reads one small public file from downloads.claude.ai or releases.openai.com. It sends no account, project or file details. It never downloads or runs anything from there.";
const PURPOSE = "CrazyAgent finds harnesses installed on this Mac. It can install one with Homebrew or npm and update one with that harness's own updater, each only when you choose it.";

type Save = (settings: UpdateSettings) => Promise<Result<void>>;

/** Shows a path with the home folder as "~". Input: an absolute path. Output: the display path. */
export function homePath(path: string): string {
  return path.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
}

/** Groups the harness catalog Ready, then Limited control, then Not installed, keeping catalog
 * order inside each group. Input: the catalog. Output: non-empty groups with titles. */
export function harnessGroups(entries: HarnessEntry[]): { title: string; entries: HarnessEntry[] }[] {
  return GROUPS.map((group) => ({ title: group.title, entries: entries.filter((entry) => entry.status.kind === group.kind) }))
    .filter((group) => group.entries.length > 0);
}

/** Saves a new online lookup choice and keeps the other settings as they are.
 * Inputs: the shown settings, the choice and the save call. Output: the save's result. */
export function chooseOnline(current: UpdateSettings, online: OnlineLookup, save: Save): Promise<Result<void>> {
  return save({ ...current, online });
}

/** Saves a new Install from the app choice and keeps the other settings as they are.
 * Inputs: the shown settings, the choice and the save call. Output: the save's result. */
export function chooseInstall(current: UpdateSettings, install: InstallFromApp, save: Save): Promise<Result<void>> {
  return save({ ...current, install });
}

function statusWord(entry: HarnessEntry): string {
  return entry.status.kind === "ready" ? "Ready" : entry.status.kind === "not_installed" ? "Not installed" : `Limited control: ${entry.status.reason}`;
}

function lastLine(versions: HarnessVersions, name: string): string | null {
  const last = versions.last;
  if (last === null) return null;
  if (last.ok && last.from === null) return last.to === null ? "It is installed now. New agents use it; no restart needed." : `Version ${last.to} is installed now. New agents use it; no restart needed.`;
  if (last.ok) return `Updated from ${last.from} to ${last.to ?? "an unknown version"}. New agents use it now; no restart needed.`;
  if (last.code === "harness_install_not_found") return `The install finished, but CrazyAgent can't find ${name}. Choose Check again.`;
  return updateCopy(last.code ?? "harness_update_failed");
}

function refusalLine(versions: HarnessVersions): string | null {
  if (versions.method.kind === "with_app") return updateCopy("harness_update_with_app");
  if (versions.refusal === null || versions.command !== null) return null;
  const line = updateCopy(versions.refusal);
  return versions.refusal === "harness_update_unknown_install" ? `${line} Update it with its own installer.` : line;
}

/** One harness line: what it is, its version, and Check for updates and Update when the settings
 * and the install allow them, or Install for one that is not on this Mac. Inputs: the catalog
 * entry, its versions (null until read), the update mode, a failure to show, whether a check
 * runs, and the actions. Output: the row. */
export function HarnessRowView({ entry, versions, mode, problem, checking = false, onCheck, onUpdate, onInstall, onGuide }: {
  entry: HarnessEntry; versions: HarnessVersions | null; mode: UpdateMode; problem?: string; checking?: boolean;
  onCheck: () => void; onUpdate: () => void; onInstall: () => void; onGuide: (url: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const missing = entry.status.kind === "not_installed";
  const shown = !missing && versions !== null && mode !== "off" ? versions : null;
  const absent = missing ? versions : null;
  const checkable = shown !== null && shown.method.kind !== "with_app" && shown.method.kind !== "unknown";
  const updatable = shown !== null && mode === "check_and_update" && shown.command !== null;
  const installable = absent !== null && absent.install !== null;
  const read = shown ?? absent;
  const running = read?.running === true;
  const refusal = shown !== null ? refusalLine(shown) : absent !== null && absent.install === null && absent.refusal !== null ? updateCopy(absent.refusal) : null;
  const last = read === null || running ? null : lastLine(read, entry.name);
  const guide = shown?.guide ?? (absent !== null && absent.install === null ? absent.guide : null);
  const said = missing || read?.last?.code?.startsWith("harness_install_") ? "What the installer said" : "What the updater said";
  return <>
    <SettingRow label={entry.name}>
      <button type="button" className="set-label" aria-expanded={open} onClick={() => setOpen(!open)}><ProviderMark provider={entry.id} name={entry.name} />{entry.name}</button>
      <span className="set-help">
        <span>Tools: {entry.tools.kind === "switchable" ? "Can turn on or off" : entry.tools.reason}</span>
        <span>Accounts it can use: {entry.account_kinds.map((kind) => KIND_LABEL[kind]).join(", ") || "None"}</span>
        {shown !== null && shown.method.kind !== "with_app" && <span>{versionLine(shown)}</span>}
        {refusal !== null && <span>{refusal}</span>}
        {running && <span role="status">{missing ? "Installing..." : "Updating..."}</span>}
        {last !== null && <span role="status">{last}</span>}
        {problem && <span className="pj-error" role="alert">{problem}</span>}
        {guide !== null && <span><a href={guide} onClick={(event) => { event.preventDefault(); onGuide(guide); }}>{missing ? "How to install it yourself" : "How to update it yourself"}</a></span>}
      </span>
      <div className="set-ctl set-line-ctl">
        {checkable && <Button aria-label={`Check ${entry.name} for updates`} disabled={running || checking} onClick={onCheck}>Check for updates</Button>}
        {updatable && <Button aria-label={`Update ${entry.name}`} disabled={running || checking} onClick={onUpdate}>Update</Button>}
        {installable && <Button aria-label={`Install ${entry.name}`} disabled={running || checking} onClick={onInstall}>Install</Button>}
        <StatusBadge word={statusWord(entry)} />
      </div>
    </SettingRow>
    {read?.last && !read.last.ok && read.last.output && !running && <details className="set-body">
      <summary>{said}</summary>
      <pre className="mono">{read.last.output}</pre>
    </details>}
    {open && <dl className="details-grid">
      <dt>Found at</dt><dd className="mono">{entry.executable ? homePath(entry.executable) : "Not found on this Mac"}</dd>
      <dt>Runs in the background with</dt><dd className="mono">{entry.headless}</dd>
      <dt>Model</dt><dd>{entry.model_selection ? "You can choose the model" : "Chosen inside the harness"}</dd>
    </dl>}
  </>;
}

/** Asks before an update: the exact line CrazyAgent runs, how the harness was installed and what
 * running agents do. Inputs: the versions with the command, the harness name, open, pending and
 * the two actions. Output: the dialog, focus starting on Cancel. */
export function UpdateConfirm({ versions, name, open, pending, onConfirm, onCancel }: {
  versions: HarnessVersions; name: string; open: boolean; pending: boolean; onConfirm: () => void; onCancel: () => void;
}) {
  return <Dialog open={open} title={`Update ${name}?`} onClose={onCancel} busy={pending}
    footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>Cancel</Button><Button variant="primary" pending={pending} onClick={onConfirm}>Update</Button></>}>
    <p>CrazyAgent runs exactly this line:</p>
    <pre className="mono">{versions.command}</pre>
    <p>{installedWith(versions.method)}</p>
    <p>When it finishes, new agents use the new version with no restart. Agents that are running keep their version until they start again.</p>
  </Dialog>;
}

/** Asks before an install: the exact line CrazyAgent runs, what it never does and when agents can
 * use the harness. Inputs: the versions with the install line, the harness name, open, pending and
 * the two actions. Output: the dialog, focus starting on Cancel. */
export function InstallConfirm({ versions, name, open, pending, onConfirm, onCancel }: {
  versions: HarnessVersions; name: string; open: boolean; pending: boolean; onConfirm: () => void; onCancel: () => void;
}) {
  return <Dialog open={open} title={`Install ${name}?`} onClose={onCancel} busy={pending}
    footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>Cancel</Button><Button variant="primary" pending={pending} onClick={onConfirm}>Install</Button></>}>
    <p>CrazyAgent runs exactly this line. It never uses sudo and never runs a script from a website.</p>
    <pre className="mono">{versions.install}</pre>
    <p>When it finishes, new agents can use {name} with no engine restart.</p>
  </Dialog>;
}

/** The Updates section: the four harness update and install settings, each saved as soon as it is chosen.
 * Inputs: the shown settings, a save failure to show and the save call. Output: the section. */
export function UpdateSettingsView({ settings, problem, save }: { settings: UpdateSettings; problem?: string; save: Save }) {
  return <SettingSection title="Updates">
    <SettingLine label="Harness updates" changed={settings.mode !== UPDATE_SETTINGS_DEFAULT.mode}>
      <Segmented label="Harness updates" value={settings.mode} options={MODES} onValue={(mode) => void save({ ...settings, mode })} />
    </SettingLine>
    <SettingLine wide label="Check for harness updates" changed={settings.when !== UPDATE_SETTINGS_DEFAULT.when}>
      <Segmented label="Check for harness updates" value={settings.when} options={WHENS} onValue={(when) => void save({ ...settings, when })} />
    </SettingLine>
    <SettingLine wide label="Newer versions of self-updating installs" help={LOOKUP_HELP} changed={settings.online !== UPDATE_SETTINGS_DEFAULT.online}>
      <Segmented label="Newer versions of self-updating installs" value={settings.online} options={LOOKUPS} onValue={(online) => void chooseOnline(settings, online, save)} />
    </SettingLine>
    <SettingLine wide label="Install from the app" help={INSTALL_HELP} changed={settings.install !== UPDATE_SETTINGS_DEFAULT.install}>
      <Segmented label="Install from the app" value={settings.install} options={INSTALLS} onValue={(install) => void chooseInstall(settings, install, save)} />
    </SettingLine>
    {problem && <div className="set-body"><p className="pj-error" role="alert">{problem}</p></div>}
  </SettingSection>;
}

function failureCopy(result: Exclude<Result<unknown>, { kind: "ok" }>): string | undefined {
  if (result.kind === "not_ready") return undefined;
  return result.code.startsWith("harness_update_") || result.code.startsWith("harness_install_") ? updateCopy(result.code) : result.message;
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function HarnessRow({ entry, settings, onCatalog }: { entry: HarnessEntry; settings: UpdateSettings | null; onCatalog: () => void }) {
  const [versions, setVersions] = useState<HarnessVersions | null>(null);
  const [problem, setProblem] = useState<string>();
  const [checking, setChecking] = useState(false);
  const [asking, setAsking] = useState(false);
  const [pending, setPending] = useState(false);
  const [adding, setAdding] = useState(false);
  const alive = useRef(true);
  const following = useRef(false);
  const mode = settings?.mode ?? "off";
  const look = settings?.when === "on_open";
  const known = settings !== null;
  const installed = entry.status.kind !== "not_installed";
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);
  const show = (read: Result<HarnessVersions>) => {
    if (!alive.current) return read;
    if (read.kind === "ok") { setVersions(read.value); setProblem(undefined); } else setProblem(failureCopy(read));
    return read;
  };
  const follow = async () => {
    if (following.current) return;
    following.current = true;
    await pollWhileRunning(entry.id, (id, again) => harnessVersions(id, again).then(show), wait, () => alive.current);
    following.current = false;
    if (alive.current) onCatalog();
  };
  const take = (read: Result<HarnessVersions>) => {
    show(read);
    if (alive.current && read.kind === "ok" && read.value.running) void follow();
  };
  useEffect(() => {
    if (!known || (installed && mode === "off")) return;
    let live = true;
    void harnessVersions(entry.id, installed && look).then((read) => { if (live) take(read); });
    return () => { live = false; };
  }, [entry.id, known, installed, mode, look]);
  const check = async () => {
    setChecking(true);
    take(await harnessVersions(entry.id, true));
    setChecking(false);
  };
  const ask = async (job: HarnessJob) => {
    setChecking(true);
    const fresh = await readForConfirm(entry.id, job, harnessVersions);
    setChecking(false);
    take(fresh.read);
    if (!alive.current || !fresh.open) return;
    if (job === "update") setAsking(true); else setAdding(true);
  };
  const start = async (job: HarnessJob) => {
    setPending(true);
    const started = await (job === "update" ? updateHarness(entry.id) : installHarness(entry.id));
    setPending(false);
    if (job === "update") setAsking(false); else setAdding(false);
    if (started.kind !== "ok") { setProblem(failureCopy(started)); return; }
    await follow();
  };
  return <>
    <HarnessRowView entry={entry} versions={versions} mode={mode} problem={problem} checking={checking}
      onCheck={() => void check()} onUpdate={() => void ask("update")} onInstall={() => void ask("install")} onGuide={(url) => void openLink(url)} />
    {versions?.command && <UpdateConfirm versions={versions} name={entry.name} open={asking} pending={pending}
      onConfirm={() => void start("update")} onCancel={() => setAsking(false)} />}
    {versions?.install && <InstallConfirm versions={versions} name={entry.name} open={adding} pending={pending}
      onConfirm={() => void start("install")} onCancel={() => setAdding(false)} />}
  </>;
}

function UpdateSettingsRows({ settings, onSaved }: { settings: UpdateSettings; onSaved: (settings: UpdateSettings) => void }) {
  const [problem, setProblem] = useState<string>();
  const save: Save = async (next) => {
    const saved = await setHarnessUpdateSettings(next);
    if (saved.kind === "ok") { setProblem(undefined); onSaved(next); } else setProblem(failureCopy(saved) ?? "Couldn't save the setting.");
    return saved;
  };
  return <UpdateSettingsView settings={settings} problem={problem} save={save} />;
}

/** The harness list as a pure view of the catalog read, with a Check again action, and the update
 * settings when the engine has them. Inputs: the catalog read, when it was checked, the recheck
 * action, the update settings (null when this engine has none), what to do once one is saved, and
 * a failed settings read to say with its retry. Output: the page. */
export function HarnessesView({ catalog, checkedAt, onCheck, settings = null, onSettings, settingsError }: {
  catalog: Loaded<HarnessEntry[]>; checkedAt?: string; onCheck: () => void; settings?: UpdateSettings | null; onSettings?: (settings: UpdateSettings) => void;
  settingsError?: { message: string; onRetry: () => void };
}) {
  return <div className="stack">
    <div className="settings-top">
      <div><h2>Harnesses</h2><p>{PURPOSE}</p></div>
      <span className="row">{checkedAt && <span className="muted small">Checked {checkedAt}</span>}
        <Button icon="refresh-cw" onClick={onCheck} disabled={catalog.kind === "loading"}>Check again</Button></span>
    </div>
    {settingsError && <LoadError message={settingsError.message} onRetry={settingsError.onRetry} />}
    {settings !== null && onSettings && <UpdateSettingsRows settings={settings} onSaved={onSettings} />}
    {catalog.kind === "loading" ? <Skeleton shape="row" count={8} />
      : catalog.kind === "not_ready" ? <NotReady what="The harness list" />
      : catalog.kind === "error" ? <LoadError message="Couldn't check harnesses." onRetry={onCheck} />
      : harnessGroups(catalog.value).map((group) => <SettingSection key={group.title} title={`${group.title} (${group.entries.length})`}>
        {group.entries.map((entry) => <HarnessRow key={entry.id} entry={entry} settings={settings} onCatalog={onCheck} />)}
      </SettingSection>)}
  </div>;
}

/** Settings, Harnesses: has the engine look for harnesses again on every visit and on Check
 * again, so a harness installed a moment ago shows at once, and reads the update settings. */
export function Harnesses() {
  const [catalog, reload] = useLoad(() => harnessCatalog(true), [], "harness-check");
  const [loaded, reloadSettings] = useLoad(loadHarnessUpdateSettings, []);
  const [settings, setSettings] = useState<UpdateSettings | null>(null);
  const [checkedAt, setCheckedAt] = useState<string>();
  useEffect(() => { if (loaded.kind === "ok") setSettings(loaded.value); }, [loaded]);
  const settingsError = loaded.kind === "error" ? { message: failureCopy(loaded) ?? updateCopy("harness_update_settings_unreadable"), onRetry: reloadSettings } : undefined;
  return <HarnessesView catalog={catalog} checkedAt={checkedAt ?? (catalog.kind === "ok" ? "just now" : undefined)} settings={settings} onSettings={setSettings} settingsError={settingsError}
    onCheck={() => { reload(); setCheckedAt(`at ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`); }} />;
}
