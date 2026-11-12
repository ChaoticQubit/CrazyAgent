import { useEffect, useState } from "react";
import type { Result } from "../../api";
import {
  EFFECT_LINE, HARNESS_DEFAULT, NO_CATALOG_LINE, STALE_LINE, canReset, draftChanged, draftOf, draftValue, harnessSettingsRefusal,
  isStale, loadHarnessSettings, notReady, resetHarnessSetting, resetLabel, setHarnessSetting, settingGroups, shownValue,
  sourceLine, storedLine, versionLine,
} from "../../api-harness-settings";
import type { HarnessSetting, HarnessSettingsPage, SettingDraft, SettingScope } from "../../api-harness-settings";
import { useLoad, type Loaded } from "../../state";
import { Button, Icon, LoadError, Notice, NotReady, SearchField, Select, SettingSection, Skeleton, Switch } from "../../ui";
import { SettingRow } from "../../ui/setting";
import { displayText } from "../conversation";
import "./harness-page.css";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** What one row needs from its page: the scope, the row's draft (null for a key the owner
 * can't change), whether it is saving, its note and the row actions. */
export interface HarnessSettingRowProps {
  setting: HarnessSetting;
  scope: SettingScope;
  draft: SettingDraft | null;
  busy: boolean;
  note: string;
  onDraft: (draft: SettingDraft) => void;
  onSave: () => void;
  onReset: () => void;
}

/** A DOM id for a key's row, one per key: every character other than a letter or digit is
 * written as its code. Input: the key. Output: the id. */
export function rowId(key: string): string {
  return `hs-${key.replace(/[^A-Za-z0-9]/g, (char) => `_${char.codePointAt(0)?.toString(16)}_`)}`;
}

function Control({ setting, id, draft, onDraft }: { setting: HarnessSetting; id: string; draft: SettingDraft; onDraft: (draft: SettingDraft) => void }) {
  const label = displayText(setting.label);
  const unset = setting.value === null;
  const harness = { value: "", label: HARNESS_DEFAULT };
  if (setting.type === "bool" && unset) {
    assert(draft === "" || typeof draft === "boolean", "a switch draft is a boolean or the harness default");
    const options = [harness, { value: "on", label: "On" }, { value: "off", label: "Off" }];
    const chosen = draft === "" ? "" : draft ? "on" : "off";
    return <Select id={`${id}-control`} label={label} hideLabel value={chosen} options={options} onValue={(value) => onDraft(value === "" ? "" : value === "on")} />;
  }
  if (setting.type === "bool") {
    assert(typeof draft === "boolean", "a switch draft is a boolean");
    return <Switch id={`${id}-control`} label={label} hideLabel checked={draft} onChecked={onDraft} />;
  }
  assert(typeof draft === "string", "a field draft is text");
  if (setting.type === "enum") {
    const listed = setting.options.map((option) => ({ value: option, label: displayText(option) }));
    const options = unset ? [harness, ...listed] : listed;
    return <Select id={`${id}-control`} label={label} hideLabel value={draft} options={options} onValue={onDraft} />;
  }
  return <input id={`${id}-control`} className="input hs-input" aria-labelledby={`${id}-label`} aria-describedby={setting.help ? `${id}-help` : undefined}
    inputMode={setting.type === "integer" ? "numeric" : undefined} placeholder={unset ? HARNESS_DEFAULT : undefined} value={draft}
    onChange={(event) => onDraft(event.target.value)} />;
}

function ReadOnly({ setting, id, reason, scope, busy, note, onReset }: { setting: HarnessSetting; id: string; reason: string; scope: SettingScope; busy: boolean; note: string; onReset: () => void }) {
  const locked = setting.access.kind === "locked";
  assert(!locked || setting.source === "engine", "a locked key shows the engine's value");
  const stored = storedLine(setting);
  return <div className="set-ctl hs-control">
    <span className="hs-value">{locked && <Icon name="lock" size={14} label="Locked" />}<span className="mono">{shownValue(setting)}</span></span>
    <p id={`${id}-reason`} className="helper">{displayText(reason)}</p>
    {stored && <p className="helper hs-stored">{stored}</p>}
    {canReset(setting) && <span className="row"><Button variant="quiet" size="small" pending={busy} pendingLabel="Clearing..." onClick={onReset}>{resetLabel(setting, scope)}</Button></span>}
    {note && <p className="helper hs-note" role="status">{note}</p>}
  </div>;
}

/** One setting as a Settings row: its label, then its key and help under it, and at the right a
 * switch, select or field with Save and the scope's reset when the owner can change it, or the
 * value with the engine's reason (and a lock for a key CrazyAgent sets itself) when they
 * can't. A saved value that is not in force says so, with the reset that clears it. Input:
 * the row props. Output: the row. */
export function HarnessSettingRow({ setting, scope, draft, busy, note, onDraft, onSave, onReset }: HarnessSettingRowProps) {
  const id = rowId(setting.key);
  const access = setting.access;
  assert((access.kind === "editable") === (draft !== null), "only an editable key has a draft");
  const stored = storedLine(setting);
  const label = displayText(setting.label);
  return <SettingRow label={label}>
    <span id={`${id}-label`} className="set-label">{label}</span>
    <span className="set-help">
      <span className="mono">{displayText(setting.key)}</span>
      {setting.help && <span id={`${id}-help`}>{displayText(setting.help)}</span>}
    </span>
    {access.kind !== "editable" || draft === null ? <ReadOnly setting={setting} id={id} reason={access.kind === "editable" ? "" : access.reason} scope={scope} busy={busy} note={note} onReset={onReset} />
      : <div className="set-ctl hs-control">
        <Control setting={setting} id={id} draft={draft} onDraft={onDraft} />
        <span className="small muted">{sourceLine(setting, scope)}</span>
        {stored && <p className="helper hs-stored">{stored}</p>}
        <span className="row">
          {draftChanged(setting, draft) && <Button variant="primary" size="small" pending={busy} pendingLabel="Saving..." onClick={onSave}>Save</Button>}
          {canReset(setting) && <Button variant="quiet" size="small" disabled={busy} onClick={onReset}>{resetLabel(setting, scope)}</Button>}
        </span>
        {note && <p className="helper hs-note" role="status">{note}</p>}
      </div>}
  </SettingRow>;
}

