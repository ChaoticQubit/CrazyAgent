import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import type { Feature } from "./extension.js";
import { unconfirmed } from "./scm-model.js";
import { EDIT_ANYWAY, agentOf, editAnywayQuestion, insideFolder } from "./worktree.js";

/** The command behind the "Edit anyway" button on a read-only agent file. */
export const EDIT_ANYWAY_COMMAND = "crazyagent.editAnyway";

/**
 * Adds "Edit anyway" for files the engine keeps read-only while their agent runs: the
 * editor title button and command palette entry ask first, then have the engine record
 * the owner's edit, unlock the file until the agent's turn ends and tell the agent.
 */
export const readonly: Feature = (context, engine) => {
  context.subscriptions.push(vscode.commands.registerCommand(EDIT_ANYWAY_COMMAND, () => editAnyway(engine)));
};

/**
 * Asks before unlocking the active editor's file in a running agent's worktree, then asks
 * the engine to record the edit and makes the editor writable at once.
 * Input: the engine bridge. Output: none; refusals are shown to the owner.
 */
async function editAnyway(engine: Engine): Promise<void> {
  const uri = vscode.window.activeTextEditor?.document.uri;
  const folder = uri === undefined ? undefined : vscode.workspace.getWorkspaceFolder(uri);
  const agent = folder === undefined ? null : agentOf(folder.uri.fsPath);
  const file = uri === undefined || folder === undefined ? null : insideFolder(folder.uri.fsPath, uri.fsPath);
  if (folder === undefined || agent === null || file === null) {
    await vscode.window.showInformationMessage("This file is not in a running agent's worktree.");
    return;
  }
  const choice = await vscode.window.showWarningMessage(
    editAnywayQuestion(folder.name),
    {
      modal: true,
      detail: "Your edit becomes part of the agent's change set, marked as edited by you, and the agent is told.",
    },
    EDIT_ANYWAY,
  );
  if (choice !== EDIT_ANYWAY) return;
  const reply = await engine.request<{ path: string; told: boolean }>("ide.owner_edit", { agent, path: file });
  if (reply.kind !== "ok") {
    await vscode.window.showErrorMessage(unconfirmed(reply.code) ?? `The file stays read-only (${reply.code}).`);
    return;
  }
  await vscode.commands.executeCommand("workbench.action.files.setActiveEditorWriteableInSession");
}
