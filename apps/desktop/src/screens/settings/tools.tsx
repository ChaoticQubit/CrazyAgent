import { useState } from "react";
import { restoreToolSwitches, setToolSwitch, toolSwitches, type Result, type ToolSwitch, type ToolView } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, SettingLine, SettingSection, Skeleton, Switch } from "../../ui";

const NOT_READY = "This change isn't ready in this build yet.";

/** The switch a tool has where it is shown: in a project its own switch, else the every-project
 * one. Inputs: the tool and the project id, if any. Output: on, off or hidden. */
export function toolState(tool: ToolView, projectId?: string): ToolSwitch {
  return projectId ? tool.here ?? tool.global : tool.global;
}

/** Whether this tool has a switch stored in this scope. Inputs: the tool and the project id,
 * if any. Output: the answer. */
export function toolChanged(tool: ToolView, projectId?: string): boolean {
  return projectId ? tool.here !== null : tool.global !== "on";
}

/** Whether any of these tools has a switch stored in this scope, so Restore defaults has
 * something to clear. Inputs: the tools and the project id, if any. Output: the answer. */
export function toolsChanged(tools: ToolView[], projectId?: string): boolean {
  return tools.some((tool) => toolChanged(tool, projectId));
}

/** One server's tools as a pure view: a switch per tool, Delete for a built-in tool, and
 * Restore defaults while any switch is stored here. Deleted tools are left out and counted.
 * Inputs: the read, project id, last failure, the tool being changed and the actions. */
export function ToolSwitchesView({ tools, projectId, failure, busy, onToggle, onHide, onRestore, onRetry }: {
  tools: Loaded<ToolView[]>;
  projectId?: string;
  failure: string;
  busy: string | null;
  onToggle: (tool: ToolView, on: boolean) => void;
  onHide: (tool: ToolView) => void;
  onRestore: () => void;
  onRetry: () => void;
}) {
  if (tools.kind === "loading") return <div className="set-body"><Skeleton shape="row" count={3} /></div>;
  if (tools.kind === "not_ready") return <div className="set-body"><NotReady what="Tool switches" compact /></div>;
  if (tools.kind === "error") return <div className="set-body"><LoadError message="Couldn't load the tools." onRetry={onRetry} /></div>;
  const shown = tools.value.filter((tool) => toolState(tool, projectId) !== "hidden");
  const hidden = tools.value.length - shown.length;
  const label = projectId ? "On in this project" : "On for every project";
  return <>
    {failure && <div className="set-body"><Notice tone="danger" alert>{failure}</Notice></div>}
    {shown.length === 0 ? <p className="set-body secondary">{tools.value.length === 0 ? "No tools yet. Check the server to list its tools." : "Every tool here is deleted."}</p>
      : shown.map((tool) => <SettingLine key={`${tool.server}/${tool.tool}`} label={tool.tool} shown={<span className="mono">{tool.tool}</span>} help={tool.description || undefined} changed={toolChanged(tool, projectId)}>
        <Switch id={`tool-${tool.server}-${tool.tool}`} label={label} checked={toolState(tool, projectId) === "on"} onChecked={(next) => onToggle(tool, next)}
          disabledReason={busy === tool.tool ? "Saving the last change." : undefined} />
        {tool.builtin && <Button variant="quiet" aria-label={`Delete ${tool.tool}`} onClick={() => onHide(tool)}>Delete</Button>}
      </SettingLine>)}
    {(hidden > 0 || toolsChanged(tools.value, projectId)) && <div className="set-body"><span className="row">
      {hidden > 0 && <span className="secondary small">{hidden} deleted. Restore defaults brings {hidden === 1 ? "it" : "them"} back.</span>}
      <Button size="small" icon="refresh-cw" pending={busy === "restore"} pendingLabel="Restoring..." onClick={onRestore}>Restore defaults</Button>
    </span></div>}
  </>;
}

/** One server's tool switches read from the engine: the CrazyAgent built-in tools (no server id)
 * or one MCP server's checked tools, global or for one project. Inputs: project id and server id. */
export function ToolSwitches({ projectId, serverId }: { projectId?: string; serverId: string | null }) {
  const session = useAppSession();
  const [tools, reload] = useLoad(() => toolSwitches(projectId, serverId), [projectId, serverId]);
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  async function act(key: string, change: () => Promise<Result<ToolView[]>>, done: string) {
    if (busy) return;
    setFailure("");
    setBusy(key);
    const result = session.noteResult(await change());
    setBusy(null);
    if (result.kind === "ok") session.showToast(done);
    else setFailure(result.kind === "not_ready" ? NOT_READY : result.message);
    reload();
  }

  return <ToolSwitchesView tools={tools} projectId={projectId} failure={failure} busy={busy} onRetry={reload}
    onToggle={(tool, on) => void act(tool.tool, () => setToolSwitch(projectId, serverId, tool.tool, on ? "on" : "off"), on ? `${tool.tool} turned on.` : `${tool.tool} turned off.`)}
    onHide={(tool) => void act(tool.tool, () => setToolSwitch(projectId, serverId, tool.tool, "hidden"), `${tool.tool} deleted. Restore defaults brings it back.`)}
    onRestore={() => void act("restore", () => restoreToolSwitches(projectId, serverId), "Tool switches restored.")} />;
}

/** Settings, Plugins, Tools: one switch per CrazyAgent built-in tool, and Add a tool, which adds
 * an MCP server whose tools then get their own switches under MCP servers. Inputs: the project
 * id, if any, and the action that opens Add MCP server. */
export function ToolsSection({ projectId, onAddTool }: { projectId?: string; onAddTool: () => void }) {
  return <SettingSection title="CrazyAgent tools" actions={<Button icon="plus" onClick={onAddTool}>Add a tool</Button>} about={projectId
    ? "Tools agents in this project can call. A switch here overrides the one for every project."
    : "Tools every agent can call. Each project can override a switch. Delete hides a built-in tool until you restore the defaults."}>
    <ToolSwitches projectId={projectId} serverId={null} />
    <p className="set-body secondary small">Each MCP server's tools have their own switches: open MCP servers and expand a server's tools.</p>
  </SettingSection>;
}
