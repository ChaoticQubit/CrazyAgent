import { useState } from "react";
import { delegateTeamTask, keepTeamWarm, setAgentBranches, setBackgroundAtEnd, setAskBeforeMerge, setMasterLands, setCatchUpMain, setChildLanding, setLandOn, setMasterBranch, setMaxHarnesses, setMaxTeammates, setNewFolderLimit, setOnHarnessSwitch, setReaskEmptyReply, setSpreadAccounts, setSubAgentDepth, setTeamStopped, setTeamTimeLimits, setTokenBudget, showFinishedAgents } from "../../api";
import type { Result } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { BackgroundAtEnd, BranchMode, ChildLanding, IntentReceipt, MasterLands, SwitchMode, TeamStatus } from "../../engine-client";
import { Button, ConfirmDialog, Segmented, Select, Switch, keyLabel, shortcutKey } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { savedStale } from "../project/settings";
import { CoordinationControls } from "./coordination";

export const TASK_PROMPT_BYTES_MAX = 4000;
export const LIMIT_CHOICES_SECS = [1800, 3600, 7200, 14_400, 28_800] as const;
export const IDLE_CHOICES_SECS = [120, 600, 3600] as const;
export const MASTER_IDLE_STOP_SECS = 900;
const ANY_HARNESS = "";
const NO_LIMIT = "off";
const encoder = new TextEncoder();
export const PARTIAL_LINE = "Older team history is hidden so the team fits in one view. Queued and running work stays listed.";

/** What the team controls say after a save refused because the team settings changed since
 * the controls read them, here or in another window, once they show the saved settings again. */
export const TEAM_SETTINGS_STALE = "Team settings changed since the controls read them, so your change was not saved. The controls now show the saved settings; make your change again.";

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

/** The Sub-agent depth choices, 2 as the default: how deep leads may start workers. */
export const DEPTH_OPTIONS: SelectOption[] = [
  { value: "1", label: "1: only the master starts sub-agents" },
  { value: "2", label: "2: sub-agents lead their own workers" },
  { value: "3", label: "3: workers lead workers too" },
];
export const DEPTH_DEFAULT = 2;

/** The New folder limit choices, the engine's own list, 1,000 files as the default: the most
 * files one new top-level folder the owner's branch does not have may gain in a change set. */
export const NEW_FOLDER_OPTIONS: SelectOption[] = [100, 1000, 10000, 100000].map((files) => ({
  value: String(files),
  label: `${files.toLocaleString("en-US")} files`,
}));
export const NEW_FOLDER_DEFAULT = 1000;

/** The choices of Background commands at task end, Wait first as the default. */
export const BACKGROUND_OPTIONS: SelectOption[] = [
  { value: "wait", label: "Wait for them" },
  { value: "grace", label: "Stop them after a grace time" },
  { value: "stop", label: "Stop them when the turn ends" },
];

/** The grace time offered when the owner first picks one, and its bounds, in seconds. */
export const GRACE_SECS_DEFAULT = 120;
export const GRACE_SECS_MIN = 10;
export const GRACE_SECS_MAX = 3600;

/** The Background commands choice a team status holds, Wait when an older engine sends none.
 * Input: the status. Output: the choice. */
export function backgroundChoice(status: Pick<TeamStatus, "background_at_end">): BackgroundAtEnd {
  return status.background_at_end ?? { mode: "wait" };
}

/** Reads a grace time the owner typed: whole seconds from 10 to 3600. Input: the text.
 * Output: the seconds, or null for anything else. */
export function graceSecs(text: string): number | null {
  const typed = text.trim();
  if (!/^[0-9]{1,4}$/.test(typed)) return null;
  const secs = Number(typed);
  return secs >= GRACE_SECS_MIN && secs <= GRACE_SECS_MAX ? secs : null;
}

/** The choices of where a lead's worker lands, When the lead accepts first as the default. */
export const LANDING_OPTIONS: SelectOption[] = [
  { value: "lead", label: "When the lead accepts" },
  { value: "owner", label: "Only when I accept" },
];

