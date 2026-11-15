import { useEffect, useState, type FormEvent } from "react";
import { KEEP_AWAKE_CHANGED, KEEP_AWAKE_DEFAULTS, KEEP_AWAKE_SETTINGS, loadKeepAwake, sendKeepAwake, type KeepAwakeSettingId, type KeepAwakeSettings as Stored, type KeepAwakeView } from "../../api-keep-awake";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, SettingLine, SettingSection, Skeleton } from "../../ui";
import { unavailable } from "../keep-awake/model";
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
];

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
 * cannot honour it, the reason, then its options as segments, each a real radio. Inputs:
 * the setting's id, the view, the shown option, a lock flag and the pick handler. Output:
 * the line. */
function OptionLine({ id, view, value, saving, onPick }: { id: KeepAwakeSettingId; view: KeepAwakeView; value: string; saving: boolean; onPick: (id: KeepAwakeSettingId, value: string) => void }) {
  const setting = KEEP_AWAKE_SETTINGS.find((entry) => entry.id === id);
  assert(setting !== undefined, `Keep awake has no setting ${id}`);
  assert(setting.options.some((option) => option.value === value), `${id} has no option ${value}`);
  const reason = unavailable(id, view);
  const help = <>{setting.help}{reason !== null && <span className="keep-awake-reason">{reason}</span>}</>;
  return <SettingLine label={setting.label} help={help} changed={value !== KEEP_AWAKE_DEFAULTS[id]}>
    <fieldset className="notice-choices" data-setting={id} disabled={saving}>
      <legend className="visually-hidden">{setting.label}</legend>
      {setting.options.map((option) => <label key={option.value} className={option.value === value ? "on" : undefined}>
        <input type="radio" name={`keep-awake-${id}`} value={option.value} checked={option.value === value} onChange={() => onPick(id, option.value)} />
        {option.label}
      </label>)}
    </fieldset>
  </SettingLine>;
}

/** Settings, Keep awake as a view of its state: the title in every state, then every
 * setting with its options, the stored one picked, a changed mark away from the default, and
 * Save changes, enabled only after a pick that differs from what is stored. Inputs: the view
 * read, the picks, a saving flag, the last problem and the handlers. Output: the tab. */
export function KeepAwakeSettingsView({ view, picks, saving, problem, onPick, onSave, onRetry }: {
  view: Loaded<KeepAwakeView>;
  picks: KeepAwakePicks;
  saving: boolean;
  problem: string;
  onPick: (id: KeepAwakeSettingId, value: string) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (view.kind === "loading") return <>{TOP}<Skeleton shape="line" count={9} /></>;
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
        {section.ids.map((id) => <OptionLine key={id} id={id} view={view.value} value={sent[id]} saving={saving} onPick={onPick} />)}
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
 * Keep awake changes in this window, and saves the picked options over the settings the
 * engine holds at that moment, so a change made in the panel or by another client while
 * the page was open is kept. The picks stay until a save took them. */
export function KeepAwakeSettings() {
  const session = useAppSession();
  const [view, reload] = useLoad(loadKeepAwake);
  const [picks, setPicks] = useState<KeepAwakePicks>({});
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => {
    window.addEventListener(KEEP_AWAKE_CHANGED, reload);
    return () => window.removeEventListener(KEEP_AWAKE_CHANGED, reload);
  }, [reload]);
  async function save() {
    if (view.kind !== "ok" || saving) return;
    setSaving(true);
    setProblem("");
    const fresh = await loadKeepAwake();
    if (fresh.kind !== "ok") {
      setSaving(false);
      setProblem(fresh.kind === "error" ? fresh.message : NOT_READY);
      return;
    }
    const result = await sendKeepAwake({ set: { settings: savedKeepAwake(fresh.value.settings, picks) } });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem(NOT_READY); return; }
    setPicks({});
    session.showToast("Keep awake settings saved.");
  }
  return <div className="stack">
    <KeepAwakeSettingsView view={view} picks={picks} saving={saving} problem={problem}
      onPick={(id, value) => { setPicks((before) => ({ ...before, [id]: value })); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
