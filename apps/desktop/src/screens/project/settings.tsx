import { nextGeneration, poolSummaries } from "../../api";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { assignPool, failureCopy, receiptState, removeProject, updateProjectSetup } from "../../api-projects";
import type { StoredSetup, TeamPlan } from "../../api-projects";
import { ConfirmDialog } from "../../ui";
import { displayText } from "../conversation";
import { PoolPicker } from "../new-project/pool-picker";
import { TeamEditor } from "../new-project/team-editor";
import { poolName, teamValid } from "../new-project/wizard";
import { folderLabel } from "./overview";
import { useLoad } from "../../state";
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

function setupOf(setup: StoredSetup, team: TeamPlan) {
  return { about: setup.about, tech_stack: setup.tech_stack, platforms: setup.platforms, research: setup.research, pool: setup.pool, team, documents: [] };
}

function PoolSection({ data }: { data: ProjectData }) {
  const pools = valueOf(data.pools);
  const [summaries] = useLoad(poolSummaries, [data.id]);
  const [choice, setChoice] = useState<string | null>(data.assigned?.pool.id ?? null);
  const [state, setState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  useEffect(() => { setChoice(data.assigned?.pool.id ?? null); }, [data.assigned?.pool.id]);

  async function save() {
    if (!choice) return;
    setState({ busy: true, error: "" });
    const previous = data.assigned?.pool.id;
    const added = await assignPool(data.id, choice, nextGeneration());
    let error = added.kind !== "ok" ? failureCopy(added.kind === "error" ? added.code : "unknown")
      : added.value.kind === "rejected" ? failureCopy(added.value.code)
        : added.value.kind === "unconfirmed" ? "We couldn't confirm your last change. Check it and save again." : "";
    if (!error && previous && previous !== choice) {
      const removed = await assignPool(data.id, previous, nextGeneration(), true);
      const label = (id: string) => { const found = valueOf(data.pools)?.find((item) => item.pool.id === id); return found ? poolName(found.pool) : id; };
      if (removed.kind !== "ok" || removed.value.kind !== "applied") error = `${label(choice)} is assigned, but ${label(previous)} still lists this project. Try again.`;
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
    <div><button type="button" className="btn primary" disabled={!choice || choice === data.assigned?.pool.id || state.busy} onClick={() => void save()}>{state.busy ? "Saving..." : "Save pool"}</button></div>
  </div>;
}

function TeamSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  const harnesses = valueOf(data.harnesses);
  const [team, setTeam] = useState(setup.team);
  const [note, setNote] = useState("");
  async function save() {
    const result = await updateProjectSetup(data.id, setupOf(setup, team), nextGeneration());
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
    <div className="pj-summary"><span>Running agents keep their current harness. Changes apply to agents started after you save.</span>
      <button type="button" className="btn primary" disabled={!teamValid(team, harnesses) || JSON.stringify(team) === JSON.stringify(setup.team)} onClick={() => void save()}>Save team</button></div>
  </div>;
}

function RemoveSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState("");
  async function remove() {
    setConfirming(false);
    const result = await removeProject(data.id, nextGeneration());
    if (result.kind === "not_ready") return setNote("Not ready yet: removing a project isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind === "applied") { data.navigate("/home"); return; }
    setNote(state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check it and try again.");
  }
  const folder = setup ? folderLabel(setup.folder) : "its folder";
  return <div className="pj-stack">
    <h2>Remove project</h2>
    <p>Remove {displayText(data.name)} from CrazyAgent? Agents stop. The project folder stays at {displayText(folder)}.</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><button type="button" className="btn danger" onClick={() => setConfirming(true)}>Remove project</button></div>
    <ConfirmDialog open={confirming} title={`Remove ${displayText(data.name)} from CrazyAgent?`} confirmLabel="Remove project"
      onCancel={() => setConfirming(false)} onConfirm={() => void remove()}>
      Agents stop. The project folder stays at {displayText(folder)}.
    </ConfirmDialog>
  </div>;
}

function GeneralSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  return <div className="pj-stack">
    <h2>General</h2>
    <dl className="pj-facts-list">
      <div><dt>Project name</dt><dd>{displayText(setup.name)}</dd></div>
      <div><dt>What is it about?</dt><dd className="pj-pre">{setup.about ? displayText(setup.about) : <span className="pj-muted">Nothing yet</span>}</dd></div>
      <div><dt>Tech stack</dt><dd>{setup.tech_stack.map(displayText).join(", ") || "None"}</dd></div>
      <div><dt>Project folder</dt><dd className="pj-mono">{displayText(folderLabel(setup.folder))}</dd></div>
    </dl>
    <NotReady what="editing a project's name, description and stack isn't in this build." />
    <p className="pj-small pj-muted">Project id for support: <span className="pj-mono">{data.id}</span></p>
  </div>;
}

/** Picks the Settings section a route asks for. Links from before skills and MCP servers shared
 * one section still open it. Input: the route's section, if any. Output: a known section. */
export function settingsSection(wanted: string | null | undefined): SectionId {
  if (wanted === "skills" || wanted === "mcp") return "plugins";
  return SECTIONS.find((item) => item.id === wanted)?.id ?? "general";
}

/** Project Settings tab: General, Pool, Team, Plugins and skills, and Remove project.
 * Inputs: the project data and an optional section from the route. Output: the tab. */
export function SettingsTab({ data, section }: { data: ProjectData; section?: string }) {
  const [active, setActive] = useState<SectionId>(settingsSection(section ?? hashSection()));
  const setup = valueOf(data.setup);
  const needsSetup = (body: () => ReactNode) => setup ? body()
    : data.setup?.kind === "not_ready" ? <NotReady what="project details aren't in this build." />
      : data.setup === undefined ? <p className="pj-muted" role="status">Loading...</p>
        : <p className="pj-muted">This project was created before project setup, so it has no team or details to change.</p>;
  return <div className="pj-settings">
    <nav className="pj-subnav" aria-label="Project settings">
      {SECTIONS.map((item) => <button type="button" key={item.id} aria-current={active === item.id ? "page" : undefined} onClick={() => setActive(item.id)}>{item.label}</button>)}
    </nav>
    <div className="pj-settings-body">
      {active === "general" && needsSetup(() => <GeneralSection data={data} setup={setup!} />)}
      {active === "pool" && <PoolSection data={data} />}
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
