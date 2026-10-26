import { nextGeneration, poolSummaries } from "../../api";
import type { Result } from "../../api";
import { useContext, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { assignPool, failureCopy, loadProjectCatalog, loadProjects, receiptState, removeProject, updateProjectSetup } from "../../api-projects";
import type { ProjectCatalog, StoredSetup } from "../../api-projects";
import { Button, ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { BasicsFields, ResearchSwitches } from "../new-project/fields";
import { PoolPicker } from "../new-project/pool-picker";
import { TeamEditor } from "../new-project/team-editor";
import { basicsErrors, draftOf, editRequest, poolName, teamValid, wireTeam } from "../new-project/wizard";
import type { TeamDraft } from "../new-project/wizard";
import { folderLabel, projectFolder } from "./overview";
import { changeOutcome } from "./research";
import { SessionContext, useLoad } from "../../state";
import { PluginsAndSkills } from "../settings/plugins";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

const SECTIONS = [
  { id: "general", label: "General" },
  { id: "pool", label: "Pool" },
  { id: "team", label: "Team" },
  { id: "plugins", label: "Plugins and skills" },
  { id: "remove", label: "Remove project" },
] as const;

type SectionId = typeof SECTIONS[number]["id"];

function hashSection(): string | null {
  if (typeof window === "undefined") return null;
  const query = window.location.hash.split("?")[1];
  return query ? new URLSearchParams(query).get("section") : null;
}

/** The project setup Project Settings saves: the stored details with this team, as the engine
 * takes it. Inputs: the stored setup and the edited team. Output: the setup to send. */
export function setupOf(setup: Pick<StoredSetup, "about" | "tech_stack" | "platforms" | "research" | "pool">, team: TeamDraft) {
  return { about: setup.about, tech_stack: setup.tech_stack, platforms: setup.platforms, research: setup.research, pool: setup.pool, team: wireTeam(team), documents: [] };
}

/** Plans Save pool: assign the chosen pool when it isn't assigned yet, take the project off the
 * old one, and record the choice in the project details when they name another pool. Inputs: the
 * assigned pool, the pool the details name (undefined without details) and the choice.
 * Output: the steps; nothing to do when the choice is already assigned and recorded. */
export function poolSteps(assigned: string | null, recorded: string | null | undefined, choice: string | null): { add: boolean; drop: string | null; record: boolean } {
  if (!choice) return { add: false, drop: null, record: false };
  const add = choice !== assigned;
  return { add, drop: add && assigned ? assigned : null, record: recorded !== undefined && recorded !== choice };
}

function PoolSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const pools = valueOf(data.pools);
  const [summaries] = useLoad(poolSummaries, [data.id]);
  const [choice, setChoice] = useState<string | null>(data.assigned?.pool.id ?? null);
  const [state, setState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  useEffect(() => { setChoice(data.assigned?.pool.id ?? null); }, [data.assigned?.pool.id]);

  const steps = poolSteps(data.assigned?.pool.id ?? null, setup ? setup.pool : undefined, choice);

  async function save() {
    if (!choice) return;
    setState({ busy: true, error: "" });
    const label = (id: string) => { const found = valueOf(data.pools)?.find((item) => item.pool.id === id); return found ? poolName(found.pool) : id; };
    let error = "";
    if (steps.add) {
      const added = await assignPool(data.id, choice, nextGeneration());
      error = added.kind !== "ok" ? failureCopy(added.kind === "error" ? added.code : "unknown")
        : added.value.kind === "rejected" ? failureCopy(added.value.code)
          : added.value.kind === "unconfirmed" ? "We couldn't confirm your last change. Check it and save again." : "";
    }
    if (!error && steps.drop) {
      const removed = await assignPool(data.id, steps.drop, nextGeneration(), true);
      if (removed.kind !== "ok" || removed.value.kind !== "applied") error = `${label(choice)} is assigned, but ${label(steps.drop)} still lists this project. Try again.`;
    }
    if (!error && steps.record && setup) {
      const recorded = await updateProjectSetup(data.id, { ...setupOf(setup, setup.team), pool: choice }, nextGeneration(), null, setup.revision);
      if (recorded.kind !== "ok" || receiptState(recorded.value).kind !== "applied") error = `${label(choice)} is assigned and research can use it, but the project details still name the old pool. Save again.`;
    }
    setState({ busy: false, error });
    data.reloadPools();
    data.reload();
    if (!error) data.notify("Pool saved.");
  }

  return <div className="pj-stack">
    <h2>Pool</h2>
    {data.pools === undefined && <p className="pj-muted" role="status">Loading pools...</p>}
    {data.pools?.kind === "not_ready" && <NotReady what="pools aren't in this build." />}
    {data.pools?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(data.pools.code)}</p>}
    {pools && pools.length === 0 && <div className="pj-note"><p>You don't have a pool yet.</p>
      <button type="button" className="btn" onClick={() => data.navigate("/settings/pools?action=new")}>Create pool</button></div>}
    {pools && pools.length > 0 && <PoolPicker name="ps-pool" pools={pools} summaries={summaries.kind === "ok" ? summaries.value : []} value={choice} later={false} onChange={setChoice} />}
    {state.error && <p className="pj-error" role="alert">{state.error}</p>}
    <div><button type="button" className="btn primary" disabled={!(steps.add || steps.record) || state.busy} onClick={() => void save()}>{state.busy ? "Saving..." : "Save pool"}</button></div>
  </div>;
}

function TeamSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  const harnesses = valueOf(data.harnesses);
  const [team, setTeam] = useState<TeamDraft>(setup.team);
  const [note, setNote] = useState("");
  async function save() {
    const result = await updateProjectSetup(data.id, setupOf(setup, team), nextGeneration(), null, setup.revision);
    if (result.kind === "not_ready") return setNote("Not ready yet: changing a project's team isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check it and save again.");
    if (state.kind === "applied") { data.notify("Team saved."); data.reload(); }
  }
  return <div className="pj-stack">
    <h2>Team</h2>
    <TeamEditor prefix="ps" team={team} harnesses={harnesses} pool={data.assigned} research={setup.research} onChange={setTeam} />
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div className="pj-summary"><span>Running agents keep their current harness. Changes apply to agents started after you save; waiting research moves to the new team.</span>
      <button type="button" className="btn primary" disabled={!teamValid(team, harnesses) || JSON.stringify(wireTeam(team)) === JSON.stringify(setup.team)} onClick={() => void save()}>Save team</button></div>
  </div>;
}

