import { useEffect, useState, type FormEvent } from "react";
import { getQuitSettings, saveQuitSettings, type LeftRunning, type OnQuit, type QuitSettings } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, RadioCard, SettingLine, SettingSection, Skeleton } from "../../ui";
import "../project/projects.css";

/** The fewest and most hours agents left running wait, the engine's `HOURS_MIN` and `HOURS_MAX`. */
export const QUIT_HOURS_MIN = 1;
export const QUIT_HOURS_MAX = 168;

/** The setting an owner has until they pick another: the engine's `QuitSettings` default. */
export const QUIT_DEFAULT: QuitSettings = { on_quit: "keep_running", left_running: "after_hours", hours: 24 };

/** The quit options in the order Settings lists them, each with its title and what it does,
 * the same words in the TUI. */
export const ON_QUIT_CHOICES: { choice: OnQuit; title: string; help: string }[] = [
  { choice: "keep_running", title: "Keep team agents running", help: "They go on with their work, and CrazyAgent takes them over when it opens again." },
  { choice: "stop", title: "Stop team agents", help: "Each agent stops as Stop would, and its task goes back to the queue." },
];

/** The time limit options in the order Settings lists them. */
export const LEFT_RUNNING_CHOICES: LeftRunning[] = ["after_hours", "never"];

/** What the time limit covers, the same words in the TUI. */
export const LEFT_RUNNING_NOTE = "An agent still running when the time is up ends with its terminal. Opening CrazyAgent before then takes it over. The limit also covers agents left behind when CrazyAgent did not quit cleanly.";

/** The title of one time limit option as Settings lists it. Inputs: the option and the hours
 * the first one waits. Output: the title, such as "24 hours after CrazyAgent quits". */
export function leftRunningTitle(left: LeftRunning, hours: number): string {
  if (left === "after_hours") return `${hours} ${hours === 1 ? "hour" : "hours"} after CrazyAgent quits`;
  return "No limit, until CrazyAgent opens again";
}

/** Reads the hours field as the engine takes them. Input: the typed text. Output: the whole
 * number of hours from 1 to 168, or null for anything else. */
export function quitHours(typed: string): number | null {
  if (!/^\d{1,3}$/.test(typed.trim())) return null;
  const hours = Number(typed.trim());
  return hours >= QUIT_HOURS_MIN && hours <= QUIT_HOURS_MAX ? hours : null;
}

/** The hours a save sends for the time limit shown. Inputs: the time limit picked, the typed
 * hours and the stored hours. Output: the typed hours under the first limit, null when they
 * are not 1 to 168; under No limit, which keeps the hours for later, the typed hours when
 * valid, else the stored ones, so a bad entry never holds back the save of No limit. */
export function savedHours(left: LeftRunning, typed: string, stored: number): number | null {
  const hours = quitHours(typed);
  return left === "after_hours" || hours === null || hours === stored ? hours : hours;
}

const QUIT_TOP = <div className="settings-top"><div>
  <h2>Quitting</h2>
  <p>What happens to team agents when CrazyAgent quits.</p>
</div></div>;

/** Settings, Quitting as a view of its state: the title in every state, then the two quit
 * options and the two time limit options with the hours the first one waits, each marked
 * when away from the default, and Save changes, enabled only after a valid change. Inputs:
 * the setting read, the picked options, the typed hours, a saving flag, the last problem and
 * the handlers. Output: the tab. */