/** The Max harnesses per project and Max teammates per harness choices besides No limit, the
 * engine's own list. */
export const CAP_CHOICES = [1, 2, 3, 4, 5, 6, 8, 10, 15] as const;

/** The Token budget choices besides Off, the engine's own list, in tokens. */
export const BUDGET_CHOICES = [10_000_000, 25_000_000, 50_000_000, 100_000_000, 250_000_000, 500_000_000, 1_000_000_000] as const;

/** Words a token count the way the engine does: millions with one decimal at most, else the
 * number. Input: the count. Output: the words, such as "12.5M tokens". */
export function tokenWords(tokens: number): string {
  if (tokens < 1_000_000) return `${tokens} tokens`;
  const tenths = Math.floor(tokens / 100_000);
  return tenths % 10 === 0 ? `${tenths / 10}M tokens` : `${Math.floor(tenths / 10)}.${tenths % 10}M tokens`;
}

/** The choices for one cap: No limit first, then each of `CAP_CHOICES`. Output: options whose
 * value is the number, or "off". */
export function capOptions(): SelectOption[] {
  return [{ value: NO_LIMIT, label: "No limit" }, ...CAP_CHOICES.map((cap) => ({ value: String(cap), label: String(cap) }))];
}

/** The Token budget choices: Off first, then each of `BUDGET_CHOICES`. Output: options whose
 * value is the tokens, or "off". */
export function budgetOptions(): SelectOption[] {
  return [{ value: NO_LIMIT, label: "Off" }, ...BUDGET_CHOICES.map((tokens) => ({ value: String(tokens), label: tokenWords(tokens) }))];
}

/** Reads a cap or budget choice. Inputs: the option value and the listed choices. Output: the
 * number, null for "off", or undefined for a value that is not listed. */
