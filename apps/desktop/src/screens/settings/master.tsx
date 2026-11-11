import { useEffect, useState, type FormEvent } from "react";
import { getMasterStepSettings, getMasterTimeSettings, saveMasterStepSettings, saveMasterTimeSettings, type MasterStepSettings, type MasterTimeSettings, type Result, type StepLimit, type TimeLimit } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, RadioCard, SettingLine, SettingSection, Skeleton } from "../../ui";
import "../project/projects.css";

/** The setting an owner has until they pick another: the engine's `MasterStepSettings` default. */
export const STEP_LIMIT_DEFAULT: MasterStepSettings = { limit: "no_limit" };

/** The step limit options in the order Settings lists them, each with its title and what it
 * does, the same words in the TUI. */
export const STEP_LIMIT_CHOICES: { choice: StepLimit; title: string; help: string }[] = [
  { choice: "no_limit", title: "No limit", help: "A turn runs until the master replies or its time is up." },
  { choice: "steps50", title: "50 steps", help: "A turn stops after 50 steps." },
  { choice: "steps100", title: "100 steps", help: "A turn stops after 100 steps." },
  { choice: "steps200", title: "200 steps", help: "A turn stops after 200 steps." },
];

/** What a step is and what a stopped turn says, the same words in the TUI. */
export const STEP_LIMIT_NOTE = "A step is one round of the master's work in a turn: it uses its tools, reads the results and decides what to do next. A turn that reaches the limit stops and says: Stopped after N steps; reply with 'continue' to go on. Its edits so far are kept. The limit covers a Claude master with tools on; a Codex master counts no steps.";

/** The setting an owner has until they pick another: the engine's `MasterTimeSettings` default. */
export const TIME_LIMIT_DEFAULT: MasterTimeSettings = { limit: "no_limit" };

/** The time limit options in the order Settings lists them, each with its title and what it
 * does, the same words in the TUI. */
export const TIME_LIMIT_CHOICES: { choice: TimeLimit; title: string; help: string }[] = [
  { choice: "no_limit", title: "No limit", help: "A turn runs until the master replies or you stop it." },
  { choice: "minutes10", title: "10 min", help: "A turn stops after 10 minutes." },
  { choice: "minutes30", title: "30 min", help: "A turn stops after 30 minutes." },
  { choice: "minutes60", title: "60 min", help: "A turn stops after 60 minutes." },
];

/** What a turn stopped by the time limit says and which masters the limit covers, the same
 * words in the TUI. */
export const TIME_LIMIT_NOTE = "A turn that reaches the limit stops and says: Stopped after N minutes; reply with 'continue' to go on. Its edits so far are kept. With no limit a turn runs until the master replies or you stop it. The limit covers Claude and Codex masters; an rcli master still stops after 10 minutes.";

const MASTER_TOP = <div className="settings-top"><div>
  <h2>Master</h2>
  <p>How far a project's master goes in one turn.</p>
</div></div>;

/** What one master limit shows and how it changes: the stored option, the picked one, a
 * saving flag, the last problem and the handlers. */
interface LimitState<T extends string> {
  settings: Loaded<{ limit: T }>;
  picked: T | null;
  saving: boolean;
  problem: string;
  onPick: (choice: T) => void;
  onSave: () => void;
  onRetry: () => void;
}

/** One master limit as a form of its own: its options as radio cards, marked when away from
 * the default, and Save changes, enabled only after a change. Inputs: the form's name, the
 * section title, the line label, the radio name, the options, the default, the note, the
 * stored and picked options, a saving flag, the last problem and the handlers. Output: the
 * form. */
function LimitForm<T extends string>({ form, title, line, name, choices, fallback, note, stored, picked, saving, problem, onPick, onSave }: {
  form: string;
  title: string;
  line: string;
  name: string;
  choices: { choice: T; title: string; help: string }[];
  fallback: T;
  note: string;
  stored: T;
  picked: T | null;
  saving: boolean;
  problem: string;
  onPick: (choice: T) => void;
  onSave: () => void;
}) {
  const limit = picked ?? stored;
  const changed = limit !== stored;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <form className="pj set-groups" aria-label={form} noValidate onSubmit={submit}>
    <SettingSection title={title}>
      <SettingLine wide label={line} changed={limit !== fallback} help={note}>
        <fieldset className="field" disabled={saving}>
          <legend className="visually-hidden">{line}</legend>
          {choices.map((option) => <RadioCard key={option.choice} name={name} value={option.choice} checked={limit === option.choice}
            onChecked={(value) => onPick(value as T)} title={option.title} details={[option.help]} />)}
        </fieldset>
      </SettingLine>
      <div className="set-body">
        {problem && <Notice tone="danger" alert>{problem}</Notice>}
        <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
      </div>
    </SettingSection>
  </form>;
}

