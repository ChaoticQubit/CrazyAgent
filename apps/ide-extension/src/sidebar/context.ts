import * as vscode from "vscode";
import type { Engine } from "../engine.js";
import { contextFolder, contextLinks, type Found } from "./model.js";
import { RowTree, coalesced } from "./tree.js";

const PROJECT_FOLDER = "Project";

/**
 * The Brief and documents view: quick links that open the project's brief, documents and
 * research read from its context folder. Refreshes when the workspace changes or the view
 * is shown again. Input: the extension context and the engine bridge. Output: none.
 */
export function sidebarContext(context: vscode.ExtensionContext, engine: Engine): void {
  const tree = new RowTree("crazyagent.context", (row, item) => {
    if (row.path === undefined) return;
    const file = vscode.Uri.file(row.path);
    item.resourceUri = file;
    item.iconPath = vscode.ThemeIcon.File;
    item.command = { command: "vscode.open", title: "Open", arguments: [file] };
  });
  const load = async (): Promise<void> => {
    const folders = vscode.workspace.workspaceFolders ?? [];
    const project = (folders.find((folder) => folder.name === PROJECT_FOLDER) ?? folders[0])?.uri;
    if (project === undefined || project.scheme !== "file") {
      tree.say("Open the CrazyAgent workspace to see the project's brief and documents.");
      tree.show([]);
      return;
    }
    const folder = contextFolder(project.fsPath, await found(vscode.Uri.joinPath(project, ".crazyagent")));
    const [documents, research] = await Promise.all([names(`${folder}/documents`), names(`${folder}/research`)]);
    tree.say(undefined);
    tree.show(contextLinks(folder, documents, research));
  };
  const refresh = coalesced(load);
  context.subscriptions.push(
    tree,
    engine.on("workspace_changed", refresh),
    vscode.workspace.onDidChangeWorkspaceFolders(refresh),
    tree.view.onDidChangeVisibility((change) => {
      if (change.visible) refresh();
    }),
    vscode.commands.registerCommand("crazyagent.context.refresh", refresh),
  );
  refresh();
}

async function found(path: vscode.Uri): Promise<Found | null> {
  try {
    const { type } = await vscode.workspace.fs.stat(path);
    return { directory: (type & vscode.FileType.Directory) !== 0, symlink: (type & vscode.FileType.SymbolicLink) !== 0 };
  } catch {
    return null;
  }
}

async function names(folder: string): Promise<string[] | null> {
  try {
    const entries = await vscode.workspace.fs.readDirectory(vscode.Uri.file(folder));
    return entries.filter(([, type]) => type === vscode.FileType.File).map(([name]) => name);
  } catch {
    return null;
  }
}