export function chosen(value: string, listed: readonly number[]): number | null | undefined {
  if (value === NO_LIMIT) return null;
  const number = Number(value);
  return listed.includes(number) ? number : undefined;
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

/** The team settings version a team change must name: the version of the team read whose
 * values the controls show and build the change from, never one an earlier save made, so a
 * second change before the team read returns is refused as stale instead of sending values
 * it did not read. Input: the team status shown. Output: the version, 0 when an older engine
 * sends none. */
export function settingsVersion(status: Pick<TeamStatus, "settings_version">): number {
  return status.settings_version ?? 0;
}

/** A choice the owner saved on a team control and the status value it replaced. */
export interface Held<T> {
  choice: T;
  from: T;
}

/** What a team control keeps after a save: the choice once the engine applied it, so the
 * control never flips back to the old value while the reload is on its way, and nothing after
 * a refused or unconfirmed save, so the status shows again. Inputs: the held choice and the
 * save's note, empty when the engine applied it. Output: the choice to keep, or null. */
export function afterSave<T>(held: Held<T>, note: string): Held<T> | null {
  return note === "" ? held : null;
}

/** The value a team control shows: the held choice while the status still holds the value it
 * replaced, otherwise the status, once a reload or another window moved it. Inputs: the held
 * choice or null, the status value and how two values compare. Output: the value. */
export function shownChoice<T>(held: Held<T> | null, current: T, same: (a: T, b: T) => boolean = Object.is): T {
  return held !== null && same(held.from, current) ? held.choice : current;
}

type Limits = { turn: number | null; session: number | null; idle: number | null };

function sameLimits(a: Limits, b: Limits): boolean {
  return a.turn === b.turn && a.session === b.session && a.idle === b.idle;
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
  const [warm, setWarm] = useState<Held<boolean> | null>(null);
  const [limits, setLimits] = useState<Held<Limits> | null>(null);
  const [onSwitch, setOnSwitch] = useState<Held<SwitchMode> | null>(null);
  const [finished, setFinished] = useState<Held<"hide" | "show"> | null>(null);
  const [depth, setDepth] = useState<number | null>(null);
  const [lands, setLands] = useState<ChildLanding | null>(null);
  const [branchMode, setBranchMode] = useState<BranchMode | null>(null);
  const [stopAsked, setStopAsked] = useState(false);
  const [spread, setSpread] = useState<"off" | "on" | null>(null);
  const [masterBranch, setMasterBranchText] = useState<string | null>(null);
  const [landOn, setLandOnText] = useState<string | null>(null);
  const [askMerge, setAskMerge] = useState<"off" | "on" | null>(null);
  const [masterLandsHeld, setMasterLandsHeld] = useState<MasterLands | null>(null);
  const [reask, setReask] = useState<"off" | "on" | null>(null);
  const [catchUp, setCatchUp] = useState<"off" | "on" | null>(null);
  const [newFolder, setNewFolder] = useState<number | null>(null);
  const [background, setBackground] = useState<BackgroundAtEnd | null>(null);
  const [graceText, setGraceText] = useState<string | null>(null);
  const [target, setTarget] = useState(ANY_HARNESS);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [sent, setSent] = useState("");
  const options = delegateOptions(status, harness);
  const saved: Limits = { turn: status.turn_limit_secs ?? null, session: status.session_limit_secs ?? null, idle: status.idle_stop_secs ?? null };
  const listed: "hide" | "show" = status.show_finished === true ? "show" : "hide";
  const checked = shownChoice(warm, status.keep_warm);
  const shown = shownChoice(limits, saved, sameLimits);
  const check = taskInput(text);
  const backgroundShown = background ?? backgroundChoice(status);
  const graceTyped = backgroundShown.mode === "grace" ? graceText ?? String(backgroundShown.secs) : null;
  const subs = options.length > 1;

  async function save(send: (version: number) => Promise<Result<IntentReceipt>>, missing: string): Promise<string> {
    const result = await send(settingsVersion(status));
    return savedStale(result) ? TEAM_SETTINGS_STALE : changeNote(result, missing);
  }

  async function toggle(on: boolean) {
    const held = { choice: on, from: status.keep_warm };
    setWarm(held);
    setBusy(true);
    const message = await save((version) => keepTeamWarm(projectId, on, version), "keeping agents ready");
    setBusy(false);
    setWarm(afterSave(held, message));
    setNote(message);
    reload();
  }

  async function limit(changed: "turn" | "session" | "idle", value: string) {
    if (busy) return;
    const secs = value === NO_LIMIT ? null : Number(value);
    const next = changed === "idle" ? { ...shown, idle: secs } : { ...limitPair(shown.turn, shown.session, changed, secs), idle: shown.idle };
    const held = { choice: next, from: saved };
    setLimits(held);
    setBusy(true);
    const message = await save((version) => setTeamTimeLimits(projectId, next.turn, next.session, next.idle, version), "team time limits");
    setBusy(false);
    setLimits(afterSave(held, message));
    setNote(message);
    reload();
  }

  async function chooseSwitch(mode: SwitchMode) {
    const held = { choice: mode, from: switchMode(status) };
    setOnSwitch(held);
    setBusy(true);
    const message = await save((version) => setOnHarnessSwitch(projectId, mode, version), "the On harness switch setting");
    setBusy(false);
    setOnSwitch(afterSave(held, message));
    setNote(message);
    reload();
  }

  async function chooseDepth(value: number) {
    setDepth(value);
    setBusy(true);
    const message = changeNote(await setSubAgentDepth(projectId, value), "the Sub-agent depth setting");
    setBusy(false);
    setDepth(null);
    setNote(message);
    reload();
  }

  async function chooseLanding(mode: ChildLanding) {
    setLands(mode);
    setBusy(true);
    const message = changeNote(await setChildLanding(projectId, mode), "the worker landing setting");
    setBusy(false);
    setLands(null);
    setNote(message);
    reload();
  }

  async function view(choice: "hide" | "show") {
    const held = { choice, from: listed };
    setFinished(held);
    setBusy(true);
    const message = await save((version) => showFinishedAgents(projectId, choice === "show", version), "choosing where finished agents show");
    setBusy(false);
    setFinished(afterSave(held, message));
    setNote(message);
    reload();
  }

  async function branches(choice: BranchMode) {
    setBranchMode(choice);
    setBusy(true);
    const message = changeNote(await setAgentBranches(projectId, choice), "choosing where new agents commit");
    setBusy(false);
    setBranchMode(null);
    setNote(message);
    reload();
  }

  async function stopAll(stopped: boolean) {
    setStopAsked(false);
    setBusy(true);
    const message = changeNote(await setTeamStopped(projectId, stopped), stopped ? "stopping the team" : "starting the team again");
    setBusy(false);
    setNote(message);
    reload();
  }

  async function saveMasterBranch(branch: string | null) {
    if (busy) return;
    setBusy(true);
    const message = changeNote(await setMasterBranch(projectId, branch), "the Master's branch");
    setBusy(false);
    setMasterBranchText(null);
    setNote(message);
    reload();
  }

  async function saveLandOn(branch: string | null) {
    if (busy) return;
    setBusy(true);
    const message = changeNote(await setLandOn(projectId, branch), "the Land on branch");
    setBusy(false);
    setLandOnText(null);
    setNote(message);
    reload();
  }

  async function cap(which: "harnesses" | "teammates", value: string) {
    const max = chosen(value, CAP_CHOICES);
    if (busy || max === undefined) return;
    setBusy(true);
    const message = which === "harnesses"
      ? changeNote(await setMaxHarnesses(projectId, max), "the Max harnesses setting")
      : changeNote(await setMaxTeammates(projectId, max), "the Max teammates setting");
    setBusy(false);
    setNote(message);
    reload();
  }

  async function chooseSpread(choice: "off" | "on") {
    setSpread(choice);
    setBusy(true);
    const message = changeNote(await setSpreadAccounts(projectId, choice === "on"), "the account spread setting");
    setBusy(false);
    setSpread(null);
    setNote(message);
    reload();
  }

  async function budget(value: string) {
    const tokens = chosen(value, BUDGET_CHOICES);
    if (busy || tokens === undefined) return;
    setBusy(true);
    const message = changeNote(await setTokenBudget(projectId, tokens), "the token budget");
    setBusy(false);
    setNote(message);
    reload();
  }

  async function chooseMasterLands(choice: MasterLands) {
    setMasterLandsHeld(choice);
    setBusy(true);
    const message = changeNote(await setMasterLands(projectId, choice), "what the master does with its conflict fixes");
    setBusy(false);
    setMasterLandsHeld(null);
    setNote(message);
    reload();
  }

  async function chooseAskMerge(choice: "off" | "on") {
    setAskMerge(choice);
    setBusy(true);
    const message = changeNote(await setAskBeforeMerge(projectId, choice === "on"), "the merge approval setting");
    setBusy(false);
    setAskMerge(null);
    setNote(message);
    reload();
  }

  async function chooseCatchUp(choice: "off" | "on") {
    setCatchUp(choice);
    setBusy(true);
    const message = changeNote(await setCatchUpMain(projectId, choice), "the catch up with main setting");
    setBusy(false);
    setCatchUp(null);
    setNote(message);
    reload();
  }

  async function chooseReask(choice: "off" | "on") {
    setReask(choice);
    setBusy(true);
    const message = changeNote(await setReaskEmptyReply(projectId, choice === "on"), "the empty reply setting");
    setBusy(false);
    setReask(null);
    setNote(message);
    reload();
  }

  async function chooseNewFolder(files: number) {
    setNewFolder(files);
    setBusy(true);
    const message = changeNote(await setNewFolderLimit(projectId, files), "the new folder limit");
    setBusy(false);
    setNewFolder(null);
    setNote(message);
    reload();
  }

  async function saveBackground(choice: BackgroundAtEnd) {
    if (busy) return;
    setBackground(choice);
    setBusy(true);
    const message = changeNote(await setBackgroundAtEnd(projectId, choice), "the Background commands setting");
    setBusy(false);
    setBackground(null);
    setGraceText(null);
    setNote(message);
    reload();
  }

  function chooseBackground(mode: string) {
    if (mode === "wait" || mode === "stop") void saveBackground({ mode });
    if (mode === "grace") void saveBackground({ mode, secs: backgroundShown.mode === "grace" ? backgroundShown.secs : GRACE_SECS_DEFAULT });
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
    <div className="pj-row">
      {status.paused
        ? <Button variant="primary" size="small" pending={busy} onClick={() => void stopAll(false)}>Start all again</Button>
        : <Button variant="danger" size="small" icon="circle-stop" disabled={busy} onClick={() => setStopAsked(true)}>Stop all</Button>}
      <span className="tg-terminal-note" role="status">{status.paused
        ? status.budget_reached === true
          ? "Every agent of this project is stopped: its agents used the token budget. Start them again, or raise the budget first."
          : "Every agent of this project is stopped until you start them again."
        : "Stops the master, every harness and every teammate of this project. They stay stopped across restarts."}</span>
    </div>
    <ConfirmDialog open={stopAsked} title="Stop every agent of this project?" confirmLabel="Stop all" cancelLabel="Keep running"
      pending={busy} onCancel={() => setStopAsked(false)} onConfirm={() => void stopAll(true)}>
      The master, every harness and every teammate stop now, and stay stopped across app and engine restarts until you start them again. Their work waits for them.
    </ConfirmDialog>
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
    <Select id={`tg-switch-${projectId}`} label="On master harness switch" value={shownChoice(onSwitch, switchMode(status))} options={SWITCH_OPTIONS}
      onValue={(value) => { if (!busy && (value === "carry" || value === "fresh")) void chooseSwitch(value); }} />
    <Select id={`tg-depth-${projectId}`} label="Sub-agent depth" value={String(depth ?? status.sub_agent_depth ?? DEPTH_DEFAULT)} options={DEPTH_OPTIONS}
      onValue={(value) => { const chosen = Number(value); if (!busy && [1, 2, 3].includes(chosen)) void chooseDepth(chosen); }}
      helper="Every agent counts against the team's running agent limit. Each one works in its own worktree and branch." />
    <Select id={`tg-lands-${projectId}`} label="Sub-agent work lands on its lead's branch" value={lands ?? status.child_landing ?? "lead"} options={LANDING_OPTIONS}
      onValue={(value) => { if (!busy && (value === "lead" || value === "owner")) void chooseLanding(value); }} />
    <Select id={`tg-new-folder-${projectId}`} label="Refuse a change set when one new folder adds more than" value={String(newFolder ?? status.new_folder_files_max ?? NEW_FOLDER_DEFAULT)} options={NEW_FOLDER_OPTIONS}
      onValue={(value) => { const chosen = Number(value); if (!busy && NEW_FOLDER_OPTIONS.some((option) => option.value === value)) void chooseNewFolder(chosen); }}
      helper="Counts the files a change set adds in one new top-level folder your branch does not have, such as a scratch folder an agent committed. The agent is told the folder to remove and finishes again." />
    <Select id={`tg-background-${projectId}`} label="Background commands at task end" value={backgroundShown.mode} options={BACKGROUND_OPTIONS}
      onValue={(value) => { if (!busy) chooseBackground(value); }}
      helper="Wait keeps a task open until the commands its agent started in the background end, up to the task limit. A grace time gives them that long after the turn ends, then stops them. Stop ends the task with its turn and stops them; the agent is told first. Wait and a grace time act on Claude Code panes. Other agents run per turn have their background commands stopped when the turn ends, and other kept agents, such as a kept Codex pane, keep theirs whatever the choice." />
    {graceTyped !== null && <form className="pj-row" aria-label="Grace time" onSubmit={(event) => { event.preventDefault(); const secs = graceSecs(graceTyped); if (secs === null) setNote(failureCopy("team_background_grace_invalid")); else void saveBackground({ mode: "grace", secs }); }}>
      <label htmlFor={`tg-grace-${projectId}`}>Grace time in seconds</label><input id={`tg-grace-${projectId}`} className="pj-input" inputMode="numeric" value={graceTyped}
        onChange={(event) => setGraceText(event.target.value)} aria-describedby={`tg-grace-help-${projectId}`} />
      <Button type="submit" size="small" disabled={busy || graceText === null} pending={busy && background !== null} pendingLabel="Saving...">Save</Button>
      <span className="tg-terminal-note" id={`tg-grace-help-${projectId}`}>From 10 to 3600 seconds.</span>
    </form>}
    <Select id={`tg-max-harnesses-${projectId}`} label="Max harnesses per project" value={status.max_harnesses == null ? NO_LIMIT : String(status.max_harnesses)} options={capOptions()}
      onValue={(value) => void cap("harnesses", value)}
      helper="How many harnesses the master's work runs at once. More tasks wait until one finishes." />
    <Select id={`tg-max-teammates-${projectId}`} label="Max teammates per harness" value={status.max_teammates == null ? NO_LIMIT : String(status.max_teammates)} options={capOptions()}
      onValue={(value) => void cap("teammates", value)}
      helper="How many sub-agents each harness runs at once: app-spawned sub-agents and native teammates alike. A new sub-agent waits at the limit; the engine ends the newest native teammates past it and tells the harness." />
    <div className="pj-row">
      <span aria-hidden="true">Spread harnesses across accounts</span>
      <Segmented label="Spread harnesses across accounts" value={spread ?? (status.spread_accounts === true ? "on" : "off")} onValue={(choice) => { if (!busy) void chooseSpread(choice); }}
        options={[{ value: "off", label: "Off" }, { value: "on", label: "On" }]} />
      <span className="tg-terminal-note">On, a new harness or sub-agent takes a signed-in pool account no running agent of this project uses, when there is one. Off, your account strategy picks. Teammates run on their harness's account.</span>
    </div>
    <Select id={`tg-token-budget-${projectId}`} label="Token budget" value={status.token_budget == null ? NO_LIMIT : String(status.token_budget)} options={budgetOptions()}
      onValue={(value) => void budget(value)}
      helper={status.token_budget == null
        ? "Off lets the agents use tokens as their accounts allow. A budget counts new input, cache writes and output; re-reads from cache are not counted. It counts from when you set it or start the team again."
        : `Used ${tokenWords(status.budget_used ?? 0)} of ${tokenWords(status.token_budget)}. At the budget the engine stops every agent and notifies you.`} />
    <div className="pj-row">
      <span aria-hidden="true">Finished agents</span>
      <Segmented label="Finished agents" value={shownChoice(finished, listed)} onValue={(choice) => { if (!busy) void view(choice); }}
        options={[{ value: "hide", label: "Hide" }, { value: "show", label: "Show" }]} />
      <span className="tg-terminal-note">Hide moves stopped and failed agents to the Finished list.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Agent branches</span>
      <Segmented label="Agent branches" value={branchMode ?? status.agent_branches ?? "named"} onValue={(choice) => { if (!busy) void branches(choice); }}
        options={[{ value: "named", label: "Named branch" }, { value: "detached", label: "Detached" }]} />
      <span className="tg-terminal-note">Named branch gives each new agent its own branch: a harness's is named after its task, crazyagent/&lt;task&gt;. Detached keeps the older detached HEAD.</span>
    </div>
    <form className="pj-row" aria-label="Master's branch" onSubmit={(event) => { event.preventDefault(); void saveMasterBranch((masterBranch ?? status.master_branch ?? "").trim() || null); }}>
      <label htmlFor={`tg-master-branch-${projectId}`}>Master's branch</label>
      <input id={`tg-master-branch-${projectId}`} className="pj-input" value={masterBranch ?? status.master_branch ?? ""} placeholder="Repository default"
        onChange={(event) => setMasterBranchText(event.target.value)} aria-describedby={`tg-master-branch-help-${projectId}`} />
      <Button type="submit" size="small" disabled={busy || masterBranch === null} pending={busy && masterBranch !== null} pendingLabel="Saving...">Save</Button>
      <Button type="button" size="small" disabled={busy || !status.master_branch} onClick={() => void saveMasterBranch(null)}>Use default</Button>
      <span className="tg-terminal-note" id={`tg-master-branch-help-${projectId}`}>The master works on this branch and merges finished harnesses into it, locally; nothing is pushed. Empty uses the repository's default branch.</span>
    </form>
    <form className="pj-row" aria-label="Land on" onSubmit={(event) => { event.preventDefault(); void saveLandOn((landOn ?? status.land_on ?? "").trim() || null); }}>
      <label htmlFor={`tg-land-on-${projectId}`}>Land on</label>
      <input id={`tg-land-on-${projectId}`} className="pj-input" value={landOn ?? status.land_on ?? ""} placeholder="The master's branch"
        onChange={(event) => setLandOnText(event.target.value)} aria-describedby={`tg-land-on-help-${projectId}`} />
      <Button type="submit" size="small" disabled={busy || landOn === null} pending={busy && landOn !== null} pendingLabel="Saving...">Save</Button>
      <Button type="button" size="small" disabled={busy || !status.land_on} onClick={() => void saveLandOn(null)}>Use the master's branch</Button>
      <span className="tg-terminal-note" id={`tg-land-on-help-${projectId}`}>The master's merges and your Accept in Changes land on this branch, and each change set counts only what this branch lacks. It can be a branch checked out nowhere: then your project folder's files stay as they are. Empty uses the master's branch. New agents start from its tip.</span>
    </form>
    <div className="pj-row">
      <span aria-hidden="true">Ask me before merging into the master's branch</span>
      <Segmented label="Ask me before merging into the master's branch" value={askMerge ?? (status.ask_before_merge === true ? "on" : "off")} onValue={(choice) => { if (!busy) void chooseAskMerge(choice); }}
        options={[{ value: "off", label: "Off" }, { value: "on", label: "On" }]} />
      <span className="tg-terminal-note">On keeps each finished harness's work in Changes for your Accept instead of the master merging it.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden={status.master_lands ? true : undefined}>When the master resolves conflicts</span>
      {status.master_lands
        ? <Segmented label="When the master resolves conflicts" value={masterLandsHeld ?? status.master_lands} onValue={(choice) => { if (!busy) void chooseMasterLands(choice); }}
          options={[{ value: "merges", label: "Merge into main itself" }, { value: "waits", label: "Wait for my Accept" }]} />
        : <span className="tg-terminal-note">Only at Full access.</span>}
      <span className="tg-terminal-note">Merge lands the master's own fix of a conflict on main with no Accept from you, and closes the change sets it fixed. Activity lists each merge.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Catch up with main</span>
      <Segmented label="Catch up with main" value={catchUp ?? (status.catch_up_main === "off" ? "off" : "on")} onValue={(choice) => { if (!busy) void chooseCatchUp(choice); }}
        options={[{ value: "on", label: "On" }, { value: "off", label: "Off" }]} />
      <span className="tg-terminal-note">For agents started from now on: On lets each agent bring the landed main into its own branch with git merge crazyagent/main and tells it to before it finishes; Off refuses that merge. Merging another agent's branch stays refused either way.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Re-ask a sub-agent's empty reply</span>
      <Segmented label="Re-ask a sub-agent's empty reply" value={reask ?? (status.reask_empty_reply === false ? "off" : "on")} onValue={(choice) => { if (!busy) void chooseReask(choice); }}
        options={[{ value: "on", label: "On" }, { value: "off", label: "Off" }]} />
      <span className="tg-terminal-note">On asks a sub-agent once more when its task ends with no reply text, and reports the empty reply if the second answer is empty too. Off reports the empty reply at once.</span>
    </div>
    <CoordinationControls projectId={projectId} status={status} reload={reload} />
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
