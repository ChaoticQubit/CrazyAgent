import { useState } from "react";
import { listSkills, setCoordSettings, setHarnessDelivery, setHarnessMode, type SkillView } from "../../api";
import type { ClaudeDisplay, CoordHarness, CoordinationView, CoordSettings, Delivery, Nudges, Overlaps, QuestionsTo, SkillRole, SubagentMode, TaskStart, TeamStatus } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, Segmented, Select } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { changeNote } from "./controls";

export const COUNTED_TOOLS_MAX = 32;
const COUNTED_TOOL = /^[A-Za-z0-9_*-]{1,64}$/;

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

const HARNESS_NAMES: Record<CoordHarness, string> = { "claude-code": "Claude Code", codex: "Codex", rcli: "rcli" };

/** The Sub-agent mode choices; Native only for a harness kind with native teammates. */
export function modeOptions(native: boolean): SelectOption[] {
  const spawned = { value: "app_spawned", label: "App-spawned sub-agents" };
  return native ? [spawned, { value: "native", label: "Native teammates" }] : [spawned];
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

/** The coordination settings a team status holds, with the plan defaults for an engine that
 * sends none. Input: the status. Output: the settings and every harness kind's row. */
export function coordination(status: Pick<TeamStatus, "coordination">): CoordinationView {
  const view = status.coordination;
  const harnesses = COORD_HARNESSES.map((harness) => view?.harnesses.find((row) => row.harness === harness)
    ?? { harness, mode: "app_spawned" as const, delivery: "auto" as const, native: harness === "claude-code" });
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

/** Coordination settings on the Agents tab: each harness kind's sub-agent mode and delivery,
 * where sub-agent questions go, what overlapping claims do, workflow nudges, the Task-start
 * reminder, the counted tools and the skills each team role gets. Every option is selectable; the plan defaults show for
 * an older engine. Inputs: the project id, the latest team status and a reload. Output: the
 * controls. */
export function CoordinationControls({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const view = coordination(status);
  const [skills] = useLoad(() => listSkills(projectId), [projectId], "skills");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [counted, setCounted] = useState<string | null>(null);
  const countedText = counted ?? view.settings.counted_tools.join("\n");
  const countedCheck = countedToolsInput(countedText);

  async function run(change: () => Promise<Parameters<typeof changeNote>[0]>, what: string) {
    if (busy) return;
    setBusy(true);
    const message = changeNote(await change(), what);
    setBusy(false);
    setNote(message);
    reload();
  }

  const save = (patch: Partial<CoordSettings>, what: string) =>
    run(() => setCoordSettings(projectId, { ...view.settings, ...patch }), what);

  async function saveCounted() {
    if (countedCheck.kind !== "ok") return;
    await save({ counted_tools: countedCheck.tools }, "the counted tools");
    setCounted(null);
  }

  return <section className="tg-controls" aria-label="Coordination">
    <h4>Coordination</h4>
    {view.harnesses.map((row) => <div className="tg-limits" role="group" aria-label={HARNESS_NAMES[row.harness]} key={row.harness}>
      <Select id={`tg-mode-${row.harness}-${projectId}`} label={`${HARNESS_NAMES[row.harness]} sub-agents`} value={row.mode} options={modeOptions(row.native)}
        onValue={(value) => { if (value === "app_spawned" || (value === "native" && row.native)) void run(() => setHarnessMode(projectId, row.harness, value as SubagentMode), "the sub-agent mode"); }}
        helper={row.native ? "Native teammates run inside the lead's own session. App-spawned sub-agents get their own terminal, worktree and branch."
          : `${HARNESS_NAMES[row.harness]} has no native teammates, so its sub-agents are app-spawned.`} />
      <Select id={`tg-delivery-${row.harness}-${projectId}`} label={`Messages to ${HARNESS_NAMES[row.harness]} agents`} value={row.delivery} options={DELIVERY_OPTIONS}
        onValue={(value) => { if (value === "auto" || value === "native_only" || value === "typing_only") void run(() => setHarnessDelivery(projectId, row.harness, value as Delivery), "message delivery"); }} />
      {row.harness === "claude-code" && <Select id={`tg-display-claude-code-${projectId}`} label="Claude Code terminal display" value={view.settings.claude_display} options={DISPLAY_OPTIONS}
        onValue={(value) => { if (value === "classic" || value === "fullscreen") void save({ claude_display: value }, "the Claude Code terminal display"); }}
        helper={DISPLAY_HELPER[view.settings.claude_display]} />}
    </div>)}
    <div className="pj-row">
      <span aria-hidden="true">Sub-agent questions go to</span>
      <Segmented label="Sub-agent questions go to" value={view.settings.questions_to} onValue={(value: QuestionsTo) => void save({ questions_to: value }, "where questions go")}
        options={[{ value: "lead", label: "Their lead" }, { value: "me", label: "Me directly" }]} />
      <span className="tg-terminal-note">A question an agent marks For: Me always reaches you; no lead or master can answer it.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Overlapping edits</span>
      <Segmented label="Overlapping edits" value={view.settings.overlaps} onValue={(value: Overlaps) => void save({ overlaps: value }, "overlapping edits")}
        options={[{ value: "warn", label: "Warn" }, { value: "block", label: "Block" }, { value: "off", label: "Off" }, { value: "wait", label: "Wait" }]} />
      <span className="tg-terminal-note">Warn tells both agents and their leads when their claimed files overlap. Block also refuses the edit. Wait holds back an agent starting on a file another agent is on, and tells it when the file is free.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Workflow nudges</span>
      <Segmented label="Workflow nudges" value={view.settings.nudges} onValue={(value: Nudges) => void save({ nudges: value }, "workflow nudges")}
        options={[{ value: "off", label: "Off" }, { value: "ten_minutes", label: "Every 10 minutes" }, { value: "first_edit", label: "First edit" }]} />
      <span className="tg-terminal-note">Every 10 minutes sends an agent one reminder when it edits without start_work, or works 10 minutes without graphify or ctx tools. First edit reminds an agent once when it edits a file before any graphify query; the edit still runs.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Task-start reminder</span>
      <Segmented label="Task-start reminder" value={view.settings.task_start} onValue={(value: TaskStart) => void save({ task_start: value }, "the task-start reminder")}
        options={[{ value: "on", label: "On" }, { value: "off", label: "Off" }]} />
      <span className="tg-terminal-note">On adds a short reminder to the first turn of each task of every sub-agent and teammate with tools on: load the context-mode tools (Claude Code runs ToolSearch with their select string) and use the approved skills that fit. Off adds nothing. It is advice; nothing is blocked.</span>
    </div>
    <form className="pj-row" aria-label="Counted tools" onSubmit={(event) => { event.preventDefault(); void saveCounted(); }}>
      <label htmlFor={`tg-counted-${projectId}`}>Counted tools</label>
      <textarea id={`tg-counted-${projectId}`} className="pj-input" rows={3} value={countedText} onChange={(event) => setCounted(event.target.value)}
        aria-describedby={`tg-counted-help-${projectId}`} />
      <Button type="submit" size="small" disabled={busy || counted === null || countedCheck.kind !== "ok"} pending={busy && counted !== null} pendingLabel="Saving...">Save</Button>
      <Button type="button" size="small" disabled={busy} onClick={() => { setCounted(null); void save({ counted_tools: COORD_DEFAULTS.counted_tools }, "the counted tools"); }}>Use defaults</Button>
      <span className="tg-terminal-note" id={`tg-counted-help-${projectId}`}>{counted !== null && countedCheck.kind === "invalid" ? countedCheck.reason
        : "The Process page and the graph count these tool calls per agent. One name per line; * matches any ending, such as ctx_*."}</span>
    </form>
    <RoleSkillsControls settings={view.settings} skills={skills} busy={busy}
      onChoose={(role, chosen) => void save({ role_skills: { ...view.settings.role_skills, [role]: chosen } }, `the ${role} skills`)} />
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </section>;
}
