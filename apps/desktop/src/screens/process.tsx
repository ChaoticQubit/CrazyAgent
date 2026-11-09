import { useEffect, useRef, useState } from "react";
import { listSkills, type Result } from "../api";
import { loadTeamStatus } from "../api-projects";
import { type ImportFile, type TeamStatus } from "../engine-client";
import {
  DEFAULT_INSTRUCTION_SETTINGS, afterSettingsSave, EDIT_DELIVERIES, INSTRUCTION_MODES, PROCESS_TEXT_BYTES_MAX, commitInstructionImport, committedMessage, confirmInstructionImport,
  defaultProcess, flagWords, importInstructionsFile, importRefusal, loadAdoption, loadInstructionImport, loadUncommittedImport, loadInstructionSettings, loadGivenText, loadProcess, processMessage,
  processTextError, saveInstructionSettings, saveProcess, savedMessage, settingsSaveVersion, skillSegments, textBytes, type Adoption, type AgentAdoption, type InstructionMode,
  type InstructionSettings, type InstructionView, type SavedText, type SkillEntry, type EditDelivery,
} from "../api-process";
import { sameJson, useAppSession, useHeld, useLoad, type Loaded } from "../state";
import { Button, ConfirmDialog, Dialog, EmptyState, Icon, LoadError, NotReady, Notice, Panel, Segmented, Skeleton, StatusBadge, Switch, TextArea } from "../ui";
import { navigate } from "../ui/router";
import { displayText } from "./conversation";
import { ConflictView, keepMine, type Conflict } from "./conflict";
import { SkillTag, skillEntries } from "./skill-tag";
import { HARNESS_NAMES } from "./team/coordination";
import "./process.css";

/** Where an instructions text applies: every project, or one project. */
export type ProcessScope = { kind: "app" } | { kind: "project"; id: string; name: string };

/** How a project's Process page shows its instructions: as agents read them, or the editor. */
export type InstructionsView = "read" | "edit";

const INSTRUCTION_VIEWS: { value: InstructionsView; label: string }[] = [
  { value: "read", label: "As agents read it" },
  { value: "edit", label: "Edit" },
];

const FOLD_LINES = 12;

const MODE_SUBTITLES: Record<InstructionMode, string> = {
  both: "App default plus this project's file",
  project_only: "This project's file only",
  app_only: "App default only",
};

const TASK_STEPS = [
  "Master writes the task and picks an agent.",
  "Agent reads instructions, asks the graph, works on its branch.",
  "Reviewer checks the change set.",
  "The master merges it, or it waits for your accept when Ask me before merging is on. At Full access a master change set that merges in another can also land by itself.",
];

/** How many lines a text shows, a final line break adding none. Input: the text. Output: the
 * count, 0 for an empty text. */
export function lineCount(text: string): number {
  const body = text.replace(/\n+$/u, "");
  return body === "" ? 0 : body.split("\n").length;
}

/** The line a project's editor shows on when running agents get an edit. Input: the edit
 * delivery, undefined for the default. Output: the line. */
export function editNote(edits: EditDelivery | undefined): string {
  const delivery = EDIT_DELIVERIES.find((option) => option.value === edits) ?? EDIT_DELIVERIES[0];
  return delivery.value === "next_message" ? "Running agents get the change at their next task." : delivery.note;
}

/** A seed text the owner opened for a preview before it goes into the editor. */
export interface ProcessPreview {
  source: "import" | "default";
  text: string;
}

/** Shows instructions text with each catalog skill name as a tag: hovering shows its one-line
 * description and clicking opens that skill in Settings, or hands it to `onOpen` when the
 * page decides first. Inputs: the text, the catalog and the optional opener. */
export function TaggedText({ text, skills, onOpen }: { text: string; skills: readonly SkillEntry[]; onOpen?: (skill: SkillEntry) => void }) {
  return <div className="process-text">
    {skillSegments(text, skills).map((segment, index) => segment.kind === "text"
      ? <span key={index}>{segment.text}</span>
      : <SkillTag key={index} name={segment.name} skills={[segment]} onOpen={onOpen} />)}
  </div>;
}

/** The draft to show once the saved text is read again: the new text while the owner left
 * the last read text untouched, else the owner's unsaved draft, so no reply or save made
 * elsewhere replaces typing. Inputs: the draft, the text last read (null before the first
 * read) and the text just read. Output: the draft. */
export function keptDraft(draft: string, loaded: string | null, saved: string): string {
  return loaded === null || draft === loaded ? saved : draft;
}

