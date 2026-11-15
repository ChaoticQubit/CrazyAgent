import { useEffect, useState, type FormEvent } from "react";
import {
  KEEP_CHOICES, KIND_WORDS, loadConnectorActivity, saveConnectorSettings, sizeWords, stampWords,
  type ActivityView, type CallView, type KeepLog,
} from "../../api-connectors";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, Select, SettingSection, Skeleton, type SelectOption } from "../../ui";
import "../notifications/notifications.css";
import "../project/projects.css";

/** The page's title, the words the TUI page uses too. */
export const ACTIVITY_TITLE = "Connector activity";
/** What the page shows, the same in the TUI. */
export const ACTIVITY_PURPOSE = "Every tool list and tool call an agent made through a connector. Arguments and results are never kept.";
const KEEP_LABEL = "Activity log keeps";
const DEFAULT_KEEP: KeepLog = "days30";
const ALL = "All";
const ALL_VALUE = "";
const SAVED = "Connector settings saved.";
const STALE = "The connector settings changed elsewhere. Check them and try again.";
const NOT_SAVED = "The connector settings were not saved.";

const TOP = <div className="settings-top"><div>
  <h2>{ACTIVITY_TITLE}</h2>
  <p>{ACTIVITY_PURPOSE}</p>
</div></div>;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** A filter's options: All, then each choice, then the current filter when the log no longer
 * offers it, so the select never shows All while a filter is on. Inputs: the choices and the
 * current filter. Output: the options. */
function filterOptions(choices: SelectOption[], current: string | null): SelectOption[] {
  const options = [{ value: ALL_VALUE, label: ALL }, ...choices];
  if (current !== null && !choices.some((option) => option.value === current)) options.push({ value: current, label: current });
  return options;
}

/** One logged call as a list row: its time, connector, tool, kind and outcome, then the
 * project, agent, sizes and duration. Only these named fields are read. Input: the call.
 * Output: the row. */
function CallRow({ call }: { call: CallView }) {
  const head = [stampWords(call.started_ms), call.connector, ...(call.tool ? [call.tool] : []), KIND_WORDS[call.kind], call.outcome_text];
  return <li data-outcome={call.outcome}>
    <div>{head.join("  ")}</div>
    <div className="set-help">{`Project: ${call.project_name}  Agent: ${call.agent}  ${sizeWords(call.bytes_in, call.bytes_out)}  ${call.ms} ms`}</div>
  </li>;
}

/** Settings, Connector activity as a view of its state: the title in every state, the keep
 * choice with Save changes, the connector and project filters, then the newest calls.
 * Inputs: the view read, the picked keep time, the filters, a saving flag, the last problem
 * and the handlers. Output: the tab. */
export function ConnectorActivityView({ activity, keep, connector, project, saving, problem, onPickKeep, onFilter, onSave, onRetry }: {
  activity: Loaded<ActivityView>;
  keep: KeepLog | null;
  connector: string | null;
  project: string | null;
  saving: boolean;
  problem: string;
  onPickKeep: (keep: KeepLog) => void;
  onFilter: (connector: string | null, project: string | null) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (activity.kind === "loading") return <>{TOP}<Skeleton shape="line" count={6} /></>;
  if (activity.kind === "not_ready") return <>{TOP}<NotReady what={ACTIVITY_TITLE} /></>;
  if (activity.kind === "error") return <>{TOP}<LoadError message={`Couldn't load the connector activity. ${activity.message}`} onRetry={onRetry} /></>;
  const view = activity.value;
  const shownKeep = keep ?? view.settings.keep_log;
  assert(KEEP_CHOICES.some((option) => option.choice === shownKeep), `no keep choice ${shownKeep}`);
  const changed = shownKeep !== view.settings.keep_log;
  const filtered = view.connector !== null || view.project !== null;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  const connectors = filterOptions(view.connectors.map((name) => ({ value: name, label: name })), connector);
  const projects = filterOptions(view.projects.map((choice) => ({ value: choice.id, label: choice.name })), project);
  const picked = (value: string) => (value === ALL_VALUE ? null : value);
  return <>
    {TOP}
    <form className="pj set-groups" aria-label={ACTIVITY_TITLE} noValidate onSubmit={submit}>
      <SettingSection title={KEEP_LABEL} changed={shownKeep !== DEFAULT_KEEP}>
        <fieldset className="notice-choices" disabled={saving} data-setting="keep_log">
          <legend className="visually-hidden">{KEEP_LABEL}</legend>
          {KEEP_CHOICES.map((option) => <label key={option.choice} className={option.choice === shownKeep ? "on" : undefined}>
            <input type="radio" name="keep_log" value={option.choice} checked={option.choice === shownKeep} onChange={() => onPickKeep(option.choice)} />
            {option.label}
          </label>)}
        </fieldset>
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
      <SettingSection title={`Calls  ${view.calls.length}`}>
        <div className="pj-row">
          <Select id="connector-activity-connector" label="Connector" value={connector ?? ALL_VALUE} options={connectors}
            onValue={(value) => onFilter(picked(value), project)} />
          <Select id="connector-activity-project" label="Project" value={project ?? ALL_VALUE} options={projects}
            onValue={(value) => onFilter(connector, picked(value))} />
        </div>
        {view.calls.length === 0 && <p className="set-help">{filtered ? "No calls match these filters." : "No connector calls yet."}</p>}
        {view.more && <p className="set-help">Showing the newest calls. Filter by connector or project to see others.</p>}
        <ul className="set-body" aria-label="Calls">
          {view.calls.map((call, at) => <CallRow key={`${call.started_ms}-${at}`} call={call} />)}
        </ul>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Connector activity: reads the activity log with the picked filters and saves
 * the keep time with the rest of the connector settings at the version read. A save stays
 * locked until the page read after it lands, so a second save never sends an old version. */
export function ConnectorActivity() {
  const session = useAppSession();
  const [connector, setConnector] = useState<string | null>(null);
  const [project, setProject] = useState<string | null>(null);
  const [activity, reload] = useLoad(() => loadConnectorActivity(connector, project), [connector, project]);
  const [keep, setKeep] = useState<KeepLog | null>(null);
  const [saving, setSaving] = useState(false);
  const [reading, setReading] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setReading(false); }, [activity]);
  useEffect(() => { if (activity.kind === "ok" && keep === activity.value.settings.keep_log) setKeep(null); }, [activity, keep]);
  async function save() {
    if (activity.kind !== "ok" || keep === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveConnectorSettings({ ...activity.value.settings, keep_log: keep }, activity.value.version);
    setSaving(false);
    if (result.kind === "error") { setProblem(`${NOT_SAVED} ${result.message}`); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    setReading(true);
    if (result.value.kind === "stale") { setKeep(null); setProblem(STALE); reload(); return; }
    session.showToast(SAVED);
    reload();
  }
  return <div className="stack">
    <ConnectorActivityView activity={activity} keep={keep} connector={connector} project={project} saving={saving || reading} problem={problem}
      onPickKeep={(choice) => { setKeep(choice); setProblem(""); }}
      onFilter={(nextConnector, nextProject) => { setConnector(nextConnector); setProject(nextProject); }}
      onSave={() => void save()} onRetry={reload} />
  </div>;
}
