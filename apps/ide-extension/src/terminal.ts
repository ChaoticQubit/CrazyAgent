import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { SandboxedPty, gitFirstOnPath } from "./pty.js";

/** The id of the Sandboxed terminal profile `package.json` contributes. */
export const PROFILE = "crazyagent.sandboxed";

/** The title of the Sandboxed terminal profile, which the engine sets as the default. */
export const TITLE = "Sandboxed";

/**
 * Makes `git` in the terminals the IDE server hosts the git the IDE's Source Control runs.
 * Input: the extension context. Output: none.
 */
export function terminalGit(context: vscode.ExtensionContext): void {
  gitFirstOnPath(context.environmentVariableCollection, vscode.workspace.getConfiguration("git").get("path"));
}

/**
 * Offers the Sandboxed terminal profile: every terminal opened from it is the owner's
 * shell under the project's agent sandbox, which the engine runs outside the IDE server's
 * sandbox and relays over the bridge. Inputs: the extension context and the bridge.
 * Output: none; the provider is one of the context's subscriptions.
 */
export function sandboxedTerminal(context: vscode.ExtensionContext, engine: Engine): void {
  context.subscriptions.push(
    vscode.window.registerTerminalProfileProvider(PROFILE, {
      provideTerminalProfile: () =>
        new vscode.TerminalProfile({
          name: TITLE,
          pty: new SandboxedPty((name, args) => engine.stream(name, args)),
        }),
    }),
  );
}