/** Writes a byte count the way the Process pages show sizes. Input: bytes. Output: the words. */
export function byteSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} bytes` : `${(bytes / 1024).toFixed(1)} KB`;
}

/** The conflict a stale instructions save shows: the version the save named, the text that
 * landed with its version, and who saved it. Input: the stale reply. Output: the conflict. */
export function conflictOf(stale: Extract<SavedText, { kind: "stale" }>): Conflict<string> {
  return { opened: stale.version, version: stale.current.version, current: stale.current.text, writer: stale.writer };
}

/** The Process page as a pure view: the editor with Save, the text with skill tags, and at
 * app level the two seeds, each opening a preview first. After a stale save it shows the
 * conflict, whose Reload and Keep mine are then the only ways on. Inputs: where it applies,
 * the saved text, the draft, the skill catalog, the last failure, what is running, the
 * preview, the conflict, the skill the owner asked to open over unsaved text and the
 * actions. The editor is read-only while saving. Nothing reads the owner's files or saves
 * without the matching click. */
export function ProcessView({ scope, saved, draft, skills, failure, busy, preview, conflict, leaving = null, view = "read", all = false, given = { kind: "loading" }, mode = "both", subtitle = MODE_SUBTITLES.both, editNote: note = "",
  onDraft, onSave, onImport, onDefault, onUsePreview, onClosePreview, onRetry, onReload, onKeepMine, onOpenSkill, onLeave, onStay, onView, onAll, onUseDefault }: {
  scope: ProcessScope;
  saved: Loaded<string>;
  draft: string;
  skills: readonly SkillEntry[];
  failure: string;
  busy: "save" | "import" | "default" | null;
  preview: ProcessPreview | null;
  conflict: Conflict<string> | null;
  leaving?: SkillEntry | null;
  view?: InstructionsView;
  all?: boolean;
  given?: Loaded<string>;
  mode?: InstructionMode | null;
  subtitle?: string;
  editNote?: string;
  onDraft: (text: string) => void;
  onSave: () => void;
  onImport: () => void;
  onDefault: () => void;
  onUsePreview: () => void;
  onClosePreview: () => void;
  onRetry: () => void;
  onReload: () => void;
  onKeepMine: () => void;
  onOpenSkill?: (skill: SkillEntry) => void;
  onLeave?: () => void;
  onStay?: () => void;
  onView?: (view: InstructionsView) => void;
  onAll?: () => void;
  onUseDefault?: () => void;
}) {
  const intro = scope.kind === "app"
    ? "Instructions every master and sub-agent gets, in every project. A project's own instructions come after these."
    : `Instructions every agent in ${scope.name} gets, after the app-wide ones. Kept in the project's .crazyagent/instructions.md, which git ignores.`;
  if (saved.kind === "loading") return <Skeleton shape="card" count={2} />;
  if (saved.kind === "not_ready") return <NotReady what="Instructions" />;
  if (saved.kind === "error") return <LoadError message={saved.message} onRetry={onRetry} />;
  const invalid = processTextError(draft)
    ?? (scope.kind === "project" && mode === "project_only" && draft.trim() === "" ? "Under Project only an empty file leaves agents with no instructions." : undefined);
  const changed = draft !== saved.value;
  const leave = <ConfirmDialog open={leaving !== null} title={`Open ${leaving?.name ?? "the skill"} without saving?`} confirmLabel="Open skill"
    cancelLabel="Keep editing" onConfirm={() => onLeave?.()} onCancel={() => onStay?.()}>
    Your instructions changes will be lost.
  </ConfirmDialog>;
  if (scope.kind === "project") {
    const editing = view === "edit" || Boolean(conflict);
    const lines = given.kind === "ok" ? lineCount(given.value) : 0;
    const read = given.kind === "loading" ? <p className="small muted" role="status">Reading what agents get...</p>
      : given.kind !== "ok" ? <Notice tone="danger" alert>{"What agents get could not be read. "}{processMessage(given)}</Notice>
      : given.value.trim() === "" ? <Notice>Agents get no instructions from you now.</Notice>
      : <>
        <div className={`pr-instr${all ? " all" : ""}`}><TaggedText text={given.value} skills={skills} onOpen={onOpenSkill} /></div>
        {lines > FOLD_LINES && <span><button type="button" className="pr-link" onClick={() => onAll?.()}>{all ? "Show less" : `Show all ${lines} lines`}</button></span>}
      </>;
    return <div className="stack">
      {failure && <Notice tone="danger" alert>{failure}</Notice>}
      {conflict && <ConflictView conflict={conflict} busy={busy === "save"} onReload={onReload} onKeepMine={onKeepMine}>
        <TaggedText text={conflict.current} skills={skills} />
      </ConflictView>}
      <Panel title={<div className="pr-head"><h2>Instructions</h2><p className="small muted">{subtitle}{given.kind === "ok" ? ` · ${lines} lines` : ""}</p></div>}
        actions={<Segmented label="How the instructions show" value={editing ? "edit" : "read"} options={INSTRUCTION_VIEWS} onValue={(next) => onView?.(next)} />}>
        {editing
          ? <form className="stack-small" onSubmit={(event) => { event.preventDefault(); if (!invalid && changed && !conflict) onSave(); }}>
            <TextArea id="process-project" label="Instructions for agents" value={draft} onValue={onDraft} rows={16} error={invalid} readOnly={busy === "save"}
              helper={`${byteSize(textBytes(draft))} of ${byteSize(PROCESS_TEXT_BYTES_MAX)}${changed ? ". Not saved yet." : ""}`} />
            <span className="row">
              <Button type="submit" variant="primary" icon="check" pending={busy === "save"} pendingLabel="Saving..."
                disabled={Boolean(invalid) || !changed || busy !== null || Boolean(conflict)}>Save</Button>
              {changed && !conflict && <Button disabled={busy !== null} onClick={() => onDraft(saved.value)}>Discard edits</Button>}
              {mode === "project_only"
                ? <span className="small muted">Project only gives agents this file alone. To use the app default, pick App and project or App only under Delivery.</span>
                : mode !== null && <Button disabled={busy !== null || draft === ""} onClick={() => onUseDefault?.()}>Use app default</Button>}
              <span className="small muted">{note}</span>
            </span>
          </form>
          : <div className="stack-small">{read}</div>}
      </Panel>
      {leave}
    </div>;
  }
  const seeds = scope.kind === "app" && <span className="row">
    <Button icon="upload" pending={busy === "import"} pendingLabel="Reading..." disabled={busy !== null} onClick={onImport}>Import from file</Button>
    <Button icon="sparkles" pending={busy === "default"} pendingLabel="Reading..." disabled={busy !== null} onClick={onDefault}>Use the CrazyAgent default process</Button>
  </span>;
  return <div className="stack">
    <p className="secondary">{intro}</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {conflict && <ConflictView conflict={conflict} busy={busy === "save"} onReload={onReload} onKeepMine={onKeepMine}>
      <TaggedText text={conflict.current} skills={skills} />
    </ConflictView>}
    {seeds}
    <Panel title="Instructions">
      <form className="stack-small" onSubmit={(event) => { event.preventDefault(); if (!invalid && changed && !conflict) onSave(); }}>
        <TextArea id={`process-${scope.kind}`} label="Instructions for agents" value={draft} onValue={onDraft} rows={16} error={invalid} readOnly={busy === "save"}
          helper={`${byteSize(textBytes(draft))} of ${byteSize(PROCESS_TEXT_BYTES_MAX)}${changed ? ". Not saved yet." : ""}`} />
        <span className="row">
          <Button type="submit" variant="primary" icon="check" pending={busy === "save"} pendingLabel="Saving..."
            disabled={Boolean(invalid) || !changed || busy !== null || Boolean(conflict)}>Save</Button>
          {changed && !conflict && <Button disabled={busy !== null} onClick={() => onDraft(saved.value)}>Discard edits</Button>}
        </span>
      </form>
    </Panel>
    {draft.trim() && <Panel title="As agents read it"><TaggedText text={draft} skills={skills} onOpen={onOpenSkill} /></Panel>}
    <Dialog open={preview !== null} wide title={preview?.source === "import" ? "Instructions from your file" : "CrazyAgent default process"}
      onClose={onClosePreview}
      footer={<>
        <Button onClick={onClosePreview}>Cancel</Button>
        <Button variant="primary" icon="check" onClick={onUsePreview}>Put in the editor</Button>
      </>}>
      <p className="secondary">{preview?.source === "import"
        ? "Read once from the file you picked; afterwards the instructions live here. It replaces the editor's text; nothing is saved until you click Save."
        : "It replaces the editor's text; nothing is saved until you click Save."}</p>
      {preview && <TaggedText text={preview.text} skills={skills} onOpen={onOpenSkill} />}
    </Dialog>
    {leave}
  </div>;
}

