import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { BASE_SCHEME, type BasePage, HEAD_SCHEME, parseBasePath, refusal } from "./scm-model.js";

const PAGES_MAX = 512;

/** Serves both sides of a change set's diff: a file's text before the agent touched it and
 * its text at the head the set recorded, read from the engine page by page, which answers
 * only for this IDE's project and only for text files of that change set. Inputs: the
 * extension context and the engine client. Output: none; the providers are disposed with
 * the extension. */
export function registerBaseContent(context: vscode.ExtensionContext, engine: Engine): void {
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(BASE_SCHEME, pagedText(engine, "ide.base_blob")),
    vscode.workspace.registerTextDocumentContentProvider(HEAD_SCHEME, pagedText(engine, "ide.head_blob")),
  );
}

/** A provider that reads one side of a diff whole through an engine call that pages it.
 * Inputs: the engine client and the call. Output: the provider. */
function pagedText(engine: Engine, call: "ide.base_blob" | "ide.head_blob"): vscode.TextDocumentContentProvider {
  return {
    async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
      const target = parseBasePath(uri.path);
      if (target === null) throw new Error(refusal("changes_not_found"));
      const parts: string[] = [];
      let page = 0;
      for (;;) {
        if (parts.length >= PAGES_MAX) throw new Error(refusal("changes_too_large"));
        const reply = await engine.request<BasePage>(call, { id: target.id, path: target.path, page });
        if (reply.kind !== "ok") throw new Error(refusal(reply.code));
        const data: Partial<BasePage> = reply.data ?? {};
        if (typeof data.text !== "string") throw new Error(refusal("changes_outcome_unknown"));
        parts.push(data.text);
        if (data.next === null) return parts.join("");
        if (typeof data.next !== "number" || !Number.isInteger(data.next) || data.next <= page) {
          throw new Error(refusal("changes_outcome_unknown"));
        }
        page = data.next;
      }
    },
  };
}
