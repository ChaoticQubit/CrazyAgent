import * as vscode from "vscode";
import { BASE_SCHEME, type Engine, parseBasePath, refusal } from "./scm-model.js";

/** Serves the left side of a change set's diff: a file's text before the agent touched it,
 * read from the engine, which answers only for this IDE's project and only for text files
 * of that change set. Inputs: the extension context and the engine client. Output: none;
 * the provider is disposed with the extension. */
export function registerBaseContent(context: vscode.ExtensionContext, engine: Engine): void {
  const provider: vscode.TextDocumentContentProvider = {
    async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
      const target = parseBasePath(uri.path);
      if (target === null) throw new Error(refusal("changes_not_found"));
      const reply = await engine.request<string>("ide.base_blob", { id: target.id, path: target.path });
      if (reply.kind === "error") throw new Error(refusal(reply.code));
      if (typeof reply.value !== "string") throw new Error(refusal("changes_outcome_unknown"));
      return reply.value;
    },
  };
  context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(BASE_SCHEME, provider));
}