/** A project's agent settings as a pure view: which of the app and project instructions its
 * agents get and whether headless Claude runs keep their transcript, each saved on its own
 * change. Settings that could not be read say why and show the defaults, so a change replaces
 * them. A pick stays on screen until a read moves the setting or its save is refused. Inputs: the
 * read, whether a save runs, the last save failure and the save action, which gives the failure,
 * empty once saved. */
export function AgentSettingsView({ settings, busy, failure, onSave }: {
  settings: Loaded<InstructionView>;
  busy: boolean;
  failure: string;
  onSave: (settings: InstructionSettings) => Promise<string>;
}) {
  const [current, hold] = useHeld<InstructionSettings>(settings.kind === "ok" ? settings.value.settings : DEFAULT_INSTRUCTION_SETTINGS, sameJson);
  if (settings.kind === "loading") return <Skeleton shape="card" count={1} />;
  if (settings.kind === "not_ready") return <NotReady what="Agent settings" />;
  const choice = INSTRUCTION_MODES.find((option) => option.value === current.mode) ?? INSTRUCTION_MODES[0];
  const delivery = EDIT_DELIVERIES.find((option) => option.value === current.edits) ?? EDIT_DELIVERIES[0];
  const save = (next: InstructionSettings) => {
    if (busy) return;
    const settled = hold(next);
    void onSave(next).then(settled);
  };
  return <Panel title="Delivery" padded={false}>
    {(settings.kind === "error" || failure) && <div className="pr-del-notes">
      {settings.kind === "error" && <Notice tone="danger" alert>{processMessage(settings)}</Notice>}
      {failure && <Notice tone="danger" alert>{failure}</Notice>}
    </div>}
    <div className="pr-del-row">
      <span className="pr-del-label">Instructions agents get</span>
      <Segmented label="Instructions agents get" value={choice.value} describedBy="agent-instructions-note"
        options={INSTRUCTION_MODES.map(({ value, label }) => ({ value, label }))}
        onValue={(mode) => save({ ...current, mode })} />
      <span className="small muted pr-del-note" id="agent-instructions-note">{choice.note}</span>
    </div>
    <div className="pr-del-row">
      <span className="pr-del-label">When you edit instructions</span>
      <Segmented label="When you edit instructions" value={delivery.value} describedBy="agent-edits-note"
        options={EDIT_DELIVERIES.map(({ value, label }) => ({ value, label }))}
        onValue={(edits) => save({ ...current, edits })} />
      <span className="small muted pr-del-note" id="agent-edits-note">{delivery.note}</span>
    </div>
    <div className="pr-del-row pr-del-toggle">
      <span className="pr-del-label"><strong>Keep transcripts</strong><span className="small muted">For headless Claude runs</span></span>
      <Switch id="agent-transcripts" label="Keep transcripts for headless Claude runs" hideLabel checked={current.transcripts}
        describedBy="agent-transcripts-note" disabledReason={busy ? "Saving..." : undefined}
        onChecked={(transcripts) => save({ ...current, transcripts })} />
      <span className="small muted pr-del-note" id="agent-transcripts-note">{current.transcripts ? "Kept in the agent's own home" : "No transcript kept"}</span>
    </div>
  </Panel>;
}

