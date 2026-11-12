import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { type Opened, handle } from "./run-command-model.js";
import { defaultLevel, engineTerminal } from "./terminal.js";

/** The name of the terminal a confirmed command is typed into. */
export const TERMINAL_NAME = "Overview command";

/**
 * Types the command the owner confirmed in the desktop into a new terminal at the project's
 * access level, then tells the engine it was typed. Always a new terminal, so a command never
 * lands in a program already running in an old one.
 * Inputs: the extension context and the engine bridge. Output: none.
 */
export function runCommand(context: vscode.ExtensionContext, engine: Engine): void {
  context.subscriptions.push(
    engine.on("run_command", (event) => {
      handle(event, {
        open: () => open(engine),
        ack: (request) => engine.request("ide.run_ack", { request }),
      }).then(undefined, () => undefined);
    }),
  );
}

/**
 * Opens the terminal for one command: the engine's sandboxed shell for a Sandboxed or Own
 * branch project, and the IDE's own default terminal for a Full access project, which the
 * engine does not host. Input: the engine bridge. Output: the terminal and whether it attached.
 */
function open(engine: Engine): Opened {
  const level = defaultLevel();
  if (level === "full") {
    return { terminal: vscode.window.createTerminal({ name: TERMINAL_NAME }), ready: Promise.resolve(true) };
  }
  const options = engineTerminal(engine, TERMINAL_NAME, level);
  return { terminal: vscode.window.createTerminal(options), ready: options.pty.ready };
}
