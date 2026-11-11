import { useState, type ReactNode } from "react";
import { listSkills, setCoordSettings, setHarnessDelivery, setHarnessMode, setTerminalsPerWindow, type Result, type SkillView } from "../../api";
import { loadTeamStatus } from "../../api-projects";
import { OVERLAP_CHOICES } from "../../engine-client";
import type { ClaudeDisplay, CoordHarness, CoordinationView, CoordSettings, Delivery, HarnessCoordination, IntentReceipt, Nudges, Overlaps, QuestionsTo, SkillRole, SubagentMode, TaskStart, TeamStatus } from "../../engine-client";
import { sameJson, useHeld, useLoad, type Loaded } from "../../state";
import { Button, Checkbox, Segmented, Select } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { SettingRow, useChange } from "./controls";
import { McpServersSetting } from "./setup-rows";

export const COUNTED_TOOLS_MAX = 32;
const COUNTED_TOOL = /^[A-Za-z0-9_*-]{1,64}$/;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The harness kinds coordination settings are kept for, in the order they are listed. */
export const COORD_HARNESSES: CoordHarness[] = ["claude-code", "codex", "rcli"];

/** The plan defaults the engine uses for a project with no saved coordination settings. */
export const COORD_DEFAULTS: CoordSettings = {
  questions_to: "lead",
  overlaps: "warn",
  nudges: "off",
  counted_tools: ["start_work", "inspect_work", "update_work", "transfer_work", "finish_work", "ctx_*", "graphify", "Skill"],
  role_skills: { master: null, lead: null, worker: null },
  claude_display: "classic",
  task_start: "on",
};

const ROLES: { role: SkillRole; label: string; about: string }[] = [
  { role: "master", label: "Master", about: "The project's master agent." },
  { role: "lead", label: "Lead", about: "A sub-agent that can start sub-agents of its own." },
  { role: "worker", label: "Worker", about: "Any other sub-agent or teammate." },
];

/** The skill names a project's agents can be given: each skill that is on, on in this
 * project and readable, once per name, in name order. Input: the project's skills read.
 * Output: the names. */
export function approvedSkills(skills: SkillView[]): string[] {
  return [...new Set(skills.filter((skill) => skill.enabled && skill.on_here !== false && !skill.refused).map((skill) => skill.name))].sort();
}

/** One role's names to save after a skill's box changes: its chosen names, or every approved
 * name while it gets them all, with that skill added or taken out, in name order. Inputs: the
 * role's choice, the approved names, the skill and its new state. Output: the names. */
export function tickedSkills(chosen: string[] | null, approved: string[], name: string, on: boolean): string[] {
  const names = (chosen ?? approved).filter((kept) => kept !== name);
  return on ? [...names, name].sort() : names;
}

/** Skills per role (DEC-111 R3): each team role with All approved skills or Chosen skills and,
 * for a role on Chosen skills, a box per approved skill; chosen names that are not approved
 * here are listed so they can be seen. Inputs: the settings, the project's skills read,
 * whether a change is saving and the change. Output: the controls. */
export function RoleSkillsControls({ settings, skills, busy, onChoose }: {
  settings: CoordSettings;
  skills: Loaded<SkillView[]>;
  busy: boolean;
  onChoose: (role: SkillRole, chosen: string[] | null) => void;
}) {
  const approved = skills.kind === "ok" ? approvedSkills(skills.value) : [];
  return <div className="stack-small" role="group" aria-label="Skills per role">
    {ROLES.map(({ role, label, about }) => {
      const chosen = settings.role_skills[role];
      const unapproved = skills.kind === "ok" ? (chosen ?? []).filter((name) => !approved.includes(name)) : [];
      return <div className="pj-row" key={role}>
        <span aria-hidden="true">{label} skills</span>
        <Segmented label={`${label} skills`} value={chosen ? "chosen" : "all"}
          onValue={(next) => { if (!busy && (next === "chosen") !== Boolean(chosen)) onChoose(role, next === "all" ? null : [...approved]); }}
          options={[{ value: "all", label: "All approved skills" }, { value: "chosen", label: "Chosen skills" }]} />
        <span className="tg-terminal-note">{about}</span>
        {chosen && (skills.kind === "loading" ? <span className="tg-terminal-note">Loading this project's skills...</span>
          : skills.kind !== "ok" ? <span className="tg-terminal-note">Couldn't load this project's skills.</span>
          : approved.length === 0 ? <span className="tg-terminal-note">No approved skills for this project yet.</span>
          : <span className="row">{approved.map((name) => <Checkbox key={name} label={name} checked={chosen.includes(name)} disabled={busy}
            onChecked={(on) => onChoose(role, tickedSkills(chosen, approved, name, on))} />)}</span>)}
        {unapproved.length > 0 && <span className="tg-terminal-note">Chosen but not approved here, so no agent gets them: {unapproved.join(", ")}.</span>}
      </div>;
    })}
  </div>;
}

