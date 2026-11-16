import { Fragment, useEffect, useState, type FormEvent } from "react";
import { FREEZE_APPS_MAX, KEEP_AWAKE_CHANGED, KEEP_AWAKE_DEFAULTS, KEEP_AWAKE_POLL_MS, KEEP_AWAKE_SETTINGS, loadKeepAwake, loadKeepAwakeApps, saveKeepAwake, sendKeepAwake, type FreezeApp, type KeepAwakeSettingId, type KeepAwakeSettings as Stored, type KeepAwakeView, type LidAction } from "../../api-keep-awake";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, SettingLine, SettingSection, Skeleton } from "../../ui";
import { lidHelperLine, lowPowerNow, lowPowerTurnOn, unavailable } from "../keep-awake/model";
import { LidHelperButtons } from "../keep-awake/panel";
import "../keep-awake/keep-awake.css";
import "../notifications/notifications.css";
import "../project/projects.css";

/** The options picked on the page and not saved yet, by setting. */
export type KeepAwakePicks = Partial<Record<KeepAwakeSettingId, string>>;

const TOP = <div className="settings-top"><div>
  <h2>Keep awake</h2>
  <p>When the computer stays awake for your agents, and when that ends.</p>
</div></div>;

const SECTIONS: readonly { title: string; about: string; ids: readonly KeepAwakeSettingId[] }[] = [
  { title: "When it holds", about: "What keeps the computer awake and for how long.", ids: ["mode", "grace", "cap"] },
  { title: "Lid closed", about: "Whether a hold goes on with the lid closed.", ids: ["lid", "lid_display"] },
  { title: "Ends by itself", about: "The limits that end a hold to protect the battery and the computer.", ids: ["battery_floor", "heat_end", "heat_notice"] },
  { title: "Display", about: "What a hold does with the display.", ids: ["display_awake"] },
  { title: "Power", about: "What a hold does to save the battery.", ids: ["low_power"] },
  { title: "While the lid is closed", about: "What a hold quiets while the lid is closed.", ids: ["freeze", "mute"] },
];

const RUNNING_MAX = 512;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The settings a save sends: every setting, the stored value or the pick. A pick names a
 * real setting and one of its options, or the page is broken. Inputs: the stored settings
 * and the picks. Output: the settings in engine order. */
export function savedKeepAwake(stored: Stored, picks: KeepAwakePicks): Stored {
  for (const id of Object.keys(picks)) assert(KEEP_AWAKE_SETTINGS.some((setting) => setting.id === id), `Keep awake has no setting ${id}`);
  const sent: Record<string, string> = {};
  for (const setting of KEEP_AWAKE_SETTINGS) {
    const value = picks[setting.id] ?? stored[setting.id];
    assert(setting.options.some((option) => option.value === value), `${setting.id} cannot be ${value}`);
    sent[setting.id] = value;
  }
  assert(Object.keys(sent).length === Object.keys(KEEP_AWAKE_DEFAULTS).length, "a save lists every setting once");
  return sent as unknown as Stored;
}

/** One setting's line: its label with the changed mark, its help and, when this computer
 * cannot honour it, the reason, then its options as segments, each a real radio. Low Power
 * Mode also says where it stands for this hold and, while it needs the lid helper, offers
 * the lid helper's own Turn on (DEC-167 addendum 42). Inputs: the setting's id, the view,
 * the shown option, a lock flag and the pick and lid command handlers. Output: the line. */
