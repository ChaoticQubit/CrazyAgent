import { nextGeneration, poolSummaries, setApplyHarnessSwitch } from "../../api";
import type { Result } from "../../api";
import { useContext, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { failureCopy, loadAgentPolicy, loadHiddenPaths, loadProjectCatalog, loadProjects, loadReadPaths, loadRemovalPreview, receiptState, removeProject, saveAgentPolicy, saveHiddenPaths, saveReadPaths, setProjectPool, updateProjectSetup, type ReadPaths } from "../../api-projects";
import type { AgentAccess, AgentNetwork, AgentPolicy, IdeAgentFiles, PolicyView, ProjectCatalog, RemovalPreview, StoredSetup, ToolCacheScope } from "../../api-projects";
import type { IntentReceipt, MessageSettings, SwitchApply, TeamStatus } from "../../engine-client";
import { AccessLevels, FULL_ACCESS_IDE, FULL_ACCESS_WARNING, IdeAgentFilesChoices, IdeGitHooksSwitch, LOWERING_STOPS_AGENTS, NetworkChoices, POLICY_RESTARTS_IDE, TOOLCHAIN_OFF_STOPS_AGENTS, ToolCacheChoices, ToolchainSwitches, accessLabel, confirmFirst, policyOf } from "./access";
import { DEFAULT_MESSAGE_SETTINGS, MESSAGE_SETTING_FIELDS, changeMessageSetting, fieldValue, messageSettingsOf } from "../../api-messages";
import { Button, ConfirmDialog, Dialog, Segmented, Select } from "../../ui";
import type { SelectOption } from "../../ui";
import { displayText } from "../conversation";
import { BasicsFields, ResearchSwitches } from "../new-project/fields";
import { PoolPicker } from "../new-project/pool-picker";
import { TeamEditor } from "../new-project/team-editor";
import { basicsErrors, draftOf, editRequest, poolName, researchChoices, teamValid, wireTeam } from "../new-project/wizard";
import type { TeamDraft } from "../new-project/wizard";
import { folderLabel, projectFolder } from "./overview";
import { changeOutcome } from "./research";
import { RemoveDialogView, deleteReady, removeTitle } from "./remove-dialog";
import type { RemoveChoice } from "./remove-dialog";
import { PlanningLine } from "./planning";
import { SessionContext, useHeld, useLoad } from "../../state";
import { ProjectAccountOrder } from "../settings/account-choice";
import { PluginsAndSkills } from "../settings/plugins";
import { TEAM_SETTINGS_STALE, changeNote, settingsVersion } from "../team/controls";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

const SECTIONS = [
  { id: "general", label: "General" },
  { id: "pool", label: "Pool" },
  { id: "team", label: "Team" },
  { id: "plugins", label: "Plugins and skills" },
  { id: "access", label: "Agent access" },
  { id: "sandbox", label: "Sandbox" },
  { id: "messages", label: "Messages" },
  { id: "remove", label: "Remove project" },
] as const;

type SectionId = typeof SECTIONS[number]["id"];

function hashParam(name: string): string | null {
  if (typeof window === "undefined") return null;
  const query = window.location.hash.split("?")[1];
  return query ? new URLSearchParams(query).get(name) : null;
}

function hashSection(): string | null {
  return hashParam("section");
}

/** The project setup Project Settings saves: the stored details with this team, as the engine
 * takes it. Inputs: the stored setup and the edited team. Output: the setup to send. */
export function setupOf(setup: Pick<StoredSetup, "about" | "tech_stack" | "platforms" | "research" | "pool">, team: TeamDraft) {
  return { about: setup.about, tech_stack: setup.tech_stack, platforms: setup.platforms, research: setup.research, pool: setup.pool, team: wireTeam(team), documents: [] };
}

/** The no-pool choice Project Settings offers: the project keeps its settings and waits. */
export const NO_POOL = { label: "No pool", note: "The master and agents wait and show Pick a pool to start." } as const;

/** Plans Save pool: give the project the chosen pool, or none, when that isn't so yet, and record
 * the choice in the project details when they name another. Inputs: the assigned pool, the pool
 * the details name (undefined without details) and the choice (null for none). Output: the steps;
 * nothing to do when the choice is already assigned and recorded. */
export function poolSteps(assigned: string | null, recorded: string | null | undefined, choice: string | null): { assign: boolean; record: boolean } {
  return { assign: choice !== assigned, record: recorded !== undefined && recorded !== choice };
}

/** The line after Save pool. Input: the chosen pool's name, or null for none. Output: the line. */
export function poolSaved(name: string | null): string {
  return name === null ? "This project has no pool now. Pick a pool to start." : `${name} is assigned.`;
}

/** Assigns a project's pool as the Pool section does: the pool first, then the project details
 * name it too, then the project and its pools are read again. Inputs: the project data, the
 * stored setup or null, and the pool id or null for none. Output: the error to show, empty once
 * both steps applied, when the owner is told which pool is assigned. */
export async function savePool(data: Pick<ProjectData, "id" | "pools" | "assigned" | "notify" | "reload" | "reloadPools">, setup: StoredSetup | null, choice: string | null): Promise<string> {
  const steps = poolSteps(data.assigned?.pool.id ?? null, setup ? setup.pool : undefined, choice);
  const found = choice === null ? undefined : valueOf(data.pools)?.find((item) => item.pool.id === choice);
  const name = choice === null ? null : found ? poolName(found.pool) : "The pool";
  let error = "";
  if (steps.assign) {
    const assigned = await setProjectPool(data.id, choice, nextGeneration());
    error = assigned.kind !== "ok" ? failureCopy(assigned.kind === "error" ? assigned.code : "unknown")
      : assigned.value.kind === "rejected" ? failureCopy(assigned.value)
        : assigned.value.kind === "unconfirmed" ? "We couldn't confirm your last change. Check it and save again." : "";
  }
  if (!error && steps.record && setup) {
    const recorded = await updateProjectSetup(data.id, { ...setupOf(setup, setup.team), pool: choice }, nextGeneration(), null, setup.revision);
    if (recorded.kind !== "ok" || receiptState(recorded.value).kind !== "applied") {
      error = `${choice === null ? "The project has no pool" : `${name} is assigned`}, but the project details still name the old pool. Save again.`;
    }
  }
  data.reloadPools();
  data.reload();
  if (!error) data.notify(poolSaved(name));
  return error;
}

function PoolSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const pools = valueOf(data.pools);
  const [summaries] = useLoad(poolSummaries, [data.id], "pool-summaries");
  const [choice, setChoice] = useState<string | null>(data.assigned?.pool.id ?? null);
  const [state, setState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  useEffect(() => { setChoice(data.assigned?.pool.id ?? null); }, [data.assigned?.pool.id]);

  const steps = poolSteps(data.assigned?.pool.id ?? null, setup ? setup.pool : undefined, choice);

  async function save() {
    setState({ busy: true, error: "" });
    setState({ busy: false, error: await savePool(data, setup, choice) });
  }

  return <div className="pj-stack">
    <h2>Pool</h2>
    {data.pools === undefined && <p className="pj-muted" role="status">Loading pools...</p>}
    {data.pools?.kind === "not_ready" && <NotReady what="pools aren't in this build." />}
    {data.pools?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(data.pools)}</p>}
    {pools && pools.length === 0 && <div className="pj-note"><p>You don't have a pool yet.</p>
      <button type="button" className="btn" onClick={() => data.navigate("/settings/pools?action=new")}>Create pool</button></div>}
    {pools && pools.length > 0 && <PoolPicker name="ps-pool" pools={pools} summaries={summaries.kind === "ok" ? summaries.value : []} value={choice}
      later laterLabel={NO_POOL.label} laterNote={NO_POOL.note} onChange={setChoice} />}
    {state.error && <p className="pj-error" role="alert">{state.error}</p>}
    <div><button type="button" className="btn primary" disabled={!(steps.assign || steps.record) || state.busy} onClick={() => void save()}>{state.busy ? "Saving..." : "Save pool"}</button></div>
    {data.assigned && <ProjectAccountOrder project={data.id} pool={data.assigned.pool.id} />}
  </div>;
}