/** Each coordination harness id with the name the app shows for it. */
export const HARNESS_NAMES: Record<CoordHarness, string> = { "claude-code": "Claude Code", codex: "Codex", rcli: "rcli" };

const NO_SUB_AGENTS = "Not available: runs only as a master";
const SUB_AGENT_HARNESSES: readonly CoordHarness[] = ["claude-code", "codex"];

/** Tells why no sub-agent runs on a harness kind: a kind the engine launches no team agent on,
 * today rcli, keeps its rows, which say so instead of a live control (DEC-141 PA-15). The
 * engine's team::launch_harness and NO_SUB_AGENTS are the source a test ties this to.
 * Input: the harness kind. Output: the reason, or undefined for a kind sub-agents run on. */
export function subAgentsUnavailable(harness: CoordHarness): string | undefined {
  return SUB_AGENT_HARNESSES.includes(harness) ? undefined : NO_SUB_AGENTS;
}

function UnavailableRow({ label, reason, helper }: { label: string; reason: string; helper?: string }) {
  return <SettingRow label={label}>
    <span className="set-label">{label}</span>
    <span className="tg-terminal-note">{reason}</span>
    {helper && <span className="tg-terminal-note">{helper}</span>}
  </SettingRow>;
}

/** Why a harness kind has no native sub-agents, in the engine's words (PA-7), or null for a kind
 * that runs them: Claude Code as agent teams, Codex as sub-agents of the lead's own session. */
export const NATIVE_UNAVAILABLE: Record<CoordHarness, string | null> = {
  "claude-code": null,
  codex: null,
  rcli: "RCLI runs without tools, so it has no native sub-agents yet; its sub-agents are CrazyAgent workers.",
};

/** What the app cannot show of Codex's own sub-agents, in the engine's words (PA-7 r1): they show
 * only in the lead's terminal, the graph, counts and Stop act on the lead, and No limit is 15. */
export const CODEX_NATIVE_SCOPE = "Codex sub-agents show only in the lead's terminal, where Codex lists each one: the app's graph, counts and Stop act on the lead, their team tool calls count as the lead's, and Codex holds them to Max teammates at once, 15 under No limit because Codex needs a number.";

const NATIVE_LABELS: Record<CoordHarness, string> = { "claude-code": "Native agent teams", codex: "Native Codex sub-agents", rcli: "Native sub-agents" };

const NATIVE_HELPERS: Record<CoordHarness, string> = {
  "claude-code": "Native agent teams run inside the lead's own session. CrazyAgent workers get their own terminal, worktree and branch.",
  codex: `Native Codex sub-agents run inside the lead's own session and worktree. Codex starts them on the lead's model and offers no other, but still runs a model a sub-agent names itself. ${CODEX_NATIVE_SCOPE} CrazyAgent workers get their own terminal, worktree and branch.`,
  rcli: "Native sub-agents run inside the lead's own session. CrazyAgent workers get their own terminal, worktree and branch.",
};

/** The Sub-agent mode choices in the mockup's words, native first for every harness kind; a kind
 * without native sub-agents lists it unavailable with its reason, so nothing is hidden per harness.
 * Inputs: the harness kind and whether it has them. Output: the options. */
export function modeOptions(harness: CoordHarness, native: boolean): SelectOption[] {
  const reason = native ? undefined : NATIVE_UNAVAILABLE[harness] ?? `${HARNESS_NAMES[harness]} has no native sub-agents, so its sub-agents are CrazyAgent workers.`;
  return [{ value: "native", label: NATIVE_LABELS[harness], ...(reason ? { disabledReason: reason } : {}) }, { value: "app_spawned", label: "CrazyAgent workers" }];
}

