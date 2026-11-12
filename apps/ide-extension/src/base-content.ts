import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { BASE_SCHEME, type BasePage, CONFIG_SCHEME, HEAD_SCHEME, parseBasePath, refusal } from "./scm-model.js";

const PAGES_MAX = 512;
const SIDES = [
  [BASE_SCHEME, "ide.base_blob"],
  [CONFIG_SCHEME, "ide.config_blob"],
  [HEAD_SCHEME, "ide.head_blob"],
] as const;

/** Serves the engine-read sides of a change set's diff, read page by page from the engine,
 * which answers only for this IDE's project and only for text files of that change set:
 * the base side, a file's text before the agent touched it, the head side, its text at the
 * head the set recorded, and an agent IDE settings file as the set's head has it, which the
 * IDE server itself cannot read. The documents are read-only. Inputs: the extension context
 * and the engine client. Output: none; the providers are disposed with the extension. */
export function registerBaseContent(context: vscode.ExtensionContext, engine: Engine): void {
  for (const [scheme, call] of SIDES) {
    context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(scheme, provider(engine, call)));
  }
}

/** One side's provider: it reads every page of the file through one engine call, at the
 * head the document address carries, the head the Changes list showed and Accept names,
 * so a set that moved on refuses the read as stale. Inputs: the engine client and the call
 * name. Output: the provider. */
function provider(engine: Engine, call: (typeof SIDES)[number][1]): vscode.TextDocumentContentProvider {
  return {
    async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
      const target = parseBasePath(uri.path);
      if (target === null) throw new Error(refusal("changes_not_found"));
      const parts: string[] = [];
      let page = 0;
      for (;;) {
        if (parts.length >= PAGES_MAX) throw new Error(refusal("changes_too_large"));
        const reply = await engine.request<BasePage>(call, { id: target.id, head: uri.query, path: target.path, page });
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