/** Settings, Master as a view of its state: the title in every state, then the four step
 * limit options, marked when away from the default, and Save changes, enabled only after a
 * change. Inputs: the setting read, the picked option, a saving flag, the last problem and
 * the handlers. Output: the top of the tab and the step limit form. */
export function MasterSettingsView({ settings, picked, saving, problem, onPick, onSave, onRetry }: LimitState<StepLimit>) {
  if (settings.kind === "loading") return <>{MASTER_TOP}<Skeleton shape="line" count={4} /></>;
  if (settings.kind === "not_ready") return <>{MASTER_TOP}<NotReady what="The master setting" /></>;
  if (settings.kind === "error") return <>{MASTER_TOP}<LoadError message={`Couldn't load the master setting. ${settings.message}`} onRetry={onRetry} /></>;
  return <>
    {MASTER_TOP}
    <LimitForm form="Master" title="Master step limit" line="Most steps in one turn" name="step-limit" choices={STEP_LIMIT_CHOICES}
      fallback={STEP_LIMIT_DEFAULT.limit} note={STEP_LIMIT_NOTE} stored={settings.value.limit} picked={picked} saving={saving} problem={problem}
      onPick={onPick} onSave={onSave} />
  </>;
}

/** The master time limit of Settings, Master as a view of its state: the four time limit
 * options, marked when away from the default, and its own Save changes, enabled only after
 * a change. Inputs: the setting read, the picked option, a saving flag, the last problem and
 * the handlers. Output: the time limit form, or what its read is waiting on. */
export function MasterTimeView({ settings, picked, saving, problem, onPick, onSave, onRetry }: LimitState<TimeLimit>) {
  if (settings.kind === "loading") return <Skeleton shape="line" count={4} />;
  if (settings.kind === "not_ready") return <NotReady what="The master time limit" />;
  if (settings.kind === "error") return <LoadError message={`Couldn't load the master time limit. ${settings.message}`} onRetry={onRetry} />;
  return <LimitForm form="Master time limit" title="Master time limit" line="Longest one turn may run" name="time-limit" choices={TIME_LIMIT_CHOICES}
    fallback={TIME_LIMIT_DEFAULT.limit} note={TIME_LIMIT_NOTE} stored={settings.value.limit} picked={picked} saving={saving} problem={problem}
    onPick={onPick} onSave={onSave} />;
}

/** Reads one master limit and saves the picked option in the engine. Inputs: the engine read
 * and save of the setting and the note shown once it is saved. Output: the limit's state for
 * its view. */
function useLimit<T extends string>(read: () => Promise<Result<{ limit: T }>>, write: (settings: { limit: T }) => Promise<Result<void>>, saved: string): LimitState<T> {
  const session = useAppSession();
  const [settings, reload] = useLoad(read);
  const [picked, setPicked] = useState<T | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok") return;
    setSaving(true);
    setProblem("");
    const result = await write({ limit: picked ?? settings.value.limit });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast(saved);
    reload();
  }
  return { settings, picked, saving, problem, onPick: (choice) => { setPicked(choice); setProblem(""); }, onSave: () => void save(), onRetry: reload };
}

/** Settings, Master: reads the owner's master step limit and master time limit and saves the
 * picked option of each in the engine, each with its own Save changes. */
export function MasterSettings() {
  const steps = useLimit<StepLimit>(getMasterStepSettings, saveMasterStepSettings, "Master step limit saved.");
  const time = useLimit<TimeLimit>(getMasterTimeSettings, saveMasterTimeSettings, "Master time limit saved.");
  return <div className="stack">
    <MasterSettingsView {...steps} />
    <MasterTimeView {...time} />
  </div>;
}