/** A project's agent settings: reads them from the engine and saves each change at once at
 * the version read; a save refused because they changed since, or one the engine could not
 * confirm, reads them again and says so, as `afterSettingsSave` decides. Inputs: the project id
 * and what to do after a save landed. */
export function AgentSettings({ projectId, onSaved }: { projectId: string; onSaved?: () => void }) {
  const session = useAppSession();
  const [settings, reload] = useLoad(() => loadInstructionSettings(projectId), [projectId]);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const store = async (next: InstructionSettings): Promise<string> => {
    setFailure("");
    setBusy(true);
    const result = session.noteResult(await saveInstructionSettings(projectId, next, settingsSaveVersion(settings)));
    setBusy(false);
    if (result.kind === "ok") {
      session.showToast("Agent settings saved. Agents started from now on follow them.");
      onSaved?.();
    }
    const after = afterSettingsSave(result);
    if (after.reload) reload();
    setFailure(after.failure);
    return after.failure;
  };
  return <AgentSettingsView settings={settings} busy={busy} failure={failure} onSave={store} />;
}

/** One agent's grounding and skill counts: graphify, ctx_*, then every skill its project
 * enabled by name with zero included, then the other skills it called. Inputs: the view's
 * skill names and the agent's row. Output: the list. */
export function SkillCounts({ skills, agent }: { skills: readonly string[]; agent: AgentAdoption }) {
  const counts = agent.skills ?? [];
  const items = [
    { name: "graphify", count: agent.graphify ?? 0, note: "" },
    { name: "ctx_*", count: agent.context_mode ?? 0, note: "" },
    ...skills.map((name, index) => ({ name, count: counts[index] ?? 0, note: "" })),
    ...(agent.other_skills ?? []).map((other) => ({ name: other.name, count: other.count, note: " (not enabled)" })),
  ];
  return <span className="adoption-skills">
    {items.map((item) => <span key={`${item.name}${item.note}`} className={item.count === 0 ? "secondary" : undefined}>
      {item.name}{item.note} <strong>{item.count}</strong>
    </span>)}
    {skills.length === 0 && <span className="secondary">No skills enabled</span>}
  </span>;
}

/** One agent's skill counts as a pure view of its project's adoption read: that agent's
 * graphify, ctx_* and per-skill counts, or why there are none. Inputs: the read, the agent's
 * team row id and the retry action. */
export function AgentSkills({ adoption, agent, onRetry }: { adoption: Loaded<Adoption>; agent: number; onRetry: () => void }) {
  const row = adoption.kind === "ok" ? adoption.value.agents.find((item) => item.agent === agent) : undefined;
  return adoption.kind === "loading" ? <Skeleton shape="card" count={1} />
    : adoption.kind === "not_ready" ? <NotReady what="Skill counts" />
    : adoption.kind === "error" ? <LoadError message={processMessage(adoption)} onRetry={onRetry} />
    : !row ? <p className="secondary">This agent is older than the agents the Process page lists, so its counts are not shown.</p>
    : <SkillCounts skills={adoption.value.skills ?? []} agent={row} />;
}

/** One agent's skill counts for its graph card menu: reads the project's adoption and shows
 * that agent's counts, again on Refresh. Inputs: the project, the agent's team row id and its
 * label, and the close action. */
export function AgentSkillsDialog({ projectId, agent, label, onClose }: { projectId: string; agent: number; label: string; onClose: () => void }) {
  const [adoption, reload] = useLoad(() => loadAdoption(projectId), [projectId]);
  return <Dialog open title={`Skills of ${label}`} onClose={onClose}
    footer={<><Button icon="refresh-cw" onClick={reload}>Refresh</Button><Button variant="primary" onClick={onClose}>Close</Button></>}>
    <AgentSkills adoption={adoption} agent={agent} onRetry={reload} />
  </Dialog>;
}

