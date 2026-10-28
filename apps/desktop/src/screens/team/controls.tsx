import { useState } from "react";
import { delegateTeamTask, keepTeamWarm, setOnHarnessSwitch, setTeamTimeLimits, showFinishedAgents } from "../../api";
import type { Result } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { IntentReceipt, SwitchMode, TeamStatus } from "../../engine-client";
import { Button, Segmented, Select, Switch, keyLabel, shortcutKey } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { savedStale } from "../project/settings";

export const TASK_PROMPT_BYTES_MAX = 4000;
export const LIMIT_CHOICES_SECS = [1800, 3600, 7200, 14_400, 28_800] as const;
export const IDLE_CHOICES_SECS = [120, 600, 3600] as const;
export const MASTER_IDLE_STOP_SECS = 900;
const ANY_HARNESS = "";
const NO_LIMIT = "off";
const encoder = new TextEncoder();
export const PARTIAL_LINE = "Older team history is hidden so the team fits in one view. Queued and running work stays listed.";

/** What the team controls say after a save refused because the team settings changed since
 * the controls read them, once they show the saved settings again. */
export const TEAM_SETTINGS_STALE = "Team settings changed in another window, so your change was not saved. The controls now show the saved settings; make your change again.";

/** The harnesses a task can go to: every sub-agent harness of the team plan, once each, plus
 * "Any sub-agent" first. Inputs: the team status and a harness name lookup. Output: options. */
export function delegateOptions(status: Pick<TeamStatus, "harnesses">, harness: (id: string) => string = (id) => id): SelectOption[] {
  const subs = [...new Set(status.harnesses.filter((item) => item.role === "sub").map((item) => item.harness))];
  return [{ value: ANY_HARNESS, label: "Any sub-agent" }, ...subs.map((id) => ({ value: id, label: harness(id) }))];
}

/** Names a time limit the way the engine's notes do: whole hours as "1 h", whole minutes as
 * "30 min", anything else in seconds, and no limit as "Off". Input: seconds or null. Output:
 * the words. */
export function limitLabel(secs: number | null): string {
  if (secs === null) return "Off";
  if (secs >= 3600 && secs % 3600 === 0) return `${secs / 3600} h`;
  if (secs >= 60 && secs % 60 === 0) return `${secs / 60} min`;
  return `${secs} s`;
}

/** The choices for one time limit: Off first, then each of `listed`, plus the stored value
 * when it is none of those. Inputs: the stored limit and the listed choices, 30 min to 8 h by
 * default. Output: options whose value is the seconds, or "off". */
export function limitOptions(current: number | null, listed: readonly number[] = LIMIT_CHOICES_SECS): SelectOption[] {
  const choices: (number | null)[] = [null, ...listed];
  if (current !== null && !choices.includes(current)) choices.push(current);
  return choices.map((secs) => ({ value: secs === null ? NO_LIMIT : String(secs), label: limitLabel(secs) }));
}

/** Keeps the session limit not below the task limit, as the engine requires, after the owner
 * changed one of them; Off on either bounds nothing, so a session limit needs no task limit. Inputs: both limits, which one changed and its
 * new value. Output: the pair to send. */
export function limitPair(turn: number | null, session: number | null, changed: "turn" | "session", value: number | null): { turn: number | null; session: number | null } {
  const below = (a: number | null, b: number | null) => a !== null && b !== null && a < b;
  if (changed === "turn") return { turn: value, session: below(session, value) ? value : session };
  return { turn: below(value, turn) ? value : turn, session: value };
}

/** Checks a task before it is handed to a sub-agent, with the engine's own rules: not blank,
 * no control characters but tab and newline, at most 4000 bytes. Input: the text. Output: the
 * trimmed task, or the plain reason it can't be sent. */
export function taskInput(text: string): { kind: "ok"; prompt: string } | { kind: "invalid"; reason: string } {
  const prompt = text.trim();
  if (!prompt) return { kind: "invalid", reason: "Write the task first." };
  if (/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(prompt)) return { kind: "invalid", reason: "Remove the control characters from the task." };
  const bytes = encoder.encode(prompt).length;
  if (bytes > TASK_PROMPT_BYTES_MAX) return { kind: "invalid", reason: `Too long: ${bytes} of ${TASK_PROMPT_BYTES_MAX} bytes.` };
  return { kind: "ok", prompt };
}

/** The choices of the On harness switch setting, Carry over first as the default. */
export const SWITCH_OPTIONS: SelectOption[] = [
  { value: "carry", label: "Carry conversation over" },
  { value: "fresh", label: "Start fresh" },
];

