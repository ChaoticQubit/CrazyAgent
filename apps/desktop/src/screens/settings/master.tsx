import { useEffect, useState, type FormEvent } from "react";
import { getMasterStepSettings, saveMasterStepSettings, type MasterStepSettings, type StepLimit } from "../../api";
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

const MASTER_TOP = <div className="settings-top"><div>
  <h2>Master</h2>
  <p>How far a project's master goes in one turn.</p>
</div></div>;

/** Settings, Master as a view of its state: the title in every state, then the four step
 * limit options, marked when away from the default, and Save changes, enabled only after a
 * change. Inputs: the setting read, the picked option, a saving flag, the last problem and
 * the handlers. Output: the tab. */
export function MasterSettingsView({ settings, picked, saving, problem, onPick, onSave, onRetry }: {
  settings: Loaded<MasterStepSettings>;
  picked: StepLimit | null;
  saving: boolean;
  problem: string;
  onPick: (choice: StepLimit) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <>{MASTER_TOP}<Skeleton shape="line" count={4} /></>;
  if (settings.kind === "not_ready") return <>{MASTER_TOP}<NotReady what="The master setting" /></>;
  if (settings.kind === "error") return <>{MASTER_TOP}<LoadError message={`Couldn't load the master setting. ${settings.message}`} onRetry={onRetry} /></>;
  const limit = picked ?? settings.value.limit;
  const changed = limit !== settings.value.limit;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {MASTER_TOP}
    <form className="pj set-groups" aria-label="Master" noValidate onSubmit={submit}>
      <SettingSection title="Master step limit">
        <SettingLine wide label="Most steps in one turn" changed={limit !== STEP_LIMIT_DEFAULT.limit} help={STEP_LIMIT_NOTE}>
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">Most steps in one turn</legend>
            {STEP_LIMIT_CHOICES.map((option) => <RadioCard key={option.choice} name="step-limit" value={option.choice} checked={limit === option.choice}
              onChecked={(value) => onPick(value as StepLimit)} title={option.title} details={[option.help]} />)}
          </fieldset>
        </SettingLine>
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Master: reads the owner's master step limit and saves the picked option in the
 * engine. */
export function MasterSettings() {
  const session = useAppSession();
  const [settings, reload] = useLoad(getMasterStepSettings);
  const [picked, setPicked] = useState<StepLimit | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok") return;
    setSaving(true);
    setProblem("");
    const result = await saveMasterStepSettings({ limit: picked ?? settings.value.limit });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Master step limit saved.");
    reload();
  }
  return <div className="stack">
    <MasterSettingsView settings={settings} picked={picked} saving={saving} problem={problem}
      onPick={(choice) => { setPicked(choice); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