/** Everything the page view shows and the actions it offers, without engine calls. */
export interface HarnessSettingsViewProps {
  page: Loaded<HarnessSettingsPage>;
  scope: SettingScope;
  query: string;
  drafts: Readonly<Record<string, SettingDraft>>;
  busy: string | null;
  notes: Readonly<Record<string, string>>;
  onQuery: (query: string) => void;
  onDraft: (key: string, draft: SettingDraft) => void;
  onSave: (setting: HarnessSetting) => void;
  onReset: (setting: HarnessSetting) => void;
  onRetry: () => void;
}

/** One harness's settings as a pure view of the page read: the version line, when changes
 * apply, a filter, then the editable, locked and read-only groups, each a Settings section
 * with its count. A harness with no settings list says so. Input: the view props. Output: the
 * page body. */
export function HarnessSettingsView(props: HarnessSettingsViewProps) {
  const { page, scope, query, drafts, busy, notes } = props;
  if (page.kind === "loading") return <Skeleton shape="row" count={6} />;
  if (page.kind === "not_ready" || notReady(page)) return <NotReady what="This page" compact />;
  if (page.kind === "error" && page.code === "harness_settings_no_catalog") return <Notice tone="info">{NO_CATALOG_LINE}</Notice>;
  if (page.kind === "error") return <LoadError message={harnessSettingsRefusal(page)} onRetry={props.onRetry} />;
  const value = page.value;
  assert((scope === "project") === (value.project_id !== null), "a project page names its project");
  const groups = settingGroups(value.keys, query);
  return <div className="stack">
    <p className="secondary">{versionLine(value)}</p>
    <p className="small muted">{EFFECT_LINE}{scope === "project" ? " A value set here wins over the app value for this project only." : ""}</p>
    <SearchField label="Filter settings" value={query} onValue={props.onQuery} />
    {groups.length === 0 && <p className="muted">{value.keys.length === 0 ? "This harness lists no settings." : "No setting matches the filter."}</p>}
    {groups.length > 0 && <div className="pj set-groups">{groups.map((group) => <SettingSection key={group.kind} title={`${group.title} (${group.keys.length})`}>
      {group.keys.map((setting) => <HarnessSettingRow key={setting.key} setting={setting} scope={scope}
        draft={setting.access.kind === "editable" ? drafts[setting.key] ?? draftOf(setting) : null} busy={busy === setting.key} note={notes[setting.key] ?? ""}
        onDraft={(draft) => props.onDraft(setting.key, draft)} onSave={() => props.onSave(setting)} onReset={() => props.onReset(setting)} />)}
    </SettingSection>)}</div>}
  </div>;
}

/** The note a row shows after a set or reset that did not store. Input: the result. Output:
 * the plain line, or "" once stored. */
export function changeNote(result: Result<void>): string {
  if (result.kind === "ok") return "";
  if (result.kind === "not_ready" || notReady(result)) return "Not ready yet: changing harness settings isn't in this build.";
  if (isStale(result)) return STALE_LINE;
  return harnessSettingsRefusal(result);
}

/** One harness's settings page, app-wide or for one project: reads the page, keeps each row's
 * draft, and sends a set or reset with the version it read; after a stale refusal it reads the
 * page again. Inputs: the harness id and the project id, or null for the app. */
export function HarnessPage({ harness, projectId }: { harness: string; projectId: string | null }) {
  const [page, reload] = useLoad(() => loadHarnessSettings(harness, projectId), [harness, projectId]);
  const [drafts, setDrafts] = useState<Record<string, SettingDraft>>({});
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  useEffect(() => { setDrafts({}); setNotes({}); setQuery(""); }, [harness, projectId]);

  function settle(key: string, result: Result<void>) {
    setBusy(null);
    setNotes((old) => ({ ...old, [key]: changeNote(result) }));
    if (result.kind !== "ok" && !isStale(result)) return;
    setDrafts((old) => Object.fromEntries(Object.entries(old).filter(([name]) => name !== key)));
    reload();
  }

  async function save(setting: HarnessSetting) {
    const parsed = draftValue(setting, drafts[setting.key] ?? draftOf(setting));
    if ("error" in parsed) return setNotes((old) => ({ ...old, [setting.key]: parsed.error }));
    setBusy(setting.key);
    settle(setting.key, await setHarnessSetting(harness, projectId, setting.key, parsed.value, setting.version));
  }

  async function reset(setting: HarnessSetting) {
    setBusy(setting.key);
    settle(setting.key, await resetHarnessSetting(harness, projectId, setting.key, setting.version));
  }

  return <HarnessSettingsView page={page} scope={projectId === null ? "app" : "project"} query={query} drafts={drafts} busy={busy} notes={notes}
    onQuery={setQuery} onDraft={(key, draft) => setDrafts((old) => ({ ...old, [key]: draft }))}
    onSave={(setting) => void save(setting)} onReset={(setting) => void reset(setting)} onRetry={reload} />;
}
