import { useState } from "react";
import { setCoordSettings, setHarnessDelivery, setHarnessMode } from "../../api";
import type { CoordHarness, CoordinationView, CoordSettings, Delivery, Overlaps, QuestionsTo, SubagentMode, TeamStatus } from "../../engine-client";
import { Button, Segmented, Select } from "../../ui";
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
  nudges: false,
  counted_tools: ["start_work", "inspect_work", "update_work", "transfer_work", "finish_work", "ctx_*", "graphify", "Skill"],
};

const HARNESS_NAMES: Record<CoordHarness, string> = { "claude-code": "Claude Code", codex: "Codex", rcli: "rcli" };

/** The Sub-agent mode choices; Native only for a harness kind with native teammates. */
export function modeOptions(native: boolean): SelectOption[] {
  const spawned = { value: "app_spawned", label: "App-spawned sub-agents" };
  return native ? [spawned, { value: "native", label: "Native teammates" }] : [spawned];
}

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
 * where sub-agent questions go, what overlapping claims do, workflow nudges and the counted
 * tools. Every option is selectable; the plan defaults show for an older engine. Inputs: the
 * project id, the latest team status and a reload. Output: the controls. */
export function CoordinationControls({ projectId, status, reload }: { projectId: string; status: TeamStatus; reload: () => void }) {
  const view = coordination(status);
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
        options={[{ value: "warn", label: "Warn" }, { value: "block", label: "Block" }, { value: "off", label: "Off" }]} />
      <span className="tg-terminal-note">Warn tells both agents and their leads when their claimed files overlap. Block also refuses the edit.</span>
    </div>
    <div className="pj-row">
      <span aria-hidden="true">Workflow nudges</span>
      <Segmented label="Workflow nudges" value={view.settings.nudges ? "on" : "off"} onValue={(value) => void save({ nudges: value === "on" }, "workflow nudges")}
        options={[{ value: "off", label: "Off" }, { value: "on", label: "On" }]} />
      <span className="tg-terminal-note">On sends an agent one reminder when it edits without start_work, or works 10 minutes without graphify or ctx tools.</span>
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
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
  </section>;
}
