import { useState } from "react";
import { approveMcpServer, listMcpServers, listSkills, setMcpDefault, setSkillDefault, type McpServer, type Result, type Skill } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, EmptyState, LoadError, NotReady, Notice, Panel, Segmented, Skeleton, StatusBadge, Switch } from "../../ui";
import { homePath } from "./harnesses";

function useAction() {
  const session = useAppSession();
  const [failure, setFailure] = useState("");
  async function act(action: () => Promise<Result<void>>, done: string, reload: () => void) {
    setFailure("");
    const result = session.noteResult(await action());
    if (result.kind === "ok") { session.showToast(done); reload(); }
    else setFailure(result.kind === "not_ready" ? "This change isn't ready in this build yet." : result.message);
  }
  return { failure, act };
}

function Skills({ skills, reload }: { skills: Loaded<Skill[]>; reload: () => void }) {
  const { failure, act } = useAction();
  if (skills.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (skills.kind === "not_ready") return <NotReady what="Skills" compact />;
  if (skills.kind === "error") return <LoadError message="Couldn't load skills." onRetry={reload} />;
  return <div className="stack-small">
    <p className="secondary">A skill is a folder with a SKILL.md file. Global skills are available to every project; you can switch them off per project.</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {skills.value.length === 0 ? <EmptyState icon="book-open" title="No skills yet" compact /> : <Panel padded={false}><div className="rows">
      {skills.value.map((skill) => <div className="item-row" key={skill.id}>
        <div className="stack-small"><span className="title">{skill.name}</span><span className="secondary">{skill.description}</span><span className="mono muted">{homePath(skill.path)}</span></div>
        <Switch id={`skill-${skill.id}`} label="On for new projects" checked={skill.default_on}
          onChecked={(on) => void act(() => setSkillDefault(skill.id, on), on ? "Skill turned on for new projects." : "Skill turned off for new projects.", reload)} />
      </div>)}
    </div></Panel>}
  </div>;
}

function Servers({ servers, reload }: { servers: Loaded<McpServer[]>; reload: () => void }) {
  const { failure, act } = useAction();
  if (servers.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (servers.kind === "not_ready") return <NotReady what="MCP servers" compact />;
  if (servers.kind === "error") return <LoadError message="Couldn't load MCP servers." onRetry={reload} />;
  return <div className="stack-small">
    <p className="secondary">MCP servers give agents extra tools. The engine checks each server, and you approve it before agents can use it.</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {servers.value.length === 0 ? <EmptyState icon="plug" title="No MCP servers yet" compact /> : <Panel padded={false}><div className="rows">
      {servers.value.map((server) => <div key={server.id}>
        <div className="item-row">
          <div className="stack-small"><span className="row"><span className="title">{server.name}</span><StatusBadge word={server.status_label} /></span>
            <span className="mono muted">{server.runs}</span>
            {server.status === "cant_start" && server.reason && <span className="field-error">Can't start: {server.reason}</span>}</div>
          <Switch id={`mcp-${server.id}`} label="On for new projects" checked={server.default_on}
            onChecked={(on) => void act(() => setMcpDefault(server.id, on), on ? "Server turned on for new projects." : "Server turned off for new projects.", reload)} />
        </div>
        {server.status === "needs_approval" && <div className="review">
          <span>Runs: <span className="mono">{server.runs}</span></span>
          <span>Network: {server.network ? "on" : "off"}</span>
          <span>Secrets: {server.secrets.length ? server.secrets.map((name) => `${name} (Key saved)`).join(", ") : "none"}</span>
          <span>Can write: only inside each project folder</span>
          <span><Button variant="primary" icon="shield-check" onClick={() => void act(() => approveMcpServer(server.id), "Server approved.", reload)}>Approve</Button></span>
        </div>}
      </div>)}
    </div></Panel>}
  </div>;
}

/** Settings, Plugins and skills: a switch between global skills and MCP servers, each read
 * from the engine, with per-item default switches and MCP approval. */
export function PluginsAndSkills() {
  const [section, setSection] = useState<"skills" | "servers">("skills");
  const [skills, reloadSkills] = useLoad(listSkills);
  const [servers, reloadServers] = useLoad(listMcpServers);
  return <div className="stack">
    <Segmented label="Plugins and skills" value={section} onValue={setSection}
      options={[{ value: "skills", label: "Skills", icon: "book-open" }, { value: "servers", label: "MCP servers", icon: "plug" }]} />
    {section === "skills" ? <Skills skills={skills} reload={reloadSkills} /> : <Servers servers={servers} reload={reloadServers} />}
  </div>;
}