/** The helper line under a harness kind's Sub-agent mode: what native sub-agents are on it, or why
 * it has none. Inputs: the harness kind and whether it has them. Output: the line. */
export function modeHelper(harness: CoordHarness, native: boolean): string {
  const reason = modeOptions(harness, native)[0].disabledReason;
  return reason ?? NATIVE_HELPERS[harness];
}

/** The Claude Code terminal display choices, Classic first as the default. */
export const DISPLAY_OPTIONS: SelectOption[] = [
  { value: "classic", label: "Classic" },
  { value: "fullscreen", label: "Fullscreen" },
];

const DISPLAY_HELPER: Record<ClaudeDisplay, string> = {
  classic: "Classic: the mouse wheel scrolls the terminal at once and a plain drag selects text. A change applies when a Claude Code terminal next starts.",
  fullscreen: "Fullscreen: Claude Code draws its own view and takes the mouse; the wheel scrolls 3 rows a notch and Option-drag selects text. A change applies when a Claude Code terminal next starts.",
};

/** The Delivery choices, Auto first as the default. */
export const DELIVERY_OPTIONS: SelectOption[] = [
  { value: "auto", label: "Auto: native, typing as fallback" },
  { value: "native_only", label: "Native only" },
  { value: "typing_only", label: "Typing only" },
];

/** What the Delivery choices do on each harness kind, shown under its Messages to row (PA-11,
 * DEC-141): Native only never types, and a harness without a channel while its agent works
 * says how its messages arrive instead. The TUI says the same sentences. */
export const DELIVERY_HELPER: Record<CoordHarness, string> = {
  "claude-code": "Auto hands a message to a Claude Code agent through its relay and types it in if the relay has not taken it after 60 seconds. Native only uses the relay alone and never types. Typing only types every message in.",
  codex: "Auto hands a message to a Codex agent with tools on through its app-server bridge and types it in when the bridge can't take it. Native only never types: with tools off, a Codex agent gets its messages as a task of their own once it is idle. Typing only types every message in.",
  rcli: "rcli runs no team agents, so no message goes to one here. A master on any harness hears its messages through the engine under every choice.",
};

/** The coordination settings a team status holds, with the plan defaults for an engine that
 * sends none. Input: the status. Output: the settings and every harness kind's row. */
export function coordination(status: Pick<TeamStatus, "coordination">): CoordinationView {
  const view = status.coordination;
  const harnesses = COORD_HARNESSES.map((harness) => view?.harnesses.find((row) => row.harness === harness)
    ?? { harness, mode: "app_spawned" as const, delivery: "auto" as const, native: NATIVE_UNAVAILABLE[harness] === null });
  return { settings: view?.settings ?? COORD_DEFAULTS, harnesses };
}

/** Reads the counted-tools list the owner typed, one name per line or comma separated, with the
 * engine's rule: up to 32 names of letters, digits, _, - or * (a wildcard), each up to 64
 * characters. Input: the text. Output: the names in order without repeats, or the reason. */
export function countedToolsInput(text: string): { kind: "ok"; tools: string[] } | { kind: "invalid"; reason: string } {
  const tools = [...new Set(text.split(/[\n,]/).map((name) => name.trim()).filter(Boolean))];
  const bad = tools.find((name) => !COUNTED_TOOL.test(name));
  if (bad) return { kind: "invalid", reason: `"${bad}" isn't a tool name. Use letters, digits, _, - or * only.` };
  if (tools.length > COUNTED_TOOLS_MAX) return { kind: "invalid", reason: `Count at most ${COUNTED_TOOLS_MAX} tools.` };
  return { kind: "ok", tools };
}

const OVERLAP_LABELS: Record<Overlaps, string> = { warn: "Warn both (default)", wait: "Wait until the first lands", block: "Block the later one", off: "Off" };

function knownOverlap(value: string): value is Overlaps {
  return (OVERLAP_CHOICES as readonly string[]).includes(value);
}

/** The Overlapping edits choices: the engine's known ones in the mockup's order and words, then
 * the engine's value as it is when this app does not know it, so it is never dropped or
 * remapped. Input: the engine's value. Output: the options. */