/** The On harness switch mode a team status holds, Carry over when an older engine sends none.
 * Input: the status. Output: the mode. */
export function switchMode(status: Pick<TeamStatus, "on_harness_switch">): SwitchMode {
  return status.on_harness_switch ?? "carry";
}

/** What a team change's result means for the owner. Inputs: the call result, what it did and
 * the "Not ready yet" subject. Output: the note to show, empty when it applied. */
export function changeNote(result: Result<IntentReceipt>, missing: string): string {
  if (result.kind === "not_ready") return `Not ready yet: ${missing} isn't in this build.`;
  if (result.kind === "error") return failureCopy(result);
  const state = receiptState(result.value);
  if (state.kind === "applied") return "";
  return state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the team before trying again.";
}

/** The team settings version a team change must name: the newer of the last team read and
 * the version this screen's own last applied save made, so a second change before the team
 * read returns is not refused as stale. Inputs: the team status and the screen's last saved
 * version for its project, if any. Output: the version. */
export function settingsVersion(status: Pick<TeamStatus, "project_id" | "settings_version">, saved: { project: string; version: number } | null): number {
  const read = status.settings_version ?? 0;
  return saved !== null && saved.project === status.project_id ? Math.max(read, saved.version) : read;
}

/** Team controls on the Agents tab: keep the plan's minimum sub-agents ready, set how long a
 * task and an agent session may run, choose what a master harness switch does with the
 * conversation, choose whether finished agents stay in the list and graph or move to the
 * Finished fold-out, and hand one task straight to a sub-agent when the master should not
 * plan it. Each setting change names the team settings version shown; one refused as stale
 * reads the team again. Inputs: the project id, the latest team status, a harness name lookup
 * and a reload. Output: the controls. */
