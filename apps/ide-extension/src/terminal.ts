import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { type Level, SandboxedPty, gitFirstOnPath } from "./pty.js";

/** The id of the Sandboxed terminal profile `package.json` contributes. */
export const PROFILE = "crazyagent.sandboxed";

/** The title of the Sandboxed terminal profile, the engine's default for a Sandboxed project. */
export const TITLE = "Sandboxed";

/** The id of the Own branch terminal profile `package.json` contributes. */
export const OWN_BRANCH_PROFILE = "crazyagent.ownBranch";

/** The title of the Own branch terminal profile, the engine's default for an Own branch project. */
export const OWN_BRANCH_TITLE = "Own branch";

const OFFERED: readonly { id: string; name: string; level: Level }[] = [
  { id: PROFILE, name: TITLE, level: "sandboxed" },
  { id: OWN_BRANCH_PROFILE, name: OWN_BRANCH_TITLE, level: "own_branch" },
];

/**
 * Makes `git` in the terminals the IDE server hosts the git the IDE's Source Control runs.
 * Input: the extension context. Output: none.
 */
export function terminalGit(context: vscode.ExtensionContext): void {
  gitFirstOnPath(context.environmentVariableCollection, vscode.workspace.getConfiguration("git").get("path"));
}

/**
 * Offers the engine-hosted terminal profiles: every terminal opened from one is the owner's
 * shell under the project's agent sandbox at that profile's level, which the engine runs
 * outside the IDE server's sandbox and relays over the bridge. Each one opens its panel, since
 * VS Code does not show one for a terminal a contributed profile starts. Inputs: the extension
 * context and the bridge. Output: none; the providers are the context's subscriptions.
 */
export function sandboxedTerminal(context: vscode.ExtensionContext, engine: Engine): void {
  for (const { id, name, level } of OFFERED) {
    context.subscriptions.push(
      vscode.window.registerTerminalProfileProvider(id, {
        provideTerminalProfile: () =>
          new vscode.TerminalProfile({
            name,
            pty: new SandboxedPty((call, args) => engine.stream(call, args), level),
          }),
      }),
    );
  }
  context.subscriptions.push(
    vscode.window.onDidOpenTerminal((terminal) => {
      if (OFFERED.some(({ name }) => name === terminal.name)) terminal.show(false);
    }),
  );
}