export function overlapOptions(current: string): { value: string; label: string }[] {
  const known = (Object.keys(OVERLAP_LABELS) as Overlaps[]).map((value) => ({ value: value as string, label: OVERLAP_LABELS[value] }));
  return knownOverlap(current) ? known : [...known, { value: current, label: displayText(current) }];
}

const chains = new Map<string, Promise<unknown>>();

/** Saves one coordination change through the project's one save path, as every row that
 * changes coordination settings does: a change waits for the save before it, then reads the
 * engine's settings and lays only its own fields over them, so a change the TUI or another
 * window made since stays. Inputs: the project id and the change. Output: the save's result,
 * or the team read's failure, with nothing sent. */
export function saveCoordSettings(projectId: string, patch: Partial<CoordSettings>): Promise<Result<IntentReceipt>> {
  assert(Object.keys(patch).length > 0, "a coordination save changes at least one setting");
  const run = (chains.get(projectId) ?? Promise.resolve()).then(async (): Promise<Result<IntentReceipt>> => {
    const team = await loadTeamStatus(projectId);
    if (team.kind !== "ok") return team;
    assert(team.value.project_id === projectId, "a coordination save builds on its own project's team read");
    return setCoordSettings(projectId, { ...coordination(team.value).settings, ...patch });
  });
  chains.set(projectId, run.catch(() => undefined));
  return run;
}

function ChangeNote({ note }: { note: string }) {
  return note ? <p className="pj-note warn" role="alert">{displayText(note)}</p> : null;
}

/** Where sub-agent questions go, as one row, on Agent settings and in the master's settings.
 * Inputs: the project id, the team status and a reload. Output: the row. */