function OptionLine({ id, view, value, saving, onPick, onLid }: { id: KeepAwakeSettingId; view: KeepAwakeView; value: string; saving: boolean; onPick: (id: KeepAwakeSettingId, value: string) => void; onLid: (action: LidAction) => void }) {
  const setting = KEEP_AWAKE_SETTINGS.find((entry) => entry.id === id);
  assert(setting !== undefined, `Keep awake has no setting ${id}`);
  assert(setting.options.some((option) => option.value === value), `${id} has no option ${value}`);
  const reason = unavailable(id, view);
  const now = id === "low_power" ? lowPowerNow(view) : null;
  const turnOn = id === "low_power" ? lowPowerTurnOn(view) : null;
  const help = <>{setting.help}{reason !== null && <span className="keep-awake-reason">{reason}</span>}{now !== null && <span className="keep-awake-now">{now}</span>}</>;
  return <SettingLine label={setting.label} help={help} changed={value !== KEEP_AWAKE_DEFAULTS[id]}>
    <fieldset className="notice-choices" data-setting={id} disabled={saving}>
      <legend className="visually-hidden">{setting.label}</legend>
      {setting.options.map((option) => <label key={option.value} className={option.value === value ? "on" : undefined}>
        <input type="radio" name={`keep-awake-${id}`} value={option.value} checked={option.value === value} onChange={() => onPick(id, option.value)} />
        {option.label}
      </label>)}
    </fieldset>
    {turnOn !== null && <Button disabled={saving} onClick={() => onLid("turn_on_lid_hold")}>{turnOn}</Button>}
  </SettingLine>;
}

/** The lid helper's line on macOS, under the lid-closed hold setting: where it stands and
 * its buttons, whatever the lid rule (QUESTION 36c). Inputs: the view, a lock flag and the
 * command handler. Output: the line, or null off macOS. */
function LidHelperLine({ view, saving, onLid }: { view: KeepAwakeView; saving: boolean; onLid: (action: LidAction) => void }) {
  if (view.platform !== "mac_os") return null;
  return <SettingLine label="Lid helper" help={lidHelperLine(view)}>
    <LidHelperButtons view={view} place="settings" busy={saving} onLid={onLid} />
  </SettingLine>;
}

const LID_CRASH_GAP = "If the engine stops by a crash during a hold, the lid stays at Do nothing until the engine starts again.";

/** The lid setting's line on Windows, under the lid-closed hold setting (DEC-167 addendum
 * 32): what a crash during a hold leaves, and Put the lid setting back now. Inputs: the
 * view, a lock flag and the command handler. Output: the line, or null off Windows. */
function LidSettingLine({ view, saving, onLid }: { view: KeepAwakeView; saving: boolean; onLid: (action: LidAction) => void }) {
  if (view.platform !== "windows") return null;
  return <SettingLine label="Lid setting" help={LID_CRASH_GAP}>
    <Button disabled={saving} onClick={() => onLid("put_lid_back")}>Put the lid setting back now</Button>
  </SettingLine>;
}

/** The apps the chooser offers: the listed apps first, then every running app not listed
 * yet, each id once. Inputs: the listed apps and the running ones. Output: the apps. */
export function offeredApps(listed: readonly FreezeApp[], running: readonly FreezeApp[]): FreezeApp[] {
  assert(listed.length <= FREEZE_APPS_MAX, "the engine lists at most 16 apps");
  const offered = [...listed];
  for (const app of running.slice(0, RUNNING_MAX)) if (!offered.some((known) => known.id === app.id)) offered.push(app);
  assert(offered.length <= FREEZE_APPS_MAX + RUNNING_MAX, "the chooser stays bounded");
  return offered;
}

/** The chooser of apps to freeze: the listed apps ticked, then the running ones, one
 * checkbox each, at most 16 ticked, with Save apps and Cancel. Inputs: the listed apps,
 * the running ones, a lock flag and the handlers. Output: the chooser. */
