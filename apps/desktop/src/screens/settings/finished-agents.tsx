import { useEffect, useState, type FormEvent } from "react";
import { getCleanupSettings, saveCleanupSettings, type CleanupMode, type CleanupSettings } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, RadioCard, SettingLine, SettingSection, Skeleton } from "../../ui";
import "../project/projects.css";

/** The fewest and most days the first option may wait, the engine's `DAYS_MIN` and `DAYS_MAX`. */
export const CLEANUP_DAYS_MIN = 1;
export const CLEANUP_DAYS_MAX = 365;

/** The setting an owner has until they pick another: the engine's `CleanupSettings` default. */
export const CLEANUP_DEFAULT: CleanupSettings = { mode: "after_days", days: 1 };

/** What every option removes and what stays, the same words in the TUI. */
export const CLEANUP_NOTE = "Only the worktree goes: the code checkout and its build files. The agent's conversations, history, logs and messages stay. An agent whose work has not landed stays, and Clean up shows why.";

/** The title of one option as Settings lists it. Inputs: the mode and the days the first
 * option waits. Output: the title, such as "3 days after the agent finishes". */
export function cleanupModeTitle(mode: CleanupMode, days: number): string {
  if (mode === "after_days") return `${days} ${days === 1 ? "day" : "days"} after the agent finishes`;
  if (mode === "once_landed") return "Once its work lands";
  return "Only when I click Clean up";
}

/** The options in the order Settings lists them. */
export const CLEANUP_MODES: CleanupMode[] = ["after_days", "once_landed", "manual"];

/** Reads the days field as the engine takes them. Input: the typed text. Output: the whole
 * number of days from 1 to 365, or null for anything else. */
export function cleanupDays(typed: string): number | null {
  if (!/^\d{1,3}$/.test(typed.trim())) return null;
  const days = Number(typed.trim());
  return days >= CLEANUP_DAYS_MIN && days <= CLEANUP_DAYS_MAX ? days : null;
}

const FINISHED_TOP = <div className="settings-top"><div>
  <h2>Finished agents</h2>
  <p>What CrazyAgent removes once an agent finishes.</p>
</div></div>;

/** Settings, Finished agents as a view of its state: the title in every state, then the three
 * options with the days the first one waits, marked when away from the default, what goes and
 * what stays, and Save changes, enabled only after a valid change. Inputs: the setting read,
 * the picked mode, the typed days, a saving flag, the last problem and the handlers. Output:
 * the tab. */
export function FinishedAgentsSettingsView({ settings, picked, typed, saving, problem, onPick, onType, onSave, onRetry }: {
  settings: Loaded<CleanupSettings>;
  picked: CleanupMode | null;
  typed: string | null;
  saving: boolean;
  problem: string;
  onPick: (mode: CleanupMode) => void;
  onType: (typed: string) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <>{FINISHED_TOP}<Skeleton shape="line" count={4} /></>;
  if (settings.kind === "not_ready") return <>{FINISHED_TOP}<NotReady what="The finished agents setting" /></>;
  if (settings.kind === "error") return <>{FINISHED_TOP}<LoadError message={`Couldn't load the finished agents setting. ${settings.message}`} onRetry={onRetry} /></>;
  const mode = picked ?? settings.value.mode;
  const text = typed ?? String(settings.value.days);
  const days = cleanupDays(text);
  const shownDays = days ?? settings.value.days;
  const changed = days !== null && (mode !== settings.value.mode || days !== settings.value.days);
  const away = mode !== CLEANUP_DEFAULT.mode || shownDays !== CLEANUP_DEFAULT.days;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {FINISHED_TOP}
    <form className="pj set-groups" aria-label="Remove finished agents' worktrees" noValidate onSubmit={submit}>
      <SettingSection title="Worktrees">
        <SettingLine wide label="Remove finished agents' worktrees" changed={away} help={CLEANUP_NOTE}>
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">Remove finished agents' worktrees</legend>
            {CLEANUP_MODES.map((option) => <RadioCard key={option} name="finished-agents" value={option} checked={mode === option}
              onChecked={(value) => onPick(value as CleanupMode)} title={cleanupModeTitle(option, shownDays)} />)}
          </fieldset>
          <div className="pj-row">
            <label htmlFor="finished-agents-days">Days after the agent finishes</label>
            <input className="pj-input num" id="finished-agents-days" type="number" min={CLEANUP_DAYS_MIN} max={CLEANUP_DAYS_MAX} value={text}
              disabled={saving || mode !== "after_days"} onChange={(event) => onType(event.target.value)}
              aria-invalid={days === null || undefined} aria-describedby={days === null ? "finished-agents-days-error" : undefined} />
          </div>
          {days === null && <p className="pj-error" id="finished-agents-days-error" role="alert">{`Enter a whole number of days from ${CLEANUP_DAYS_MIN} to ${CLEANUP_DAYS_MAX}.`}</p>}
        </SettingLine>
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Finished agents: reads the owner's finished agents setting and saves the picked
 * mode and days in the engine. */
export function FinishedAgentsSettings() {
  const session = useAppSession();
  const [settings, reload] = useLoad(getCleanupSettings);
  const [picked, setPicked] = useState<CleanupMode | null>(null);
  const [typed, setTyped] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setPicked(null); setTyped(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok") return;
    const days = cleanupDays(typed ?? String(settings.value.days));
    if (days === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveCleanupSettings({ mode: picked ?? settings.value.mode, days });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Finished agents setting saved.");
    reload();
  }
  return <div className="stack">
    <FinishedAgentsSettingsView settings={settings} picked={picked} typed={typed} saving={saving} problem={problem}
      onPick={(mode) => { setPicked(mode); setProblem(""); }} onType={(text) => { setTyped(text); setProblem(""); }}
      onSave={() => void save()} onRetry={reload} />
  </div>;
}