/** Whether an agent asked the graph before its edits: missed when it edited without
 * start_work, yes once it queried the graph, else not yet. Input: the agent's row. */
function graphState(agent: AgentAdoption): "yes" | "missed" | "none" {
  if (agent.flags.includes("edits_without_start_work")) return "missed";
  return (agent.graphify ?? 0) > 0 ? "yes" : "none";
}

/** Whether an agent reads through the context tools: missed when it went 10 minutes without
 * graphify or context-mode, yes once it used ctx_*, else not yet. Input: the agent's row. */
function contextState(agent: AgentAdoption): "yes" | "missed" | "none" {
  if (agent.flags.includes("no_graphify_or_context_mode")) return "missed";
  return (agent.context_mode ?? 0) > 0 ? "yes" : "none";
}

function Check({ state }: { state: "yes" | "missed" | "none" }) {
  if (state === "none") return <span className="secondary">Not yet</span>;
  return <span className={`pr-check ${state}`}><Icon name={state === "yes" ? "circle-check" : "circle-x"} size={14} />{state === "yes" ? "Yes" : "Missed"}</span>;
}

/** The skills an agent used: its project's enabled skills it called, then the others it
 * called. Inputs: the view's skill names and the agent's row. Output: "tdd, other-one" or
 * "None". */
function skillsUsed(skills: readonly string[], agent: AgentAdoption): string {
  const counts = agent.skills ?? [];
  const used = [
    ...skills.filter((_, index) => (counts[index] ?? 0) > 0),
    ...(agent.other_skills ?? []).filter((other) => other.count > 0).map((other) => other.name),
  ];
  return used.length === 0 ? "None" : used.join(", ");
}

/** Who follows a project's instructions, as a pure view: one row per agent with whether it
 * asked the graph before its edits and used the context tools, the skills it used and its
 * flags, marked once it was reminded. A clicked row shows the agent's counts; stopped agents
 * wait behind Show finished. Inputs: the adoption read, the agents' names by id, whether
 * finished agents show, the open row and the actions. */
export function FollowingView({ adoption, names, finished, expanded, onFinished, onExpand, onMetrics, onRetry }: {
  adoption: Loaded<Adoption>;
  names: Map<number, string>;
  finished: boolean;
  expanded: number | null;
  onFinished: () => void;
  onExpand: (agent: number | null) => void;
  onMetrics: () => void;
  onRetry: () => void;
}) {
  const head = <div className="pr-head"><h2>Are agents following it</h2><p className="small muted">One row per agent. Click a row for details. Counts are in Metrics.</p></div>;
  const open = <Button size="small" icon="trending-up" onClick={onMetrics}>Open Metrics</Button>;
  if (adoption.kind === "loading") return <Panel title={head} actions={open}><Skeleton shape="row" count={3} /></Panel>;
  if (adoption.kind === "not_ready") return <NotReady what="Workflow adoption" />;
  if (adoption.kind === "error") return <Panel title={head} actions={open}><LoadError message={processMessage(adoption)} onRetry={onRetry} /></Panel>;
  const { counted, agents } = adoption.value;
  const skills = adoption.value.skills ?? [];
  const hidden = adoption.value.hidden ?? 0;
  const stopped = agents.filter((agent) => !agent.active).length;
  const shown = agents.filter((agent) => agent.active || finished);
  return <Panel title={head} actions={open} padded={false}>
    {agents.length === 0
      ? <EmptyState compact icon="users" title="No agents yet">Counts appear once this project's agents use tools.</EmptyState>
      : <div className="pj-table-wrap pr-table-wrap"><table className="pj-table pr-table">
        <thead><tr><th scope="col">Agent</th><th scope="col">Graph before edits</th><th scope="col">Context tools</th><th scope="col">Skills used</th><th scope="col">Flags</th></tr></thead>
        <tbody>{shown.flatMap((agent) => {
          const name = names.get(agent.agent) ?? (agent.role === "master" ? "Master" : `Agent ${agent.agent}`);
          const isOpen = expanded === agent.agent;
          const row = <tr key={agent.agent} className={agent.active ? undefined : "adoption-stopped"}>
            <th scope="row"><button type="button" className="pr-link pr-agent" aria-expanded={isOpen} onClick={() => onExpand(isOpen ? null : agent.agent)}>{name}</button>
              {" "}<span className="muted small">{HARNESS_NAMES[agent.harness as keyof typeof HARNESS_NAMES] ?? agent.harness}</span></th>
            <td><Check state={graphState(agent)} /></td>
            <td><Check state={contextState(agent)} /></td>
            <td className="secondary">{skillsUsed(skills, agent)}</td>
            <td><span className="adoption-flags">{agent.flags.length === 0
              ? <span className="secondary">None</span>
              : agent.flags.map((flag) => <span key={flag}><StatusBadge tone="danger" word={flagWords(flag)} />
                {agent.reminded.includes(flag) && <span className="small muted"> reminded</span>}</span>)}</span></td>
          </tr>;
          if (!isOpen) return [row];
          return [row, <tr key={`${agent.agent}-detail`} className="pr-expand"><td colSpan={5}>
            <span className="adoption-skills">{counted.map((tool, index) => <span key={tool}>{tool} <strong>{agent.counts[index] ?? 0}</strong></span>)}</span>
            <SkillCounts skills={skills} agent={agent} />
          </td></tr>];
        })}</tbody>
      </table>
      {(stopped > 0 || hidden > 0) && <p className="secondary adoption-note">
        {stopped > 0 && <button type="button" className="pr-link" onClick={onFinished}>{finished ? "Hide finished" : `Show finished (${stopped})`}</button>}
        {stopped > 0 && hidden > 0 && " "}
        {hidden > 0 && `${hidden} older ${hidden === 1 ? "agent is" : "agents are"} not listed.`}
      </p>}
      </div>}
  </Panel>;
}