function FreezeChooser({ listed, running, saving, onApps, onClose }: { listed: readonly FreezeApp[]; running: readonly FreezeApp[]; saving: boolean; onApps: (apps: FreezeApp[]) => void; onClose: () => void }) {
  const offered = offeredApps(listed, running);
  const [ticked, setTicked] = useState<readonly string[]>(listed.map((app) => app.id));
  const full = ticked.length >= FREEZE_APPS_MAX;
  return <div className="keep-awake-apps">
    <fieldset disabled={saving}>
      <legend className="visually-hidden">Apps to freeze</legend>
      {offered.length === 0 && <p>No running app can be frozen. Open the app first.</p>}
      {offered.map((app) => {
        const on = ticked.includes(app.id);
        return <label key={app.id}>
          <input type="checkbox" checked={on} disabled={!on && full} onChange={() => setTicked(on ? ticked.filter((id) => id !== app.id) : [...ticked, app.id])} />
          {app.name}
        </label>;
      })}
    </fieldset>
    {full && <p className="keep-awake-reason">Pick at most 16 apps, each once.</p>}
    <span>
      <Button variant="primary" pending={saving} pendingLabel="Saving..." onClick={() => onApps(offered.filter((app) => ticked.includes(app.id)))}>Save apps</Button>
      <Button variant="quiet" disabled={saving} onClick={onClose}>Cancel</Button>
    </span>
  </div>;
}

/** The apps line under the freeze setting: the listed apps by name and Choose apps, or
 * while choosing the running apps as they load. Hidden where apps cannot be frozen.
 * Inputs: the view, the running apps while choosing, a lock flag and the handlers.
 * Output: the line, or null. */
function FreezeAppsLine({ view, running, saving, onChoose, onApps, onClose }: { view: KeepAwakeView; running: Loaded<FreezeApp[]> | null; saving: boolean; onChoose: () => void; onApps: (apps: FreezeApp[]) => void; onClose: () => void }) {
  if (unavailable("freeze", view) !== null) return null;
  const listed = view.freeze_apps;
  const names = listed.length === 0 ? "None listed." : listed.map((app) => app.name).join(", ");
  return <SettingLine label="Apps to freeze" help={names}>
    {running === null && <Button disabled={saving} onClick={onChoose}>Choose apps</Button>}
    {running?.kind === "loading" && <span>Listing the running apps...</span>}
    {running?.kind === "error" && <span><Notice tone="danger" alert>{`Couldn't list the running apps. ${running.message}`}</Notice><Button variant="quiet" onClick={onClose}>Close</Button></span>}
    {running?.kind === "not_ready" && <span><Notice tone="danger" alert>{NOT_READY}</Notice><Button variant="quiet" onClick={onClose}>Close</Button></span>}
    {running?.kind === "ok" && <FreezeChooser listed={listed} running={running.value} saving={saving} onApps={onApps} onClose={onClose} />}
  </SettingLine>;
}

/** Settings, Keep awake as a view of its state: the title in every state, then every
 * setting with its options, the stored one picked, a changed mark away from the default,
 * the lid helper's line on macOS or the lid setting's line on Windows, the apps to freeze
 * under the freeze setting, and Save changes, enabled only after a pick that differs from
 * what is stored. Inputs: the view read, the picks, the running apps while choosing, a
 * saving flag, the last problem and the handlers. Output: the tab. */
