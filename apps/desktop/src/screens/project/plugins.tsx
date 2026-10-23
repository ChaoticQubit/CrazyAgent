import type { Result } from "../../api";
import { useEffect, useState } from "react";
import { addProjectSkill, failureCopy, loadProjectMcpServers, loadProjectSkills, switchProjectPlugin } from "../../api-projects";
import type { McpServerStatus, McpServerView, PluginScope, SkillView } from "../../api-projects";
import { Button, StatusBadge, Switch, type Tone } from "../../ui";
import { displayText } from "../conversation";
import { NotReady } from "./shared";
import type { ProjectData } from "./shared";

const FOLDER_BYTES_MAX = 4096;

/** Splits skills or servers into the ones on for every project and the ones added to this
 * project, keeping engine order. Input: the items. Output: the two groups. */
export function pluginGroups<T extends { scope: PluginScope }>(items: T[]): { global: T[]; project: T[] } {
  return { global: items.filter((item) => item.scope.kind === "global"), project: items.filter((item) => item.scope.kind === "project") };
}

/** Tells whether a skill or server is on in this project: the project switch when the engine
 * reports one, else its global switch. Input: the item. Output: on or off. */
export function onHere(item: { on_here: boolean | null; enabled: boolean }): boolean {
  return item.on_here ?? item.enabled;
}

/** Maps an MCP server status to the design tone for its status word.
 * Input: the engine status. Output: the tone. */
export function serverTone(status: McpServerStatus): Tone {
  switch (status) {
    case "approved": return "ok";
    case "waiting_for_approval": return "warn";
    case "cant_start": return "danger";
    case "off": return "neutral";
  }
}

/** Checks a typed skill folder before it goes to the engine, which checks it again.
 * Input: the typed path. Output: plain error copy, or null when it is a full path. */
export function skillFolderError(folder: string): string | null {
  const path = folder.trim();
  if (!path.startsWith("/") || path.includes("\0")) return "Enter the full path of a folder that holds SKILL.md.";
  if (new TextEncoder().encode(path).length > FOLDER_BYTES_MAX) return "This path is too long.";
  return null;
}

function useList<T>(load: () => Promise<Result<T[]>>, id: string): [Result<T[]> | undefined, () => void] {
  const [list, setList] = useState<Result<T[]>>();
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    void load().then((result) => { if (live) setList(result); });
    return () => { live = false; };
  }, [id, round]);
  return [list, () => setRound((value) => value + 1)];
}

function ListState<T>({ list, what, loading }: { list: Result<T[]> | undefined; what: string; loading: string }) {
  if (list === undefined) return <p className="pj-muted" role="status">{loading}</p>;
  if (list.kind === "not_ready") return <NotReady what={what} />;
  if (list.kind === "error") return <p className="pj-error" role="alert">{failureCopy(list.code)}</p>;
  return null;
}