/** What removing a project does, for the section and its confirm. Inputs: the project folder, or
 * null when the project has no saved setup. Output: one plain paragraph. */
export function removeLine(folder: string | null): string {
  const where = folder ? `at ${folderLabel(folder)}` : "where it is";
  return `Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk ${where}, with every file in it.`;
}

function RemoveSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const session = useContext(SessionContext);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const line = removeLine(setup ? projectFolder(setup.folder) : null);
  async function remove() {
    setBusy(true);
    const result = await removeProject(data.id, nextGeneration());
    setBusy(false);
    setConfirming(false);
    if (result.kind === "not_ready") return setNote("Not ready yet: removing a project isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind === "applied") {
      await session?.refreshProjects();
      data.notify(`${data.name || "The project"} was removed. Its folder stays on disk.`);
      data.navigate("/home");
      return;
    }
    setNote(state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check it and try again.");
  }
  return <div className="pj-stack">
    <h2>Remove project</h2>
    <p>Remove {displayText(data.name)} from CrazyAgent? {displayText(line)}</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><button type="button" className="btn danger" onClick={() => setConfirming(true)}>Remove project</button></div>
    <ConfirmDialog open={confirming} title={`Remove ${displayText(data.name)} from CrazyAgent?`} confirmLabel="Remove project" cancelLabel="Keep project" pending={busy}
      onCancel={() => setConfirming(false)} onConfirm={() => void remove()}>
      {displayText(line)}
    </ConfirmDialog>
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
    void loadProjectCatalog(setup.pool, setup.research).then((result) => { if (live) setCatalog(result); });
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

/** Picks the Settings section a route asks for. Links from before skills and MCP servers shared
 * one section still open it. Input: the route's section, if any. Output: a known section. */
export function settingsSection(wanted: string | null | undefined): SectionId {
  if (wanted === "skills" || wanted === "mcp") return "plugins";
  return SECTIONS.find((item) => item.id === wanted)?.id ?? "general";
}

/** Project Settings tab: General, Pool, Team, Plugins and skills, and Remove project. A setup
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
      {active === "general" && needsSetup(() => <GeneralSection data={data} setup={setup!} />)}
      {active === "pool" && <PoolSection data={data} setup={setup} />}
      {active === "team" && needsSetup(() => <TeamSection data={data} setup={setup!} />)}
      {active === "plugins" && <div className="pj-stack">
        <h2>Plugins and skills</h2>
        <p className="pj-secondary">What you add here applies to this project only. Items from Settings, Plugins and skills can be switched off for this project.</p>
        <PluginsAndSkills projectId={data.id} />
      </div>}
      {active === "remove" && <RemoveSection data={data} setup={setup} />}
    </div>
  </div>;
}