export function KeepAwakeSettingsView({ view, picks, running = null, saving, problem, onPick, onSave, onLid, onChoose = () => undefined, onApps = () => undefined, onClose = () => undefined, onRetry }: {
  view: Loaded<KeepAwakeView>;
  picks: KeepAwakePicks;
  running?: Loaded<FreezeApp[]> | null;
  saving: boolean;
  problem: string;
  onPick: (id: KeepAwakeSettingId, value: string) => void;
  onSave: () => void;
  onLid: (action: LidAction) => void;
  onChoose?: () => void;
  onApps?: (apps: FreezeApp[]) => void;
  onClose?: () => void;
  onRetry: () => void;
}) {
  if (view.kind === "loading") return <>{TOP}<Skeleton shape="line" count={11} /></>;
  if (view.kind === "not_ready") return <>{TOP}<NotReady what="Keep awake" /></>;
  if (view.kind === "error") return <>{TOP}<LoadError message={`Couldn't load the Keep awake settings. ${view.message}`} onRetry={onRetry} /></>;
  const stored = view.value.settings;
  const sent = savedKeepAwake(stored, picks);
  const changed = KEEP_AWAKE_SETTINGS.some((setting) => sent[setting.id] !== stored[setting.id]);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {TOP}
    <form className="pj set-groups" aria-label="Keep awake" noValidate onSubmit={submit}>
      {SECTIONS.map((section) => <SettingSection key={section.title} title={section.title} about={section.about}>
        {section.ids.map((id) => <Fragment key={id}>
          <OptionLine id={id} view={view.value} value={sent[id]} saving={saving} onPick={onPick} onLid={onLid} />
          {id === "lid" && <LidHelperLine view={view.value} saving={saving} onLid={onLid} />}
          {id === "lid" && <LidSettingLine view={view.value} saving={saving} onLid={onLid} />}
          {id === "freeze" && <FreezeAppsLine view={view.value} running={running} saving={saving} onChoose={onChoose} onApps={onApps} onClose={onClose} />}
        </Fragment>)}
      </SettingSection>)}
      <div className="set-body">
        {problem && <Notice tone="danger" alert>{problem}</Notice>}
        <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
      </div>
    </form>
  </>;
}

const NOT_READY = "The engine is not ready yet. Try again.";

/** Settings, Keep awake: reads the owner's Keep awake settings, reads them again whenever
 * Keep awake changes in this window and every 5 s, so a lid helper command shows what it
 * did once the engine ran it, saves the picked options over the settings the engine holds
 * at that moment, so a change made in the panel or by another client while the page was
 * open is kept, laying them again when another window saved first, and lists the running
 * apps to choose the apps to freeze. The picks stay until a save took them. */
export function KeepAwakeSettings() {
  const session = useAppSession();
  const [view, reload] = useLoad(loadKeepAwake);
  const [picks, setPicks] = useState<KeepAwakePicks>({});
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  const [running, setRunning] = useState<Loaded<FreezeApp[]> | null>(null);
  useEffect(() => {
    window.addEventListener(KEEP_AWAKE_CHANGED, reload);
    return () => window.removeEventListener(KEEP_AWAKE_CHANGED, reload);
  }, [reload]);
  useEffect(() => {
    const timer = window.setInterval(reload, KEEP_AWAKE_POLL_MS);
    return () => window.clearInterval(timer);
  }, [reload]);
  async function lid(action: LidAction) {
    if (view.kind !== "ok" || saving) return;
    setSaving(true);
    setProblem("");
    const result = await sendKeepAwake(action);
    setSaving(false);
    if (result.kind === "error") setProblem(result.message);
    if (result.kind === "not_ready") setProblem(NOT_READY);
  }
  async function choose() {
    if (view.kind !== "ok" || saving || running !== null) return;
    setRunning({ kind: "loading" });
    setRunning(await loadKeepAwakeApps());
  }
  async function apps(chosen: FreezeApp[]) {
    if (view.kind !== "ok" || saving) return;
    setSaving(true);
    setProblem("");
    const result = await sendKeepAwake({ freeze_apps: { apps: chosen } });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem(NOT_READY); return; }
    setRunning(null);
    session.showToast("Apps to freeze saved.");
  }
  async function save() {
    if (view.kind !== "ok" || saving) return;
    setSaving(true);
    setProblem("");
    const result = await saveKeepAwake((stored) => savedKeepAwake(stored, picks));
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem(NOT_READY); return; }
    setPicks({});
    session.showToast("Keep awake settings saved.");
  }
  return <div className="stack">
    <KeepAwakeSettingsView view={view} picks={picks} running={running} saving={saving} problem={problem}
      onPick={(id, value) => { setPicks((before) => ({ ...before, [id]: value })); setProblem(""); }} onSave={() => void save()}
      onLid={(action) => void lid(action)} onChoose={() => void choose()} onApps={(chosen) => void apps(chosen)}
      onClose={() => setRunning(null)} onRetry={reload} />
  </div>;
}
