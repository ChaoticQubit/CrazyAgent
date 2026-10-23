import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import {
  assignPool, failureCopy, loadProjectMcpServers, loadProjectSkills, nextGeneration, receiptState, removeProject, updateProjectSetup,
} from "../../api-projects";
import type { EngineResult, StoredSetup, TeamPlan } from "../../api-projects";
import { displayText } from "../conversation";
import { PoolPicker } from "../new-project/pool-picker";
import { TeamEditor } from "../new-project/team-editor";
import { teamValid } from "../new-project/wizard";
import { folderLabel } from "./overview";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

const SECTIONS = [
  { id: "general", label: "General" },
  { id: "pool", label: "Pool" },
  { id: "team", label: "Team" },
  { id: "skills", label: "Skills" },
  { id: "mcp", label: "MCP servers" },
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
  const [choice, setChoice] = useState<string | null>(data.assigned?.pool.id ?? null);
  const [state, setState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  useEffect(() => { setChoice(data.assigned?.pool.id ?? null); }, [data.assigned?.pool.id]);

  async function save() {
    if (!choice) return;
    setState({ busy: true, error: "" });
    const previous = data.assigned?.pool.id;
    const added = await assignPool(data.id, choice, nextGeneration());
    let error = added.kind !== "ok" ? failureCopy(added.kind === "failed" ? added.code : "unknown")
      : added.value.kind === "rejected" ? failureCopy(added.value.code)
        : added.value.kind === "unconfirmed" ? "We couldn't confirm your last change. Check it and save again." : "";
    if (!error && previous && previous !== choice) {
      const removed = await assignPool(data.id, previous, nextGeneration(), true);
      if (removed.kind !== "ok" || removed.value.kind !== "applied") error = `${choice} is assigned, but ${previous} still lists this project. Try again.`;
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
    {data.pools?.kind === "failed" && <p className="pj-error" role="alert">{failureCopy(data.pools.code)}</p>}
    {pools && pools.length === 0 && <div className="pj-note"><p>You don't have a pool yet.</p>
      <button type="button" className="pj-btn" onClick={() => data.navigate("/settings/pools?action=new")}>Create pool</button></div>}
    {pools && pools.length > 0 && <PoolPicker name="ps-pool" pools={pools} value={choice} later={false} onChange={setChoice} />}
    {state.error && <p className="pj-error" role="alert">{state.error}</p>}
    <div><button type="button" className="pj-btn primary" disabled={!choice || choice === data.assigned?.pool.id || state.busy} onClick={() => void save()}>{state.busy ? "Saving..." : "Save pool"}</button></div>
  </div>;
}

function TeamSection({ data, setup }: { data: ProjectData; setup: StoredSetup }) {
  const harnesses = valueOf(data.harnesses);
  const [team, setTeam] = useState(setup.team);
  const [note, setNote] = useState("");
  async function save() {
    const result = await updateProjectSetup(data.id, setupOf(setup, team), nextGeneration());
    if (result.kind === "not_ready") return setNote("Not ready yet: changing a project's team isn't in this build.");
    if (result.kind === "failed") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check it and save again.");
    if (state.kind === "applied") { data.notify("Team saved."); data.reload(); }
  }
  return <div className="pj-stack">
    <h2>Team</h2>
    <TeamEditor prefix="ps" team={team} harnesses={harnesses} pool={data.assigned} research={setup.research} onChange={setTeam} />
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div className="pj-summary"><span>Running agents keep their current harness. Changes apply to agents started after you save.</span>
      <button type="button" className="pj-btn primary" disabled={!teamValid(team, harnesses) || JSON.stringify(team) === JSON.stringify(setup.team)} onClick={() => void save()}>Save team</button></div>
  </div>;
}

function ListSection({ title, load, what, body }: { title: string; load: () => Promise<EngineResult<unknown>>; what: string; body: string }) {
  const [result, setResult] = useState<EngineResult<unknown>>();
  useEffect(() => { void load().then(setResult); }, []);
  return <div className="pj-stack">
    <h2>{title}</h2>
    <p className="pj-secondary">{body}</p>
    {result === undefined && <p className="pj-muted" role="status">Loading...</p>}
    {result?.kind === "not_ready" && <NotReady what={what} />}
    {result?.kind === "failed" && <p className="pj-error" role="alert">{failureCopy(result.code)}</p>}
    {result?.kind === "ok" && <p className="pj-muted">The engine answered, but this build can't show the list yet.</p>}
  </div>;
}

function RemoveSection({ data, setup }: { data: ProjectData; setup: StoredSetup | null }) {
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState("");
  async function remove() {
    setConfirming(false);
    const result = await removeProject(data.id, nextGeneration());
    if (result.kind === "not_ready") return setNote("Not ready yet: removing a project isn't in this build.");
    if (result.kind === "failed") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind === "applied") { data.navigate("/home"); return; }
    setNote(state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check it and try again.");
  }
  const folder = setup ? folderLabel(setup.folder) : "its folder";
  return <div className="pj-stack">
    <h2>Remove project</h2>
    <p>Remove {displayText(data.name)} from CrazyAgent? Agents stop. The project folder stays at {displayText(folder)}.</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <div><button type="button" className="pj-btn danger" onClick={() => setConfirming(true)}>Remove project</button></div>
    {confirming && <div className="pj-scrim"><div className="pj-dialog" role="alertdialog" aria-modal="true" aria-labelledby="pj-remove-title">
      <h2 id="pj-remove-title">Remove {displayText(data.name)} from CrazyAgent?</h2>
      <p>Agents stop. The project folder stays at {displayText(folder)}.</p>
      <div className="pj-row end">
        <button type="button" className="pj-btn" autoFocus onClick={() => setConfirming(false)}>Cancel</button>
        <button type="button" className="pj-btn danger" onClick={() => void remove()}>Remove project</button>
      </div>
    </div></div>}
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

/** Project Settings tab: General, Pool, Team, Skills, MCP servers and Remove project.
 * Inputs: the project data and an optional section from the route. Output: the tab. */
export function SettingsTab({ data, section }: { data: ProjectData; section?: string }) {
  const wanted = section ?? hashSection();
  const [active, setActive] = useState<SectionId>(SECTIONS.some((item) => item.id === wanted) ? wanted as SectionId : "general");
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
      {active === "skills" && <ListSection title="Skills" load={() => loadProjectSkills(data.id)} what="per-project skills aren't in this build."
        body="Global skills are available to every project; you can switch them off here. Project skills live in the project folder." />}
      {active === "mcp" && <ListSection title="MCP servers" load={() => loadProjectMcpServers(data.id)} what="per-project MCP servers aren't in this build."
        body="MCP servers give agents extra tools. The engine checks each server, and you approve it before agents can use it." />}
      {active === "remove" && <RemoveSection data={data} setup={setup} />}
    </div>
  </div>;
}