/** Who follows a project's instructions: reads its workflow adoption and its agents' names
 * from the engine, and keeps which finished agents and which row show. Input: the project id. */
export function ProjectFollowing({ projectId }: { projectId: string }) {
  const [adoption, reload] = useLoad(() => loadAdoption(projectId), [projectId]);
  const [status] = useLoad<TeamStatus>(() => loadTeamStatus(projectId), [projectId]);
  const [finished, setFinished] = useState(false);
  const [expanded, setExpanded] = useState<number | null>(null);
  const names = new Map(status.kind === "ok" ? status.value.agents.map((agent) => [agent.id, displayText(agent.label)]) : []);
  return <FollowingView adoption={adoption} names={names} finished={finished} expanded={expanded}
    onFinished={() => setFinished((old) => !old)} onExpand={setExpanded} onRetry={reload}
    onMetrics={() => navigate({ name: "project", id: projectId, tab: "metrics" })} />;
}

/** How a task runs on a project, in four steps, beside its Process page. */
export function TaskSteps() {
  return <Panel>
    <h3 className="pr-steps-title">How a task runs</h3>
    {TASK_STEPS.map((step, index) => <div key={step} className="pr-step"><span className="pr-n">{index + 1}</span><span>{step}</span></div>)}
  </Panel>;
}

/** The skill catalog the tags name, from the engine's skill list. Input: the read, whose
 * skills carry the engine's one-line description as `line` where the engine reports it.
 * Output: each skill's id, name and one-line description, empty while unread or failed. */
export function catalogOf(skills: Loaded<{ id: string; name: string; description: string; line?: string }[]>): SkillEntry[] {
  return skills.kind === "ok" ? skillEntries(skills.value) : [];
}

/** The Process page for one scope: reads the saved text and the skill catalog from the
 * engine, keeps the owner's draft, which no later read replaces while it holds unsaved
 * edits, and saves only on Save. A stale save shows the conflict and keeps the draft until
 * the owner picks Reload or Keep mine. A skill link over unsaved text asks first. The seeds
 * read the owner's global file or the default text only on their click, into a preview. A
 * project's page also shows its agent settings. Input: where it applies. */
