import * as vscode from "vscode";
import type { Engine } from "../engine.js";
import { agentGroups, type Agent, type Row } from "./model.js";
import { RowTree, ask, coalesced, trouble } from "./tree.js";

const MESSAGE_BYTES_MAX = 16384;
const REFUSALS: Record<string, string> = {
  team_message_invalid: "The message is empty or too long.",
  team_agent_not_found: "That agent is not a running sub-agent of this project.",
  team_agent_not_running: "That agent's terminal is not running.",
  team_agent_unreachable: "That agent's terminal did not take the message.",
};

/**
 * The Agents view: the master, running and stopped sub-agents with their status, an action
 * that shows an agent's worktree in the Explorer and one that types a message into a
 * running sub-agent through the engine. Refreshes on `agents_changed`.
 * Input: the extension context and the engine bridge. Output: none.
 */
export function sidebarAgents(context: vscode.ExtensionContext, engine: Engine): void {
  const tree = new RowTree("crazyagent.agents", (row, item) => {
    const agent = row.agent;
    if (agent === undefined) return;
    item.iconPath = new vscode.ThemeIcon(icon(agent));
    item.tooltip = `${row.label} (${agent.harness}): ${row.description}`;
  });
  const load = async (): Promise<void> => {
    const reply = await ask<Agent[]>(engine, "ide.agents");
    if (reply?.kind !== "ok") {
      tree.say(trouble(reply));
      return;
    }
    tree.say(reply.data.length === 0 ? "No agents yet." : undefined);
    tree.show(agentGroups(reply.data));
  };
  const refresh = coalesced(load);
  const worktree = async (row?: Row): Promise<void> => {
    const path = row?.agent?.worktree;
    if (path === undefined || path === null) return;
    const folder = vscode.Uri.file(path);
    if (vscode.workspace.getWorkspaceFolder(folder) === undefined) {
      void vscode.window.showInformationMessage("This agent's worktree is not in the workspace yet. Show all agents to add it.");
      return;
    }
    await vscode.commands.executeCommand("revealInExplorer", folder);
  };
  const message = async (row?: Row): Promise<void> => {
    const agent = row?.agent;
    if (agent === undefined || !agent.message) return;
    const text = await vscode.window.showInputBox({
      title: `Message ${row?.label ?? "agent"}`,
      prompt: "Typed into the agent's terminal as one message. The master hears about it on its next turn.",
      validateInput: (value) => {
        if (value.trim() === "") return "Type a message.";
        if (Buffer.byteLength(value, "utf8") > MESSAGE_BYTES_MAX) return "The message is too long.";
        return undefined;
      },
    });
    if (text === undefined) return;
    const reply = await ask(engine, "ide.agent_message", { agent: agent.id, text });
    if (reply?.kind === "ok") {
      void vscode.window.showInformationMessage(`Sent to ${row?.label ?? "the agent"}.`);
    } else {
      void vscode.window.showWarningMessage(reply?.kind === "rejected" ? REFUSALS[reply.code] ?? trouble(reply) : trouble(reply));
    }
  };
  context.subscriptions.push(
    tree,
    engine.on("agents_changed", refresh),
    vscode.commands.registerCommand("crazyagent.agents.message", message),
    vscode.commands.registerCommand("crazyagent.agents.openWorktree", worktree),
    vscode.commands.registerCommand("crazyagent.agents.refresh", refresh),
  );
  refresh();
}

function icon(agent: Agent): string {
  if (agent.role === "master") return "star-full";
  if (agent.status === "working") return "sync";
  if (agent.status === "stopped" || agent.status === "failed") return "debug-stop";
  return "person";
}
