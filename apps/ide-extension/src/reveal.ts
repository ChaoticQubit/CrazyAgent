import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { revealPlace } from "./reveal-path.js";

/**
 * Shows what the owner asked the TUI to open here: a file opens in an editor and a folder
 * is revealed in the Explorer, then the engine hears it was shown. A path outside the
 * workspace folders is not shown and not acknowledged, so the TUI says it was not shown.
 * Inputs: the extension context and the engine bridge. Output: none.
 */
export function reveal(context: vscode.ExtensionContext, engine: Engine): void {
  context.subscriptions.push(
    engine.on("reveal", (event) => {
      show(event.path).then(
        (shown) => (shown ? engine.request("ide.reveal_ack", { request: event.request }) : undefined),
        () => undefined,
      );
    }),
  );
}

/**
 * Opens one revealed path inside its workspace folder. Input: the absolute path the
 * engine sent. Output: whether it was shown.
 */
async function show(path: string): Promise<boolean> {
  const folders = vscode.workspace.workspaceFolders ?? [];
  const place = revealPlace(
    folders.map((folder) => folder.uri.path),
    path,
  );
  if (place === undefined) return false;
  const folder = folders[place.index].uri;
  const uri = place.parts.length === 0 ? folder : vscode.Uri.joinPath(folder, ...place.parts);
  const stat = await vscode.workspace.fs.stat(uri);
  if (stat.type & vscode.FileType.Directory) {
    await vscode.commands.executeCommand("revealInExplorer", uri);
  } else {
    await vscode.window.showTextDocument(uri, { preview: false });
  }
  return true;
}