export function ProcessEditor({ scope }: { scope: ProcessScope }) {
  const session = useAppSession();
  const projectId = scope.kind === "project" ? scope.id : null;
  const [loaded, reload] = useLoad(() => loadProcess(projectId), [projectId]);
  const saved: Loaded<string> = loaded.kind === "ok" ? { kind: "ok", value: loaded.value.text } : loaded;
  const [skills] = useLoad(() => listSkills(projectId ?? undefined), [projectId]);
  const [given, reloadGiven] = useLoad<string>(() => projectId === null ? Promise.resolve({ kind: "ok", value: "" }) : loadGivenText(projectId), [projectId]);
  const [settings, readSettings] = useLoad<InstructionView | null>(() => projectId === null ? Promise.resolve({ kind: "ok", value: null }) : loadInstructionSettings(projectId), [projectId]);
  const [view, setView] = useState<InstructionsView>("read");
  const [all, setAll] = useState(false);
  const [draft, setDraft] = useState("");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<"save" | "import" | "default" | null>(null);
  const [preview, setPreview] = useState<ProcessPreview | null>(null);
  const [conflict, setConflict] = useState<Conflict<string> | null>(null);
  const [leaving, setLeaving] = useState<SkillEntry | null>(null);
  const lastRead = useRef<string | null>(null);
  const savedText = saved.kind === "ok" ? saved.value : null;
  useEffect(() => {
    if (savedText === null) return;
    const before = lastRead.current;
    lastRead.current = savedText;
    if (conflict === null) setDraft((current) => keptDraft(current, before, savedText));
  }, [savedText]);
  const open = (skill: SkillEntry) => navigate({ name: "settings", tab: "plugins", skill: skill.id });
  const openSkill = (skill: SkillEntry) => {
    setPreview(null);
    if (savedText !== null && draft !== savedText) setLeaving(skill);
    else open(skill);
  };

  const fail = (result: Result<unknown>) => { if (result.kind !== "ok") setFailure(processMessage(result)); };
  const seed = async (source: "import" | "default") => {
    setFailure("");
    setBusy(source);
    const result = session.noteResult(await (source === "import" ? importInstructionsFile() : defaultProcess()));
    setBusy(null);
    if (result.kind !== "ok") fail(result);
    else if (result.value !== null) setPreview({ source, text: result.value });
  };
  const save = async (shown: Conflict<string> | null) => {
    if (loaded.kind !== "ok") return;
    setFailure("");
    setBusy("save");
    const named = shown ? keepMine(shown) : { version: loaded.value.version, opened: null };
    const result = session.noteResult(await saveProcess(projectId, draft, named.version, named.opened));
    setBusy(null);
    if (result.kind !== "ok") fail(result);
    else if (result.value.kind === "stale") setConflict(conflictOf(result.value));
    else {
      setConflict(null);
      session.showToast(savedMessage(result.value.size));
      reload();
      reloadGiven();
    }
  };
  const reloadConflict = () => {
    if (conflict) setDraft(conflict.current);
    setConflict(null);
    setFailure("");
    reload();
    reloadGiven();
  };
  const current = settings.kind === "ok" && settings.value !== null ? settings.value.settings : DEFAULT_INSTRUCTION_SETTINGS;
  const reloadSettings = () => { readSettings(); reloadGiven(); };
  const mode = settings.kind === "ok" && settings.value !== null ? settings.value.settings.mode : null;
  const editor = <ProcessView scope={scope} saved={saved} draft={draft} skills={catalogOf(skills)} failure={failure} busy={busy} preview={preview} conflict={conflict}
    view={view} all={all} given={given} mode={mode} subtitle={MODE_SUBTITLES[current.mode]} editNote={editNote(current.edits)}
    onView={setView} onAll={() => setAll((old) => !old)} onUseDefault={() => setDraft("")}
    onDraft={setDraft} onSave={() => void save(null)} onImport={() => void seed("import")} onDefault={() => void seed("default")}
    onUsePreview={() => { if (preview) setDraft(preview.text); setPreview(null); }} onClosePreview={() => setPreview(null)} onRetry={reload}
    onReload={reloadConflict} onKeepMine={() => void save(conflict)}
    leaving={leaving} onOpenSkill={openSkill} onLeave={() => { if (leaving) open(leaving); setLeaving(null); }} onStay={() => setLeaving(null)} />;
  if (scope.kind === "app") return editor;
  return <div className="pr">
    <div className="stack">
      {editor}
      <ProjectFollowing projectId={scope.id} />
    </div>
    <aside className="pr-side">
      <AgentSettings projectId={scope.id} onSaved={reloadSettings} />
      <TaskSteps />
    </aside>
  </div>;
}

/** The found instruction files of a project waiting for the owner's one-time import, as a
 * pure view: each file with its size, a note when git tracks it (so it shows as deleted after
 * the move) or why it stays, and the confirm and Not now actions. Inputs: the project name,
 * the files, whether the import runs, the last failure and the actions. */
export function InstructionImportView({ name, files, busy, failure, onConfirm, onLater }: {
  name: string;
  files: readonly ImportFile[];
  busy: boolean;
  failure: string;
  onConfirm: () => void;
  onLater: () => void;
}) {
  const moving = files.filter((file) => file.refused === null);
  const tracked = moving.some((file) => file.tracked);
  return <Dialog open title={`Instruction files found in ${name}`} busy={busy} onClose={onLater}
    footer={<>
      <Button disabled={busy} onClick={onLater}>Not now</Button>
      <Button variant="primary" icon="check" pending={busy} pendingLabel="Moving..." disabled={moving.length === 0} onClick={onConfirm}>Copy into instructions.md and move aside</Button>
    </>}>
    <div className="stack-small">
      <p className="secondary">Agents now get instructions only from the app. These files can be copied once into this project's .crazyagent/instructions.md and moved to .crazyagent/imported/. Nothing changes until you confirm; Not now asks again the next time you open the project.</p>
      {failure && <Notice tone="danger" alert>{failure}</Notice>}
      <ul className="instruction-import">{files.map((file) => <li key={file.path}>
        <strong>{file.path}</strong> <span className="secondary">{byteSize(file.bytes)}{file.tracked ? ", tracked by git" : ""}</span>
        {file.refused !== null && <span className="secondary"> Stays in place: {importRefusal(file.refused)}</span>}
      </li>)}</ul>
      {tracked && <Notice tone="warn">Git tracks some of these files, so after the move your checkout shows them as deleted.</Notice>}
    </div>
  </Dialog>;
}

/** The choice offered while moved git-tracked instruction files still show as deleted, as a
 * pure view: names the files and, when known, the checked-out branch merges into are refused,
 * says what the commit holds and offers Commit the move and, when given, Leave it. Inputs: the
 * paths, the branch, whether the commit runs, the last failure and the actions. */