/** Saves a project's team as the Team section does: the stored setup with this team, at the
 * setup revision shown, so a save from an older read is refused. Inputs: the project data, the
 * stored setup and the team. Output: the note to show, empty once the engine applied it, when
 * the owner is told and the project is read again. */
export async function saveTeam(data: Pick<ProjectData, "id" | "notify" | "reload">, setup: StoredSetup, team: TeamDraft): Promise<string> {
  const result = await updateProjectSetup(data.id, setupOf(setup, team), nextGeneration(), null, setup.revision);
  if (result.kind === "not_ready") return "Not ready yet: changing a project's team isn't in this build.";
  if (result.kind === "error") return failureCopy(result);
  const state = receiptState(result.value);
  if (state.kind !== "applied") return state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and save again.";
  data.notify("Team saved.");
  data.reload();
  return "";
}

function TeamSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  const harnesses = valueOf(data.harnesses);
  const [team, setTeam] = useState<TeamDraft>(setup.team);
  const [note, setNote] = useState("");
  async function save() {
    setNote(await saveTeam(data, setup, team));
  }
  return <div className="pj-stack">
    <h2>Team</h2>
    <TeamEditor prefix="ps" team={team} harnesses={harnesses} pool={data.assigned} research={researchChoices(setup.research).research} onChange={setTeam} />
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div className="pj-summary"><span>Running sub-agents keep their current harness; changes apply to agents started after you save, and the master follows Apply a master harness change. Waiting research moves to the new team.</span>
      <button type="button" className="btn primary" disabled={!teamValid(team, harnesses) || JSON.stringify(wireTeam(team)) === JSON.stringify(setup.team)} onClick={() => void save()}>Save team</button></div>
    <ApplySwitch data={data} />
  </div>;
}