function useSwitch(data: Pick<ProjectData, "id">, reload: () => void): [string | null, string, (id: string, on: boolean) => Promise<void>] {
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  async function flip(plugin: string, on: boolean) {
    setBusy(plugin);
    setNote("");
    const result = await switchProjectPlugin(data.id, plugin, on);
    setBusy(null);
    if (result.kind === "not_ready") return setNote("Not ready yet: switching this for one project isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result.code));
    reload();
  }
  return [busy, note, flip];
}

/** Project Settings, Skills: skills on for every project with a switch for this project, then
 * this project's own skills with their folders and Add skill to this project.
 * Input: the project data. Output: the section. */
export function ProjectSkills({ data }: { data: Pick<ProjectData, "id" | "notify"> }) {
  const [list, reload] = useList<SkillView>(() => loadProjectSkills(data.id), data.id);
  const [busy, note, flip] = useSwitch(data, reload);
  const [folder, setFolder] = useState("");
  const [error, setError] = useState("");
  const [adding, setAdding] = useState(false);
  const groups = pluginGroups(list?.kind === "ok" ? list.value : []);

  async function add() {
    const problem = skillFolderError(folder);
    if (problem) return setError(problem);
    setError("");
    setAdding(true);
    const result = await addProjectSkill(data.id, folder.trim());
    setAdding(false);
    if (result.kind === "not_ready") return setError("Not ready yet: adding a skill isn't in this build.");
    if (result.kind === "error") return setError(failureCopy(result.code));
    setFolder("");
    data.notify(`${result.value.name} added to this project.`);
    reload();
  }

  return <div className="pj-stack">
    <h2>Skills</h2>
    <p className="pj-secondary">Skills on for every project can be switched off here. Project skills apply to this project only.</p>
    <ListState list={list} what="project skills aren't in this build." loading="Loading skills..." />
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {list?.kind === "ok" && <section className="pj-stack-s" aria-labelledby="pj-skills-global">
      <h3 id="pj-skills-global">For every project</h3>
      {groups.global.length ? <ul className="pj-rows">{groups.global.map((item) => <li key={item.id} className="pj-plugin-row">
        <span><strong>{displayText(item.name)}</strong><span className="pj-small pj-secondary">{displayText(item.description)}</span></span>
        <Switch id={`pj-skill-${item.id}`} label="On in this project" checked={onHere(item)} onChecked={(on) => void flip(item.id, on)}
          disabledReason={busy === item.id ? "Saving..." : undefined} />
      </li>)}</ul> : <p className="pj-muted">No skills are on for every project. Add them in Settings, Plugins and skills.</p>}
    </section>}
    {list?.kind === "ok" && <section className="pj-stack-s" aria-labelledby="pj-skills-project">
      <h3 id="pj-skills-project">Project skills</h3>
      {groups.project.length ? <ul className="pj-rows">{groups.project.map((item) => <li key={item.id} className="pj-plugin-row">
        <span><strong>{displayText(item.name)}</strong><span className="pj-small pj-secondary">{displayText(item.description)}</span>
          <span className="pj-mono pj-muted">{displayText(item.folder)}</span></span>
      </li>)}</ul> : <p className="pj-muted">No skills added to this project yet.</p>}
      <form className="pj-stack-s" onSubmit={(event) => { event.preventDefault(); void add(); }}>
        <label htmlFor="pj-skill-folder">Folder with SKILL.md</label>
        <span className="pj-row">
          <input className="pj-input" id="pj-skill-folder" value={folder} placeholder="/Users/you/skills/review" aria-describedby={error ? "pj-skill-folder-error" : undefined}
            aria-invalid={error ? true : undefined} onChange={(event) => setFolder(event.target.value)} />
          <Button type="submit" pending={adding} pendingLabel="Adding...">Add skill to this project</Button>
        </span>
        <span className="pj-help">The folder picker isn't in this build, so type the folder's full path. The engine keeps its own copy.</span>
        {error && <p className="pj-error" id="pj-skill-folder-error" role="alert">{error}</p>}
      </form>
    </section>}
  </div>;
}

/** Project Settings, MCP servers: every server with its status word, a switch for this project
 * on servers that are on for every project, and a link to approve waiting servers.
 * Input: the project data. Output: the section. */
export function ProjectServers({ data }: { data: Pick<ProjectData, "id" | "notify" | "navigate"> }) {
  const [list, reload] = useList<McpServerView>(() => loadProjectMcpServers(data.id), data.id);
  const [busy, note, flip] = useSwitch(data, reload);
  const servers = list?.kind === "ok" ? list.value : [];
  const waiting = servers.some((item) => item.status === "waiting_for_approval");
  return <div className="pj-stack">
    <h2>MCP servers</h2>
    <p className="pj-secondary">MCP servers give agents extra tools. Agents use a server only after you approve it.</p>
    <ListState list={list} what="project MCP servers aren't in this build." loading="Loading MCP servers..." />
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {waiting && <div className="pj-note warn"><p>A server waits for your approval before agents can use it.</p>
      <Button onClick={() => data.navigate("/settings/plugins")}>Review in Settings</Button></div>}
    {list?.kind === "ok" && (servers.length ? <ul className="pj-rows" aria-label="MCP servers">{servers.map((item) => <li key={item.id} className="pj-plugin-row">
      <span><strong>{displayText(item.name)}</strong> <StatusBadge word={item.status_label} tone={serverTone(item.status)} />
        <span className="pj-mono pj-muted">{displayText(item.runs)}</span>
        <span className="pj-small pj-secondary">{displayText(item.scope_label)}{item.tools.length ? `. ${item.tools.length} ${item.tools.length === 1 ? "tool" : "tools"}.` : ""}</span>
        {item.problem && <span className="pj-small pj-warn-text">{displayText(item.problem)}</span>}</span>
      {item.scope.kind === "global" && <Switch id={`pj-server-${item.id}`} label="On in this project" checked={onHere(item)} onChecked={(on) => void flip(item.id, on)}
        disabledReason={busy === item.id ? "Saving..." : undefined} />}
    </li>)}</ul> : <p className="pj-muted">No MCP servers yet. Add them in Settings, Plugins and skills.</p>)}
  </div>;
}