export function ImportMoveView({ files, branch, busy, failure, onCommit, onLeave }: {
  files: readonly string[];
  branch?: string | null;
  busy: boolean;
  failure: string;
  onCommit: () => void;
  onLeave?: () => void;
}) {
  const one = files.length === 1;
  return <Notice tone="warn">
    <span className="import-move">
      <span>Git still shows {files.join(", ")} as deleted, and merges into {branch == null ? "this branch" : displayText(branch)} are refused until the move is committed. Commit the move makes one local commit on your current branch holding only {one ? "this deletion" : "these deletions"}; it is never pushed.</span>
      {failure && <span className="import-move-failure" role="alert">{failure}</span>}
      <span className="import-move-actions">
        {onLeave && <Button disabled={busy} onClick={onLeave}>Leave it</Button>}
        <Button variant="primary" icon="check" pending={busy} pendingLabel="Committing..." onClick={onCommit}>Commit the move</Button>
      </span>
    </span>
  </Notice>;
}

/** Commits an instruction import's move on the owner's click and shows the result: a toast
 * naming the branch and files, or the refusal in place. Inputs: the project id, the paths,
 * the checked-out branch when known, what to do after a commit and, when the owner may
 * decline, Leave it. */
export function ImportMoveCommit({ projectId, files, branch, onCommitted, onLeave }: {
  projectId: string;
  files: readonly string[];
  branch?: string | null;
  onCommitted: () => void;
  onLeave?: () => void;
}) {
  const session = useAppSession();
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const commit = async () => {
    setFailure("");
    setBusy(true);
    const result = session.noteResult(await commitInstructionImport(projectId));
    setBusy(false);
    if (result.kind !== "ok") {
      setFailure(processMessage(result));
      if (result.kind === "error" && result.code === "process_commit_none") onCommitted();
      return;
    }
    session.showToast(committedMessage(result.value));
    onCommitted();
  };
  return <ImportMoveView files={files} branch={branch} busy={busy} failure={failure} onCommit={() => void commit()} onLeave={onLeave} />;
}

/** The project page banner shown while an instruction import's move stays uncommitted: it
 * names the files and the checked-out branch and offers Commit the move. Inputs: the project id and a version that a
 * finished import bumps so the list is read again. */
export function ImportMoveBanner({ projectId, version }: { projectId: string; version: number }) {
  const [waiting, reload] = useLoad(() => loadUncommittedImport(projectId), [projectId, version]);
  if (waiting.kind !== "ok" || waiting.value.files.length === 0) return null;
  return <ImportMoveCommit projectId={projectId} files={waiting.value.files} branch={waiting.value.branch} onCommitted={reload} />;
}

/** Asks once per opening of a project to import its found instruction files: reads the
 * pending list, shows it when not empty and copies and moves on confirm. When a moved file
 * is git-tracked and its move is uncommitted, it then offers Commit the move or Leave it;
 * otherwise it lists the moved files in a toast. Not now closes it until the project is
 * opened again. Inputs: the project id and name, and what to do once the import finished. */
export function InstructionImportPrompt({ projectId, name, onMoved }: { projectId: string; name: string; onMoved?: () => void }) {
  const session = useAppSession();
  const [pending] = useLoad(() => loadInstructionImport(projectId), [projectId]);
  const [closed, setClosed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [moved, setMoved] = useState<readonly ImportFile[] | null>(null);
  useEffect(() => { setClosed(false); setFailure(""); setMoved(null); }, [projectId]);
  const finish = () => { setClosed(true); setMoved(null); onMoved?.(); };
  if (closed) return null;
  if (moved !== null) {
    const waiting = moved.filter((file) => file.uncommitted === true).map((file) => file.path);
    return <Dialog open title="Moved to .crazyagent/imported/" onClose={finish}>
      <div className="stack-small">
        <p className="secondary">Their text is in this project's .crazyagent/instructions.md.</p>
        <ul className="instruction-import">{moved.map((file) => <li key={file.path}><strong>{file.path}</strong></li>)}</ul>
        <ImportMoveCommit projectId={projectId} files={waiting} onCommitted={finish} onLeave={finish} />
      </div>
    </Dialog>;
  }
  if (pending.kind !== "ok" || pending.value.length === 0) return null;
  const confirm = async () => {
    setFailure("");
    setBusy(true);
    const result = session.noteResult(await confirmInstructionImport(projectId));
    setBusy(false);
    if (result.kind !== "ok") { setFailure(processMessage(result)); return; }
    if (result.value.some((file) => file.uncommitted === true)) { setMoved(result.value); return; }
    finish();
    const names = result.value.map((file) => file.path).join(", ");
    session.showToast(names ? `Moved to .crazyagent/imported/: ${names}. Their text is in the project's instructions.` : "Nothing was moved.");
  };
  return <InstructionImportView name={name} files={pending.value} busy={busy} failure={failure}
    onConfirm={() => void confirm()} onLater={() => setClosed(true)} />;
}

/** Settings, Process: the app-wide instructions every agent gets. */
export function ProcessSettings() {
  return <ProcessEditor scope={{ kind: "app" }} />;
}