/** The choices for when a saved master harness change reaches the running live master. */
export const APPLY_OPTIONS: SelectOption[] = [
  { value: "when_idle", label: "When the master is idle" },
  { value: "next_start", label: "At the next start" },
];

/** The Apply a master harness change choice a team status holds, When the master is idle when
 * an older engine sends none. Input: the status. Output: the choice. */
export function applyMode(status: Pick<TeamStatus, "apply_harness_switch">): SwitchApply {
  return status.apply_harness_switch ?? "when_idle";
}

/** The Apply a master harness change setting (N-D26), saved at once like the team's other
 * switches with the team settings version the page read; a save from an older read says the
 * settings changed and the page reads the team again. Input: the project data with its team
 * status read. Output: the select with the last change's note, or nothing before the team
 * status is read. */
export function ApplySwitch({ data }: { data: Pick<ProjectData, "id" | "team" | "reloadTeam"> }) {
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState("");
  const status = data.team.result?.kind === "ok" ? data.team.result.value : null;
  const [shown, hold] = useHeld<SwitchApply | null>(status === null ? null : applyMode(status), Object.is, status);
  if (status === null) return null;
  const choose = async (mode: SwitchApply) => {
    const settled = hold(mode);
    setSaving(true);
    const result = await setApplyHarnessSwitch(data.id, mode, settingsVersion(status));
    const message = savedStale(result) ? TEAM_SETTINGS_STALE : changeNote(result, "the Apply a master harness change setting");
    setSaving(false);
    settled(message);
    setNote(message);
    data.reloadTeam();
  };
  return <>
    <Select id="ps-apply-switch" label="Apply a master harness change" value={shown ?? applyMode(status)} options={APPLY_OPTIONS}
      onValue={(value) => { if (!saving && (value === "when_idle" || value === "next_start")) void choose(value); }}
      helper="When the master is idle switches the running master once it finishes its turn and an account of the pool can run the new harness. At the next start keeps it on its harness until it starts again." />
    {note && <p className="pj-note warn" role="status">{displayText(note)}</p>}
  </>;
}

/** What removing a project does, for the section and its confirm. Inputs: the project folder, or
 * null when the project has no saved setup. Output: one plain paragraph. */
export function removeLine(folder: string | null): string {
  const where = folder ? `at ${folderLabel(folder)}` : "where it is";
  return `Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk ${where}, with every file in it.`;
}

function RemoveSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const session = useContext(SessionContext);
  const [confirming, setConfirming] = useState(() => hashParam("ask") === "remove");
  const [preview, setPreview] = useState<Result<RemovalPreview>>();
  const [choice, setChoice] = useState<RemoveChoice>("dashboard");
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const line = removeLine(setup ? projectFolder(setup.folder) : null);
  const read = preview?.kind === "ok" ? preview.value : null;
  useEffect(() => {
    if (!confirming) return;
    let live = true;
    setPreview(undefined);
    setChoice("dashboard");
    setTyped("");
    void loadRemovalPreview(data.id).then((result) => { if (live) setPreview(result); });
    return () => { live = false; };
  }, [confirming, data.id]);
  async function remove() {
    const deleting = choice === "everything";
    if (deleting && !deleteReady(read, typed)) return;
    setBusy(true);
    const result = await removeProject(data.id, nextGeneration(), deleting ? typed : null);
    setBusy(false);
    setConfirming(false);
    if (result.kind === "not_ready") return setNote("Not ready yet: removing a project isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind === "applied") {
      await session?.refreshProjects();
      const name = data.name || "The project";
      data.notify(deleting ? `${name} was deleted. Its folder is in the Trash.` : `${name} was removed. Its folder stays on disk.`);
      data.navigate("/home");
      return;
    }
    setNote(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and try again.");
  }
  return <div className="pj-stack">
    <h2>Remove project</h2>
    <p>Remove {displayText(data.name)} from CrazyAgent? {displayText(line)}</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><button type="button" className="btn danger" onClick={() => setConfirming(true)}>Remove project</button></div>
    <Dialog open={confirming} title={removeTitle(displayText(data.name))} onClose={() => setConfirming(false)} busy={busy} wide>
      {preview === undefined
        ? <p className="pj-muted" role="status">Reading what removing would do...</p>
        : <>
          {preview.kind !== "ok" && <p className="pj-note warn" role="status">We couldn't read what deleting would remove, so only Remove from dashboard is offered.</p>}
          <RemoveDialogView preview={read} choice={choice} typed={typed} pending={busy} onChoice={setChoice} onTyped={setTyped}
            onConfirm={() => void remove()} onCancel={() => setConfirming(false)} />
        </>}
    </Dialog>
  </div>;
}

function GeneralSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  const session = useContext(SessionContext);
  const [draft, setDraft] = useState(() => draftOf(setup));
  const [catalog, setCatalog] = useState<Result<ProjectCatalog>>();
  const [names, setNames] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  useEffect(() => {
    let live = true;
    void loadProjectCatalog(setup.pool, researchChoices(setup.research).research).then((result) => { if (live) setCatalog(result); });
    void loadProjects().then((result) => { if (live && result.kind === "ok") setNames(result.value.filter((item) => item.id !== data.id).map((item) => item.name)); });
    return () => { live = false; };
  }, [data.id]);
  const errors = basicsErrors(draft, names);
  const edit = editRequest(setup, draft);
  const canSave = edit.changed && Object.keys(errors).length === 0 && !busy;

  async function save() {
    if (!canSave) return;
    setBusy(true);
    setNote("");
    const outcome = changeOutcome(await updateProjectSetup(data.id, edit.setup, nextGeneration(), edit.name, setup.revision), "Changes saved.", "changing a project's details");
    setBusy(false);
    setNote(outcome.note);
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.reload) data.reload();
    if (outcome.toast && edit.name !== null) await session?.refreshProjects();
  }

  return <form className="pj-stack" aria-labelledby="ps-general-title" onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <h2 id="ps-general-title">General</h2>
    {catalog?.kind === "not_ready" && <NotReady what="the tech stack list isn't in this build. You can still add your own entries." />}
    <BasicsFields prefix="ps" draft={draft} onChange={setDraft} catalog={catalog} errors={errors} showErrors />
    <fieldset className="pj-field">
      <legend>Research</legend>
      <p className="pj-help">Turning research off stops its agent. Turning it on starts an agent when the project has a pool.</p>
      <ResearchSwitches prefix="ps" catalog={catalog} value={draft.research} onChange={(research) => setDraft({ ...draft, research })} />
    </fieldset>
    <div className="pj-field">
      <span className="pj-label">Project folder</span>
      <p className="pj-mono">{displayText(folderLabel(projectFolder(setup.folder)))}</p>
    </div>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><Button type="submit" variant="primary" pending={busy} pendingLabel="Saving..." disabled={!canSave}>Save changes</Button></div>
    <p className="pj-small pj-muted">Project id for support: <span className="pj-mono">{data.id}</span></p>
  </form>;
}

/** The most extra read-only folders a project can share with its agents, as the engine allows. */
export const READ_PATHS_MAX = 8;

/** The most folders a project can hide from its agents, as the engine allows. */
export const HIDDEN_PATHS_MAX = 8;

/** Adds one typed folder to the extra read-only list, as the engine will check it again.
 * Inputs: the current list and the typed text. Output: the new list, or a plain error. */