export function QuittingSettingsView({ settings, onQuit, left, typed, saving, problem, onPickQuit, onPickLeft, onType, onSave, onRetry }: {
  settings: Loaded<QuitSettings>;
  onQuit: OnQuit | null;
  left: LeftRunning | null;
  typed: string | null;
  saving: boolean;
  problem: string;
  onPickQuit: (choice: OnQuit) => void;
  onPickLeft: (choice: LeftRunning) => void;
  onType: (typed: string) => void;
  onSave: () => void;
  onRetry: () => void;
}) {
  if (settings.kind === "loading") return <>{QUIT_TOP}<Skeleton shape="line" count={4} /></>;
  if (settings.kind === "not_ready") return <>{QUIT_TOP}<NotReady what="The quitting setting" /></>;
  if (settings.kind === "error") return <>{QUIT_TOP}<LoadError message={`Couldn't load the quitting setting. ${settings.message}`} onRetry={onRetry} /></>;
  const quit = onQuit ?? settings.value.on_quit;
  const limit = left ?? settings.value.left_running;
  const text = typed ?? String(settings.value.hours);
  const hours = savedHours(limit, text, settings.value.hours);
  const shownHours = quitHours(text) ?? settings.value.hours;
  const refused = limit === "after_hours" && hours === null;
  const changed = hours !== null
    && (quit !== settings.value.on_quit || limit !== settings.value.left_running || hours !== settings.value.hours);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !saving) onSave();
  }
  return <>
    {QUIT_TOP}
    <form className="pj set-groups" aria-label="Quitting" noValidate onSubmit={submit}>
      <SettingSection title="Team agents">
        <SettingLine wide label="When CrazyAgent quits" changed={quit !== QUIT_DEFAULT.on_quit}>
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">When CrazyAgent quits</legend>
            {ON_QUIT_CHOICES.map((option) => <RadioCard key={option.choice} name="on-quit" value={option.choice} checked={quit === option.choice}
              onChecked={(value) => onPickQuit(value as OnQuit)} title={option.title} details={[option.help]} />)}
          </fieldset>
        </SettingLine>
        <SettingLine wide label="End agents left running" changed={limit !== QUIT_DEFAULT.left_running || shownHours !== QUIT_DEFAULT.hours} help={LEFT_RUNNING_NOTE}>
          <fieldset className="field" disabled={saving}>
            <legend className="visually-hidden">End agents left running</legend>
            {LEFT_RUNNING_CHOICES.map((option) => <RadioCard key={option} name="left-running" value={option} checked={limit === option}
              onChecked={(value) => onPickLeft(value as LeftRunning)} title={leftRunningTitle(option, shownHours)} />)}
          </fieldset>
          <div className="pj-row">
            <label htmlFor="left-running-hours">Hours after CrazyAgent quits</label>
            <input className="pj-input num" id="left-running-hours" type="number" min={QUIT_HOURS_MIN} max={QUIT_HOURS_MAX} value={text}
              disabled={saving || limit !== "after_hours"} onChange={(event) => onType(event.target.value)}
              aria-invalid={refused || undefined} aria-describedby={refused ? "left-running-hours-error" : undefined} />
          </div>
          {refused && <p className="pj-error" id="left-running-hours-error" role="alert">{`Enter a whole number of hours from ${QUIT_HOURS_MIN} to ${QUIT_HOURS_MAX}.`}</p>}
        </SettingLine>
        <div className="set-body">
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          <span><Button type="submit" variant="primary" disabled={!changed} pending={saving} pendingLabel="Saving...">Save changes</Button></span>
        </div>
      </SettingSection>
    </form>
  </>;
}

/** Settings, Quitting: reads the owner's quitting setting and saves the picked options and
 * hours in the engine. */
export function QuittingSettings() {
  const session = useAppSession();
  const [settings, reload] = useLoad(getQuitSettings);
  const [onQuit, setOnQuit] = useState<OnQuit | null>(null);
  const [left, setLeft] = useState<LeftRunning | null>(null);
  const [typed, setTyped] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  useEffect(() => { setOnQuit(null); setLeft(null); setTyped(null); }, [settings]);
  async function save() {
    if (settings.kind !== "ok") return;
    const limit = left ?? settings.value.left_running;
    const hours = savedHours(limit, typed ?? String(settings.value.hours), settings.value.hours);
    if (hours === null) return;
    setSaving(true);
    setProblem("");
    const result = await saveQuitSettings({
      on_quit: onQuit ?? settings.value.on_quit,
      left_running: limit,
      hours,
    });
    setSaving(false);
    if (result.kind === "error") { setProblem(result.message); return; }
    if (result.kind === "not_ready") { setProblem("The engine is not ready yet. Try again."); return; }
    session.showToast("Quitting setting saved.");
    reload();
  }
  return <div className="stack">
    <QuittingSettingsView settings={settings} onQuit={onQuit} left={left} typed={typed} saving={saving} problem={problem}
      onPickQuit={(choice) => { setOnQuit(choice); setProblem(""); }} onPickLeft={(choice) => { setLeft(choice); setProblem(""); }}
      onType={(text) => { setTyped(text); setProblem(""); }} onSave={() => void save()} onRetry={reload} />
  </div>;
}