export function QuestionsSetting({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const { note, change } = useChange(reload);
  const [questionsTo, hold] = useHeld<QuestionsTo>(coordination(status).settings.questions_to, Object.is, status);
  return <SettingRow label="Sub-agent questions go to">
    <span className="set-label" aria-hidden="true">Sub-agent questions go to</span>
    <Segmented label="Sub-agent questions go to" value={questionsTo} onValue={(value: QuestionsTo) => void change(() => saveCoordSettings(projectId, { questions_to: value }), "where questions go", () => hold(value))}
      options={[{ value: "lead", label: "Their lead" }, { value: "me", label: "Me directly" }]} />
    <span className="tg-terminal-note">A question an agent marks For: Me always reaches you; no lead or master can answer it.</span>
    <ChangeNote note={note} />
  </SettingRow>;
}

/** How messages reach one harness kind's agents, as one row, on Agent settings and in the
 * master's settings. Inputs: the project id, the harness kind's row, the team status it came
 * from and a reload. Output: the row. */
export function DeliverySetting({ projectId, row, status, reload }: { projectId: string; row: HarnessCoordination; status: TeamStatus; reload: () => void }) {
  const { note, change } = useChange(reload);
  const [delivery, hold] = useHeld<Delivery>(row.delivery, Object.is, status);
  const label = `Messages to ${HARNESS_NAMES[row.harness]} agents`;
  const unavailable = subAgentsUnavailable(row.harness);
  if (unavailable) return <UnavailableRow label={label} reason={unavailable} helper={DELIVERY_HELPER[row.harness]} />;
  return <SettingRow label={label}>
    <Select id={`tg-delivery-${row.harness}-${projectId}`} label={label} value={delivery} options={DELIVERY_OPTIONS}
      onValue={(value) => { if (value === "auto" || value === "native_only" || value === "typing_only") void change(() => setHarnessDelivery(projectId, row.harness, value as Delivery), "message delivery", () => hold(value as Delivery)); }} />
    <span className="tg-terminal-note">{DELIVERY_HELPER[row.harness]}</span>
    <ChangeNote note={note} />
  </SettingRow>;
}

/** The Skills and tools rows of Agent settings: the skills each team role gets, the MCP servers
 * agents can reach, workflow nudges, the Task-start reminder and the counted tools. Every option is selectable; the plan
 * defaults show for an older engine. Inputs: the project id, the latest team status and a
 * reload. Output: the rows. */
export function ToolsControls({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const [settings, hold] = useHeld(coordination(status).settings, sameJson, status);
  const [skills] = useLoad(() => listSkills(projectId), [projectId], "skills");
  const { busy, note, change } = useChange(reload);
  const [counted, setCounted] = useState<string | null>(null);
  const countedText = counted ?? settings.counted_tools.join("\n");
  const countedCheck = countedToolsInput(countedText);

  const save = (patch: Partial<CoordSettings>, what: string) =>
    change(() => saveCoordSettings(projectId, patch), what, () => hold({ ...settings, ...patch }));

  async function saveCounted() {
    if (countedCheck.kind !== "ok") return;
    await save({ counted_tools: countedCheck.tools }, "the counted tools");
    setCounted(null);
  }

  return <>
    <SettingRow label="Skills per role" wide>
      <span className="set-label" aria-hidden="true">Skills per role</span>
      <RoleSkillsControls settings={settings} skills={skills} busy={busy}
        onChoose={(role, chosen) => void save({ role_skills: { ...settings.role_skills, [role]: chosen } }, `the ${role} skills`)} />
    </SettingRow>
    <McpServersSetting projectId={projectId} />
    <SettingRow label="Workflow nudges">
      <span className="set-label" aria-hidden="true">Workflow nudges</span>
      <Segmented label="Workflow nudges" value={settings.nudges} onValue={(value: Nudges) => void save({ nudges: value }, "workflow nudges")}
        options={[{ value: "off", label: "Off" }, { value: "ten_minutes", label: "Every 10 minutes" }, { value: "first_edit", label: "First edit" }]} />
      <span className="tg-terminal-note">Every 10 minutes sends an agent one reminder when it edits without start_work, or works 10 minutes without graphify or ctx tools. First edit reminds an agent once when it edits a file before any graphify query; the edit still runs.</span>
    </SettingRow>
    <SettingRow label="Task-start reminder">
      <span className="set-label" aria-hidden="true">Task-start reminder</span>
      <Segmented label="Task-start reminder" value={settings.task_start} onValue={(value: TaskStart) => void save({ task_start: value }, "the task-start reminder")}
        options={[{ value: "on", label: "On" }, { value: "off", label: "Off" }]} />
      <span className="tg-terminal-note">On adds a short reminder to the first turn of each task of every sub-agent and teammate with tools on: load the context-mode tools (Claude Code runs ToolSearch with their select string) and use the approved skills that fit. Off adds nothing. It is advice; nothing is blocked.</span>
    </SettingRow>
    <SettingRow label="Counted tools" wide>
      <form className="pj-row" aria-label="Counted tools" onSubmit={(event) => { event.preventDefault(); void saveCounted(); }}>
        <label htmlFor={`tg-counted-${projectId}`}>Counted tools</label>
        <textarea id={`tg-counted-${projectId}`} className="pj-input" rows={3} value={countedText} onChange={(event) => setCounted(event.target.value)}
          aria-describedby={`tg-counted-help-${projectId}`} />
        <Button type="submit" size="small" disabled={busy || counted === null || countedCheck.kind !== "ok"} pending={busy && counted !== null} pendingLabel="Saving...">Save</Button>
        <Button type="button" size="small" disabled={busy} onClick={() => { setCounted(null); void save({ counted_tools: COORD_DEFAULTS.counted_tools }, "the counted tools"); }}>Use defaults</Button>
        <span className="tg-terminal-note" id={`tg-counted-help-${projectId}`}>{counted !== null && countedCheck.kind === "invalid" ? countedCheck.reason
          : "The Process page and the graph count these tool calls per agent. One name per line; * matches any ending, such as ctx_*."}</span>
      </form>
    </SettingRow>
    <ChangeNote note={note} />
  </>;
}

/** One harness kind's sub-agent mode as a row: native sub-agents listed for every kind, chosen only where it has them.
 * Inputs: the project id, the harness kind's row, the team status it came from and a reload.
 * Output: the row. */
function SubAgentModeSetting({ projectId, row, status, reload }: { projectId: string; row: HarnessCoordination; status: TeamStatus; reload: () => void }) {
  const { note, change } = useChange(reload);
  const [mode, hold] = useHeld<SubagentMode>(row.mode, Object.is, status);
  const unavailable = subAgentsUnavailable(row.harness);
  if (unavailable) return <UnavailableRow label={`${HARNESS_NAMES[row.harness]} sub-agents`} reason={unavailable} />;
  return <SettingRow label={`${HARNESS_NAMES[row.harness]} sub-agents`}>
    <Select id={`tg-mode-${row.harness}-${projectId}`} label={`${HARNESS_NAMES[row.harness]} sub-agents`} value={mode} options={modeOptions(row.harness, row.native)}
      onValue={(value) => { if (value === "app_spawned" || (value === "native" && row.native)) void change(() => setHarnessMode(projectId, row.harness, value as SubagentMode), "the sub-agent mode", () => hold(value as SubagentMode)); }}
      helper={modeHelper(row.harness, row.native)} />
    <ChangeNote note={note} />
  </SettingRow>;
}

/** The Coordination rows of Agent settings in the approved mockup's order: each harness kind's
 * sub-agent mode, then the rows handed in, where sub-agent questions go, what overlapping claims
 * do and the terminals per window, with the engine's choices and any value it holds that this
 * app does not know. Every option is selectable; the plan defaults show for an older engine.
 * Inputs: the project id, the latest team status, a reload and rows to show after the sub-agent
 * rows. Output: the rows. */
export function CoordinationControls({ projectId, status, reload, children }: { projectId: string; status: TeamStatus; reload: () => void; children?: ReactNode }) {
  const view = coordination(status);
  const { note, change } = useChange(reload);
  const [overlaps, holdOverlaps] = useHeld<string>(view.settings.overlaps, Object.is, status);
  const [perWindow, holdPerWindow] = useHeld(String(status.terminals_per_window ?? 6), Object.is, status);

  return <>
    {view.harnesses.map((row) => <SubAgentModeSetting key={row.harness} projectId={projectId} row={row} status={status} reload={reload} />)}
    {children}
    <QuestionsSetting projectId={projectId} status={status} reload={reload} />
    <SettingRow label="Overlapping edits" wide>
      <span className="set-label" aria-hidden="true">Overlapping edits</span>
      <span className="tg-terminal-note">Warn tells both agents and their leads when their claimed files overlap. Block also refuses the edit. Wait holds back an agent starting on a file another agent is on, and tells it when the file is free.</span>
      <Segmented label="Overlapping edits" value={overlaps} options={overlapOptions(overlaps)}
        onValue={(value) => { if (knownOverlap(value)) void change(() => saveCoordSettings(projectId, { overlaps: value }), "overlapping edits", () => holdOverlaps(value)); }} />
    </SettingRow>
    <SettingRow label="Terminals per window">
      <span className="set-label" aria-hidden="true">Terminals per window</span>
      <Segmented label="Terminals per window" value={perWindow}
        onValue={(value) => { if (value === "4" || value === "6" || value === "9") void change(() => setTerminalsPerWindow(projectId, Number(value) as 4 | 6 | 9), "terminals per window", () => holdPerWindow(value)); }}
        options={[{ value: "4", label: "4" }, { value: "6", label: "6 (default)" }, { value: "9", label: "9" }]} />
      <span className="tg-terminal-note">How many terminals Open all terminals puts in one window. More agents open more windows.</span>
    </SettingRow>
    <ChangeNote note={note} />
  </>;
}

/** The per-harness rows the approved mockup leaves off Agent settings, kept under each harness:
 * how messages reach its agents and, for Claude Code, the terminal display. Every option is
 * selectable; the plan defaults show for an older engine. Inputs: the project id, the latest
 * team status and a reload. Output: one group per harness kind. */
export function PerHarnessControls({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const view = coordination(status);
  const { note, change } = useChange(reload);
  const [display, holdDisplay] = useHeld<ClaudeDisplay>(view.settings.claude_display, Object.is, status);
  return <>
    {view.harnesses.map((row) => <div className="set-harness" role="group" aria-label={HARNESS_NAMES[row.harness]} key={row.harness}>
      <DeliverySetting projectId={projectId} row={row} status={status} reload={reload} />
      {row.harness === "claude-code" && <SettingRow label="Claude Code terminal display">
        <Select id={`tg-display-claude-code-${projectId}`} label="Claude Code terminal display" value={display} options={DISPLAY_OPTIONS}
          onValue={(value) => { if (value === "classic" || value === "fullscreen") void change(() => saveCoordSettings(projectId, { claude_display: value }), "the Claude Code terminal display", () => holdDisplay(value)); }}
          helper={DISPLAY_HELPER[display]} />
      </SettingRow>}
    </div>)}
    <ChangeNote note={note} />
  </>;
}