export function addReadPath(paths: string[], typed: string): { paths: string[] } | { error: string } {
  const path = typed.trim();
  if (!path.startsWith("/")) return { error: "Type the full path of a folder, starting with /." };
  if (path.length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return { error: "Each folder must be one plain path of up to 1024 characters." };
  if (paths.includes(path)) return { error: "That folder is already in the list." };
  if (paths.length >= READ_PATHS_MAX) return { error: `A project can share at most ${READ_PATHS_MAX} extra folders.` };
  return { paths: [...paths, path] };
}

/** What the folders list says after a save refused because the folders changed since it read
 * them, once it shows the saved folders again. */
export const READ_PATHS_STALE = "These folders changed since you opened them. The list now shows the saved folders; make your change again.";

/** What Agent access says after a save refused because the policy changed since the page read
 * it, once it shows the saved access again. */
export const ACCESS_STALE = "Agent access changed since you opened it. The page now shows the saved access; make your change again.";

const ACCESS_NOT_READY = "Not ready yet: agent access isn't in this build.";

/** What the owner is told once agent access is saved. Input: the saved level. Output: the line. */
export function accessSaved(access: AgentAccess): string {
  return `Agent access saved: ${accessLabel(access)}. Agents and master turns started from now on use it.`;
}

/** Whether a versioned save, such as read-only folders or agent access, was refused because
 * the value changed since the screen read it, so the screen reads it again instead of
 * resending the same old version. Input: the save's result. Output: true for that refusal. */
export function savedStale(result: Result<IntentReceipt>): boolean {
  if (result.kind !== "ok") return result.kind === "error" && result.code === "stale_revision";
  const state = receiptState(result.value);
  return state.kind === "rejected" && state.code === "stale_revision";
}

/** Adds one typed folder to the hidden list, as the engine will check it again against the
 * project folder. Inputs: the current list and the typed text. Output: the new list, or a plain
 * error for what the client can judge; the engine checks the folder exists inside the project. */
export function addHiddenPath(paths: string[], typed: string): { paths: string[] } | { error: string } {
  const path = typed.trim();
  if (!path.startsWith("/")) return { error: "Type the full path of a folder, starting with /." };
  if (path.length > 1024 || /["'\\\u0000-\u001f\u007f]/.test(path)) return { error: "Each folder must be one plain path of up to 1024 characters, without quotes or backslashes." };
  if (paths.includes(path)) return { error: "That folder is already in the list." };
  if (paths.length >= HIDDEN_PATHS_MAX) return { error: `A project can hide at most ${HIDDEN_PATHS_MAX} folders.` };
  return { paths: [...paths, path] };
}

function SandboxSection({ data }: { data: ProjectData }) {
  const [saved, setSaved] = useState<ReadPaths | null>(null);
  const [paths, setPaths] = useState<string[]>([]);
  const [typed, setTyped] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let live = true;
    void loadReadPaths(data.id).then((result) => {
      if (!live) return;
      if (result.kind === "ok") { setSaved(result.value); setPaths(result.value.paths); }
      else setNote(result.kind === "not_ready" ? "Not ready yet: extra read-only folders aren't in this build." : failureCopy(result));
    });
    return () => { live = false; };
  }, [data.id]);

  function add() {
    const next = addReadPath(paths, typed);
    if ("error" in next) return setNote(next.error);
    setPaths(next.paths);
    setTyped("");
    setNote("");
  }

  async function reread(): Promise<boolean> {
    const fresh = await loadReadPaths(data.id);
    if (fresh.kind !== "ok") return false;
    setSaved(fresh.value);
    setPaths(fresh.value.paths);
    return true;
  }

  async function save() {
    if (saved === null) return;
    setBusy(true);
    const result = await saveReadPaths(data.id, paths, saved.version, nextGeneration());
    if (savedStale(result)) {
      const shown = await reread();
      setBusy(false);
      return setNote(shown ? READ_PATHS_STALE : "These folders changed since you opened them. Reopen this page to see them.");
    }
    if (result.kind !== "ok") { setBusy(false); return setNote(result.kind === "not_ready" ? "Not ready yet: extra read-only folders aren't in this build." : failureCopy(result)); }
    const state = receiptState(result.value);
    if (state.kind !== "applied") { setBusy(false); return setNote(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and save again."); }
    const shown = await reread();
    setBusy(false);
    if (!shown) return setNote("Read-only folders saved. Reopen this page to change them again.");
    setNote("");
    data.notify("Read-only folders saved. Agents started from now on get them.");
  }

  const changed = saved !== null && JSON.stringify(saved.paths) !== JSON.stringify(paths);
  return <div className="pj-stack">
    <h2>Sandbox</h2>
    <p className="pj-secondary">Agents find the toolchains this project uses (Rust, Node, Python and Homebrew) on their own and can read them, never change them. Add other folders they may read here. They can never read credential files, such as .npmrc, .netrc or .ssh, even inside these folders.</p>
    {saved === null && !note && <p className="pj-muted" role="status">Loading...</p>}
    {saved !== null && <ul className="pj-paths" aria-label="Extra read-only folders">
      {paths.length === 0 && <li className="pj-muted">No extra folders.</li>}
      {paths.map((path) => <li key={path}><span className="pj-mono">{displayText(path)}</span>{" "}
        <button type="button" className="btn" aria-label={`Remove ${path}`} onClick={() => setPaths(paths.filter((item) => item !== path))}>Remove</button></li>)}
    </ul>}
    {saved !== null && <form className="pj-field" onSubmit={(event) => { event.preventDefault(); add(); }}>
      <label className="pj-label" htmlFor="ps-read-path">Folder path</label>
      <input id="ps-read-path" className="pj-input" value={typed} placeholder="/Users/you/shared-config" onChange={(event) => setTyped(event.target.value)} />
      <div><button type="submit" className="btn" disabled={!typed.trim()}>Add folder</button></div>
    </form>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><Button variant="primary" pending={busy} pendingLabel="Saving..." disabled={!changed || busy} onClick={() => void save()}>Save folders</Button></div>
  </div>;
}

function HiddenSection({ data }: { data: ProjectData }) {
  const [saved, setSaved] = useState<string[] | null>(null);
  const [paths, setPaths] = useState<string[]>([]);
  const [typed, setTyped] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [fullAccess, setFullAccess] = useState(false);
  const notReady = "Not ready yet: hidden folders aren't in this build.";
  useEffect(() => {
    let live = true;
    void loadHiddenPaths(data.id).then((result) => {
      if (!live) return;
      if (result.kind === "ok") { setSaved(result.value); setPaths(result.value); }
      else setNote(result.kind === "not_ready" ? notReady : failureCopy(result));
    });
    void loadAgentPolicy(data.id).then((result) => {
      if (!live) return;
      if (result.kind === "ok") setFullAccess(result.value.access === "full");
    });
    return () => { live = false; };
  }, [data.id]);

  function add() {
    const next = addHiddenPath(paths, typed);
    if ("error" in next) return setNote(next.error);
    setPaths(next.paths);
    setTyped("");
    setNote("");
  }

  async function save() {
    setBusy(true);
    const result = await saveHiddenPaths(data.id, paths, nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? notReady : failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind !== "applied") return setNote(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and save again.");
    setSaved(paths);
    setNote("");
    data.notify("Hidden folders saved. Agents started from now on can't open them.");
  }

  const changed = saved !== null && JSON.stringify(saved) !== JSON.stringify(paths);
  return <div className="pj-stack">
    <h2>Folders agents can't open</h2>
    <p className="pj-secondary">Folders inside the project that agents, the master and research jobs can't read, write or run while access is Sandboxed. Use it for a secrets folder or a vendored dump you don't want touched. Each must be a folder inside the project that git does not track.</p>
    {fullAccess && <p className="pj-note warn" role="status">Full access runs agents without a sandbox, so hidden folders do not apply.</p>}
    {saved === null && !note && <p className="pj-muted" role="status">Loading...</p>}
    {saved !== null && <ul className="pj-paths" aria-label="Hidden folders">
      {paths.length === 0 && <li className="pj-muted">No hidden folders.</li>}
      {paths.map((path) => <li key={path}><span className="pj-mono">{displayText(path)}</span>{" "}
        <button type="button" className="btn" aria-label={`Stop hiding ${path}`} onClick={() => setPaths(paths.filter((item) => item !== path))}>Remove</button></li>)}
    </ul>}
    {saved !== null && <form className="pj-field" onSubmit={(event) => { event.preventDefault(); add(); }}>
      <label className="pj-label" htmlFor="ps-hidden-path">Folder path</label>
      <input id="ps-hidden-path" className="pj-input" value={typed} placeholder="/Users/you/project/secrets" onChange={(event) => setTyped(event.target.value)} />
      <div><button type="submit" className="btn" disabled={!typed.trim()}>Hide folder</button></div>
    </form>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><Button variant="primary" pending={busy} pendingLabel="Saving..." disabled={!changed || busy} onClick={() => void save()}>Save hidden folders</Button></div>
  </div>;
}

/** Saves a project's agent access as the Agent access section does, at the policy version
 * shown; a save from an older read reads the policy again and says so. Inputs: the project id,
 * the policy shown and the policy to save. Output: the note to show, empty once the engine
 * applied it, and the policy read again, or null when it was not read. */
export async function saveAccess(projectId: string, view: PolicyView, policy: AgentPolicy): Promise<{ note: string; fresh: PolicyView | null }> {
  const result = await saveAgentPolicy(projectId, policy, view.version, nextGeneration());
  if (savedStale(result)) {
    const fresh = await loadAgentPolicy(projectId);
    return fresh.kind === "ok" ? { note: ACCESS_STALE, fresh: fresh.value } : { note: "Agent access changed since you opened it. Reopen this page to see it.", fresh: null };
  }
  if (result.kind !== "ok") return { note: result.kind === "not_ready" ? ACCESS_NOT_READY : failureCopy(result), fresh: null };
  const state = receiptState(result.value);
  if (state.kind !== "applied") return { note: state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and save again.", fresh: null };
  const fresh = await loadAgentPolicy(projectId);
  return { note: "", fresh: fresh.kind === "ok" ? fresh.value : null };
}

function AccessSection({ data }: { data: ProjectData }) {
  const [view, setView] = useState<PolicyView | null>(null);
  const [access, setAccess] = useState<AgentAccess>("sandboxed");
  const [network, setNetwork] = useState<AgentNetwork>("open");
  const [toolCache, setToolCache] = useState<ToolCacheScope>("shared");
  const [allowed, setAllowed] = useState<boolean[]>([]);
  const [ideGitHooks, setIdeGitHooks] = useState(false);
  const [ideAgentFiles, setIdeAgentFiles] = useState<IdeAgentFiles>("allow_all");
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const notReady = ACCESS_NOT_READY;
  function seed(next: PolicyView) {
    setView(next);
    setAccess(next.access);
    setNetwork(next.network);
    setToolCache(next.tool_cache);
    setAllowed(next.toolchains.map((row) => row.allowed));
    setIdeGitHooks(next.ide_git_hooks);
    setIdeAgentFiles(next.ide_agent_files);
  }
  useEffect(() => {
    let live = true;
    void loadAgentPolicy(data.id).then((result) => {
      if (!live) return;
      if (result.kind === "ok") seed(result.value);
      else setNote(result.kind === "not_ready" ? notReady : failureCopy(result));
    });
    return () => { live = false; };
  }, [data.id]);

  async function save() {
    if (!view) return;
    setConfirming(false);
    setBusy(true);
    const saved = await saveAccess(data.id, view, policyOf(view, access, network, allowed, ideGitHooks, ideAgentFiles, toolCache));
    setBusy(false);
    if (saved.fresh) seed(saved.fresh);
    setNote(saved.note);
    if (saved.note === "") data.notify(accessSaved(access));
  }

  const changed = view !== null && (access !== view.access || network !== view.network || toolCache !== view.tool_cache || ideGitHooks !== view.ide_git_hooks || ideAgentFiles !== view.ide_agent_files || allowed.some((on, index) => on !== view.toolchains[index]?.allowed));
  return <div className="pj-stack">
    <h2>Agent access</h2>
    <p className="pj-secondary">What this project's agents may touch. It applies to the master, sub-agents and research jobs; research jobs write reports, so they run Sandboxed unless you choose Full access.</p>
    {view === null && !note && <p className="pj-muted" role="status">Loading...</p>}
    {view && <AccessLevels name="ps-access" value={access} unavailable={(level) => view.levels.find((item) => item.access === level)?.unavailable ?? null} onChange={setAccess} />}
    {view && <p className="pj-help">{LOWERING_STOPS_AGENTS}</p>}
    {view && <p className="pj-help">{POLICY_RESTARTS_IDE}</p>}
    {view && <NetworkChoices name="ps-network" value={network} access={access} onChange={setNetwork} />}
    {view && <ToolCacheChoices name="ps-tool-cache" value={toolCache} access={access} onChange={setToolCache} />}
    {view && <ToolchainSwitches prefix="ps" view={view} allowed={allowed} onChange={setAllowed} />}
    {view && <p className="pj-help">{TOOLCHAIN_OFF_STOPS_AGENTS}</p>}
    {view && <IdeGitHooksSwitch prefix="ps" checked={ideGitHooks} onChange={setIdeGitHooks} />}
    {view && <IdeAgentFilesChoices name="ps-agent-files" value={ideAgentFiles} access={access} onChange={setIdeAgentFiles} />}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><Button variant="primary" pending={busy} pendingLabel="Saving..." disabled={!changed || busy}
      onClick={() => confirmFirst(access) ? setConfirming(true) : void save()}>Save agent access</Button></div>
    <ConfirmDialog open={confirming} title="Give this project's agents Full access?" confirmLabel="Allow Full access" cancelLabel="Keep current access" pending={busy}
      onCancel={() => setConfirming(false)} onConfirm={() => void save()}>
      {FULL_ACCESS_WARNING} {FULL_ACCESS_IDE}
    </ConfirmDialog>
  </div>;
}

/** The Messages section: the six settings of the sub-agents' Messages timelines, each with every
 * choice and the proposed default named. A choice changes its one setting against what the
 * engine stores now; when another client changed the settings since this view was read, it
 * saves nothing and shows the stored ones. The section shows the outcome until the next team
 * read, then what the engine stored. Input: the project data. Output: the section. */
function MessagesSection({ data }: { data: ProjectData }) {
  const [pending, setPending] = useState<{ settings: MessageSettings; at: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const read = data.team.result;
  const saved = read?.kind === "ok" ? messageSettingsOf(read.value) : null;
  const shown = pending !== null && data.team.at < pending.at ? pending.settings : saved;

  async function choose(key: keyof MessageSettings, value: string) {
    if (busy || shown === null) return;
    setBusy(true);
    const change = await changeMessageSetting(data.id, shown, key, value);
    setBusy(false);
    if (change.kind === "elsewhere") {
      setPending({ settings: change.settings, at: Date.now() });
      setNote("Messages settings changed elsewhere, so nothing was saved. This shows what's saved now; choose again.");
    } else {
      const message = changeNote(change.result, "Messages settings");
      setPending(message || change.kind === "unread" ? null : { settings: change.settings, at: Date.now() });
      setNote(message);
    }
    data.reloadTeam();
  }

  return <div className="pj-stack">
    <h2>Messages</h2>
    <p className="pj-secondary">Each sub-agent's Messages show its tasks, what the master and you told it, and its questions, progress and results, newest first. Open them from the Agents tab or the team graph.</p>
    {read === undefined && <p className="pj-muted" role="status">Loading...</p>}
    {read?.kind === "not_ready" && <NotReady what="the live team isn't in this build." />}
    {read?.kind === "error" && <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(read)}</p>}
    {read?.kind === "ok" && shown === null && <NotReady what="Messages settings aren't in this build." />}
    {shown !== null && MESSAGE_SETTING_FIELDS.map((field) => {
      const proposed = field.options.find((option) => option.value === fieldValue(DEFAULT_MESSAGE_SETTINGS, field.key));
      return <div className="pj-field" key={field.key}>
        <span className="pj-label" aria-hidden="true">{field.label}</span>
        <Segmented label={field.label} value={fieldValue(shown, field.key)} options={[...field.options]} onValue={(value) => void choose(field.key, value)} />
        <p className="pj-help">{field.help} Recommended: {proposed?.label}.</p>
      </div>;
    })}
    {note && <p className="pj-note warn" role="status">{note}</p>}
  </div>;
}

/** Picks the Settings section a route asks for. Links from before skills and MCP servers shared
 * one section still open it. Input: the route's section, if any. Output: a known section. */
export function settingsSection(wanted: string | null | undefined): SectionId {
  if (wanted === "skills" || wanted === "mcp") return "plugins";
  return SECTIONS.find((item) => item.id === wanted)?.id ?? "general";
}

/** Project Settings tab: General, Pool, Team, Plugins and skills, Agent access, Sandbox, Messages and Remove project. A setup
 * that belongs to another project counts as still loading, so no form seeds from it.
 * Inputs: the project data and an optional section from the route. Output: the tab. */
export function SettingsTab({ data, section }: { data: ProjectData; section?: string }) {
  const [active, setActive] = useState<SectionId>(settingsSection(section ?? hashSection()));
  const loaded = valueOf(data.setup);
  const setup = loaded && loaded.project_id !== data.id ? null : loaded;
  const needsSetup = (body: () => ReactNode) => setup ? body()
    : data.setup?.kind === "not_ready" ? <NotReady what="project details aren't in this build." />
      : data.setup === undefined || loaded !== setup ? <p className="pj-muted" role="status">Loading...</p>
        : <p className="pj-muted">This project was created before project setup, so it has no team or details to change.</p>;
  return <div className="pj-settings">
    <nav className="pj-subnav" aria-label="Project settings">
      {SECTIONS.map((item) => <button type="button" key={item.id} aria-current={active === item.id ? "page" : undefined} onClick={() => setActive(item.id)}>{item.label}</button>)}
    </nav>
    <div className="pj-settings-body">
      {active === "general" && needsSetup(() => <><GeneralSection data={data} setup={setup!} /><PlanningLine data={data} /></>)}
      {active === "pool" && <PoolSection data={data} setup={setup} />}
      {active === "team" && needsSetup(() => <TeamSection data={data} setup={setup!} />)}
      {active === "plugins" && <div className="pj-stack">
        <h2>Plugins and skills</h2>
        <p className="pj-secondary">What you add here applies to this project only. Items from Settings, Plugins and skills can be switched off for this project.</p>
        <PluginsAndSkills projectId={data.id} />
      </div>}
      {active === "access" && <AccessSection data={data} />}
      {active === "sandbox" && <div className="pj-stack"><SandboxSection data={data} /><HiddenSection data={data} /></div>}
      {active === "messages" && <MessagesSection data={data} />}
      {active === "remove" && <RemoveSection data={data} setup={setup} />}
    </div>
  </div>;
}
