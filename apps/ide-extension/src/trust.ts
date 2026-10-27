import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { CONFIG_SCHEME } from "./scm-model.js";
import { IDE_CONFIG_NOTE, type MarkedSet, ideConfigFiles, ideConfigWarning, warningKey } from "./trust-model.js";

const POLL_MS = 5_000;
const BADGE = "!";

/** Flags agent changes to IDE settings: each `.vscode` or `*.code-workspace` file an open
 * change set changed is decorated in the agent's worktree and in the engine's read-only
 * copy the diff opens, and one warning names them once per change set head. Inputs: the
 * extension context and the engine client. Output: none; all is disposed with the
 * extension. */
export function registerTrust(context: vscode.ExtensionContext, engine: Engine): void {
  const changed = new vscode.EventEmitter<vscode.Uri[] | undefined>();
  const warned = new Set<string>();
  let files = new Map<string, string>();
  let loading: Promise<void> | undefined;

  const show = (sets: MarkedSet[]): void => {
    files = ideConfigFiles(sets);
    changed.fire(undefined);
    for (const set of sets) {
      const warning = ideConfigWarning(set);
      if (warning === null || warned.has(warningKey(set))) continue;
      warned.add(warningKey(set));
      void vscode.window.showWarningMessage(warning);
    }
  };

  const load = (): Promise<void> => {
    loading ??= engine
      .request<MarkedSet[]>("ide.change_sets", {})
      .then(
        (reply) => {
          if (reply.kind === "ok" && Array.isArray(reply.data)) show(reply.data);
        },
        () => undefined,
      )
      .finally(() => {
        loading = undefined;
      });
    return loading;
  };

  const flagged = (tooltip: string, propagate: boolean): vscode.FileDecoration => {
    const decoration = new vscode.FileDecoration(BADGE, tooltip, new vscode.ThemeColor("list.warningForeground"));
    decoration.propagate = propagate;
    return decoration;
  };

  const provider: vscode.FileDecorationProvider = {
    onDidChangeFileDecorations: changed.event,
    provideFileDecoration: (uri: vscode.Uri): vscode.FileDecoration | undefined => {
      if (uri.scheme === CONFIG_SCHEME) return flagged(IDE_CONFIG_NOTE, false);
      if (uri.scheme !== "file") return undefined;
      const agent = files.get(uri.fsPath);
      return agent === undefined ? undefined : flagged(`${IDE_CONFIG_NOTE} (${agent})`, true);
    },
  };

  const timer = setInterval(() => void load(), POLL_MS);
  context.subscriptions.push(
    changed,
    vscode.window.registerFileDecorationProvider(provider),
    engine.on("change_sets_changed", () => void load()),
    { dispose: () => clearInterval(timer) },
  );
  void load();
}
