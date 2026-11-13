import { useEffect, useState, type FormEvent } from "react";
import { getProviderRetry, saveProviderRetry, type ProviderRetry, type ProviderRetryView } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, Notice, RadioCard, SettingLine, SettingSection } from "../../ui";

/** The setting's options in list order, each with its title and what it does. */
export const PROVIDER_RETRY_CHOICES: { choice: ProviderRetry; title: string; summary: string }[] = [
  { choice: "stop_at_once", title: "Stop at once", summary: "A run fails with the provider's error instead of retrying." },
  { choice: "retry_briefly", title: "Retry briefly", summary: "Harnesses keep their own retries, but none waits for the network without end." },
  { choice: "harness_default", title: "Harness default", summary: "CrazyAgent sets nothing, so each harness does what it does on its own." },
];

/** The option a fresh engine runs under, the engine's default `ProviderRetry`. */
export const PROVIDER_RETRY_DEFAULT: ProviderRetry = "retry_briefly";

const HELP = "New runs take the choice; runs already going keep theirs. Each harness line is proven on read-only runs.";

/** The setting's section. Inputs: the read, the picked option (null until one is picked), whether
 * a save runs, a refused save's sentence, and the pick, save and retry actions. Output: the
 * section, nothing while it loads or when the engine has no such setting, or the failed read. */
export function ProviderRetrySettingsView({ retry, picked, saving, problem, onPick, onSave, onRetry }: {
  retry: Loaded<ProviderRetryView>; picked: ProviderRetry | null; saving: boolean; problem: string;
  onPick: (choice: ProviderRetry) => void; onSave: () => void; onRetry: () => void;
}) {
  if (retry.kind === "loading" || retry.kind === "not_ready") return null;
  if (retry.kind === "error") return <LoadError message={`Couldn't load the setting for model providers that can't be reached. ${retry.message}`} onRetry={onRetry} />;
  const stored = retry.value.choice;
  const choice = picked ?? stored;
  const changed = choice !== stored;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <form className="pj set-groups" aria-label="When a model provider can't be reached" noValidate onSubmit={submit}>
    <SettingSection title="When a model provider can't be reached">
      <SettingLine wide label="What harnesses do" changed={choice !== PROVIDER_RETRY_DEFAULT} help={HELP}>
        <fieldset className="field" disabled={saving}>
          <legend className="visually-hidden">What harnesses do</legend>
          {PROVIDER_RETRY_CHOICES.map((option) => <RadioCard key={option.choice} name="provider-retry" value={option.choice} checked={choice === option.choice}
            onChecked={(value) => onPick(value as ProviderRetry)} title={option.title}
            details={[option.summary, ...retry.value.notes.filter((note) => note.option === option.choice).map((note) => note.line)]} />)}
        </fieldset>
      </SettingLine>
      <div className="set-body">
        {problem && <Notice tone="danger" alert>{problem}</Notice>}
        <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
      </div>
    </SettingSection>
  </form>;
}

/** Settings, Harnesses, When a model provider can't be reached: reads the owner's choice and
 * the engine's line for every option and harness, and saves the picked option in the engine. */
export function ProviderRetrySettings() {
  const session = useAppSession();
  const [retry, reload] = useLoad(getProviderRetry);
  const [picked, setPicked] = useState<ProviderRetry | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); }, [retry]);
  async function save() {
    if (retry.kind !== "ok" || picked === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveProviderRetry(picked);
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Model provider setting saved.");
    reload();
  }
  return <ProviderRetrySettingsView retry={retry} picked={picked} saving={saving} problem={problem}
    onPick={(choice) => { setPicked(choice); setProblem(""); }} onSave={() => void save()} onRetry={reload} />;
}
