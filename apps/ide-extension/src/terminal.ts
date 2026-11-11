import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { ABOVE_PROJECT, ACCESS_ENV, type Access, type Level, SandboxedPty, accessOf, gitFirstOnPath } from "./pty.js";

/** The id of the Sandboxed terminal profile `package.json` contributes. */
export const PROFILE = "crazyagent.sandboxed";

/** The title of the Sandboxed terminal profile, the engine's default for a Sandboxed project. */
export const TITLE = "Sandboxed";

/** The id of the Own branch terminal profile `package.json` contributes. */
export const OWN_BRANCH_PROFILE = "crazyagent.ownBranch";

/** The title of the Own branch terminal profile, the engine's default for an Own branch project. */
export const OWN_BRANCH_TITLE = "Own branch";

/** The label of the owner's login shell, the profile the engine writes for a Full access project. */
export const FULL_TITLE = "Full access";

const OFFERED: readonly { id: string; name: string; level: Level; command: string }[] = [
  { id: PROFILE, name: TITLE, level: "sandboxed", command: "crazyagent.terminal.sandboxed" },
  { id: OWN_BRANCH_PROFILE, name: OWN_BRANCH_TITLE, level: "own_branch", command: "crazyagent.terminal.ownBranch" },
];

const ACCESS_KEY = "crazyagent.ideAccess";
const FULL_COMMAND = "crazyagent.terminal.full";

/**
 * The palette's commands for the two levels above a project's. The palette drops a greyed
 * command instead of greying it, so these stand in for the menu's greyed rows there and only
 * say to raise project access.
 */
const ABOVE: readonly string[] = ["crazyagent.terminal.ownBranchAbove", "crazyagent.terminal.fullAbove"];

/**
 * What an engine-hosted terminal opens with. Inputs: the bridge, the terminal's name and its
 * level. Output: the terminal options, whose pty relays the engine's shell and says when it
 * attached.
 */
export function engineTerminal(
  engine: Engine,
  name: string,
  level: Level,
): vscode.ExtensionTerminalOptions & { pty: SandboxedPty } {
  return { name, pty: new SandboxedPty((call, args) => engine.stream(call, args), level) };
}

/**
 * The level a terminal opens at when the owner picked none: the project's access level, which
 * the engine named when it started this IDE and which its default terminal profile follows.
 * Output: the level, the narrowest when the engine named none.
 */
export function defaultLevel(): Access {
  return accessOf(process.env[ACCESS_ENV]);
}

/**
 * The menu in the terminal panel's title: tells VS Code the project's access level, which
 * greys the levels above it, and opens a terminal at the level picked. The two engine-hosted
 * levels open directly, because `newWithProfile` finds only the profiles VS Code's settings
 * list, not one an extension contributes. The engine still clamps each terminal itself.
 * Inputs: the extension context and the bridge. Output: none; the commands are the context's
 * subscriptions.
 */
export function terminalMenu(context: vscode.ExtensionContext, engine: Engine): void {
  void vscode.commands.executeCommand("setContext", ACCESS_KEY, accessOf(process.env[ACCESS_ENV]));
  for (const { command, name, level } of OFFERED) {
    context.subscriptions.push(
      vscode.commands.registerCommand(command, () => {
        vscode.window.createTerminal(engineTerminal(engine, name, level));
      }),
    );
  }
  context.subscriptions.push(
    vscode.commands.registerCommand(FULL_COMMAND, () =>
      vscode.commands.executeCommand("workbench.action.terminal.newWithProfile", { profileName: FULL_TITLE }),
    ),
  );
  for (const command of ABOVE) {
    context.subscriptions.push(
      vscode.commands.registerCommand(command, () => {
        void vscode.window.showInformationMessage(ABOVE_PROJECT);
      }),
    );
  }
}

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
        provideTerminalProfile: () => new vscode.TerminalProfile(engineTerminal(engine, name, level)),
      }),
    );
  }
  context.subscriptions.push(
    vscode.window.onDidOpenTerminal((terminal) => {
      if (OFFERED.some(({ name }) => name === terminal.name)) terminal.show(false);
    }),
  );
}