export function TeamControls({ projectId, status, harness, reload }: {
  projectId: string;
  status: TeamStatus;
  harness?: (id: string) => string;
  reload: () => void;
}) {
  const [warm, setWarm] = useState<boolean | null>(null);
  const [limits, setLimits] = useState<{ turn: number | null; session: number | null; idle: number | null } | null>(null);
  const [onSwitch, setOnSwitch] = useState<SwitchMode | null>(null);
  const [finished, setFinished] = useState<"hide" | "show" | null>(null);
  const [target, setTarget] = useState(ANY_HARNESS);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [sent, setSent] = useState("");
  const [saved, setSaved] = useState<{ project: string; version: number } | null>(null);
  const options = delegateOptions(status, harness);
  const checked = warm ?? status.keep_warm;
  const shown = limits ?? { turn: status.turn_limit_secs ?? null, session: status.session_limit_secs ?? null, idle: status.idle_stop_secs ?? null };
  const check = taskInput(text);
  const subs = options.length > 1;

  async function save(send: (version: number) => Promise<Result<IntentReceipt>>, missing: string): Promise<string> {
    const version = settingsVersion(status, saved);
    const result = await send(version);
    if (savedStale(result)) return TEAM_SETTINGS_STALE;
    const message = changeNote(result, missing);
    if (!message) setSaved({ project: status.project_id, version: version + 1 });
    return message;
  }

  async function toggle(on: boolean) {
    setWarm(on);
    setBusy(true);
    const message = await save((version) => keepTeamWarm(projectId, on, version), "keeping agents ready");
    setBusy(false);
    setWarm(null);
    setNote(message);
    reload();
  }

  async function limit(changed: "turn" | "session" | "idle", value: string) {
    if (busy) return;
    const secs = value === NO_LIMIT ? null : Number(value);
    const next = changed === "idle" ? { ...shown, idle: secs } : { ...limitPair(shown.turn, shown.session, changed, secs), idle: shown.idle };
    setLimits(next);
    setBusy(true);
    const message = await save((version) => setTeamTimeLimits(projectId, next.turn, next.session, next.idle, version), "team time limits");
    setBusy(false);
    setLimits(null);
    setNote(message);
    reload();
  }

  async function chooseSwitch(mode: SwitchMode) {
    setOnSwitch(mode);
    setBusy(true);
    const message = await save((version) => setOnHarnessSwitch(projectId, mode, version), "the On harness switch setting");
    setBusy(false);
    setOnSwitch(null);
    setNote(message);
    reload();
  }

  async function view(choice: "hide" | "show") {
    setFinished(choice);
    setBusy(true);
    const message = await save((version) => showFinishedAgents(projectId, choice === "show", version), "choosing where finished agents show");
    setBusy(false);
    setFinished(null);
    setNote(message);
    reload();
  }

  async function hand() {
    if (check.kind !== "ok" || busy) return;
    setBusy(true);
    const message = changeNote(await delegateTeamTask(projectId, target || null, check.prompt), "handing tasks to sub-agents");
    setBusy(false);
    setNote(message);
    if (!message) {
      setText("");
      setSent("Task handed off. It shows in the graph and the task list.");
    }
    reload();
  }

  return <section className="tg-controls" aria-label="Team controls">
    {status.partial && <p className="pj-note" role="status">{PARTIAL_LINE}</p>}
    <Switch id={`tg-warm-${projectId}`} label={`Keep ${status.min_agents} ${status.min_agents === 1 ? "sub-agent" : "sub-agents"} ready`} checked={checked}
      onChecked={(on) => void toggle(on)} disabledReason={busy ? "Saving the last change." : status.min_agents < 1 ? "The team plan keeps no sub-agents ready." : undefined} />
    <div className="tg-limits" role="group" aria-label="Time limits">
      <Select id={`tg-turn-limit-${projectId}`} label="Each task may run" value={shown.turn === null ? NO_LIMIT : String(shown.turn)} options={limitOptions(shown.turn)}
        onValue={(value) => void limit("turn", value)} />
      <Select id={`tg-session-limit-${projectId}`} label="Each agent session may run" value={shown.session === null ? NO_LIMIT : String(shown.session)} options={limitOptions(shown.session)}
        onValue={(value) => void limit("session", value)}
        helper="Off lets agents work as long as they need. A task limit stops one turn. A session counts from when the agent started; once it passes, the next task goes to a fresh agent. The conversation names the limit that stopped a turn." />
      <Select id={`tg-idle-stop-${projectId}`} label="Stop an idle sub-agent after" value={shown.idle === null ? NO_LIMIT : String(shown.idle)} options={limitOptions(shown.idle, IDLE_CHOICES_SECS)}
        onValue={(value) => void limit("idle", value)}
        helper={`Off keeps an idle sub-agent running until the team stops. An idle master stops ${limitLabel(MASTER_IDLE_STOP_SECS)} after its work is done and is never stopped while that work is still going.`} />
    </div>
    <Select id={`tg-switch-${projectId}`} label="On master harness switch" value={onSwitch ?? switchMode(status)} options={SWITCH_OPTIONS}
      onValue={(value) => { if (!busy && (value === "carry" || value === "fresh")) void chooseSwitch(value); }} />
    <div className="pj-row">
      <span aria-hidden="true">Finished agents</span>
      <Segmented label="Finished agents" value={finished ?? (status.show_finished === true ? "show" : "hide")} onValue={(choice) => { if (!busy) void view(choice); }}
        options={[{ value: "hide", label: "Hide" }, { value: "show", label: "Show" }]} />
      <span className="tg-terminal-note">Hide moves idle, stopped and failed agents to the Finished list.</span>
    </div>
    {subs ? <form className="tg-hand" onSubmit={(event) => { event.preventDefault(); void hand(); }}>
      <h4>Hand a task to a sub-agent</h4>
      <Select id={`tg-hand-to-${projectId}`} label="Send to" value={target} options={options} onValue={setTarget} />
      <label htmlFor={`tg-hand-text-${projectId}`}>Task</label>
      <textarea id={`tg-hand-text-${projectId}`} className="pj-input" rows={3} value={text} onChange={(event) => { setText(event.target.value); setSent(""); }}
        aria-describedby={`tg-hand-help-${projectId}`} onKeyDown={(event) => { if (event.key === "Enter" && shortcutKey(event)) { event.preventDefault(); void hand(); } }} />
      <div className="tg-composer-foot">
        <span className="tg-terminal-note" id={`tg-hand-help-${projectId}`}>{text && check.kind === "invalid" ? check.reason : keyLabel("Skips the master. The master sees the task and its result. Cmd+Enter to send.")}</span>
        <Button type="submit" variant="primary" size="small" icon="send" disabled={check.kind !== "ok" || busy} pending={busy} pendingLabel="Handing off...">Hand off</Button>
      </div>
    </form> : <p className="tg-terminal-note">This team has no sub-agents. Add some in the project settings to hand tasks to them.</p>}
    {sent && <p className="pj-note" role="status">{sent}</p>}
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </section>;
}
