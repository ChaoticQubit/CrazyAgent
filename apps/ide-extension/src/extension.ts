import * as vscode from "vscode";
import { registerBaseContent } from "./base-content.js";
import { registerComments } from "./comments.js";
import { Engine } from "./engine.js";
import { extensions } from "./extensions.js";
import { remoteGit } from "./git.js";
import { startLock } from "./lock.js";
import { readonly } from "./readonly.js";
import { reveal } from "./reveal.js";
import { runCommand } from "./run-command.js";
import { sidebarAgents } from "./sidebar/agents.js";
import { sidebarChanges } from "./sidebar/changes.js";
import { sidebarContext } from "./sidebar/context.js";
import { sidebarMemories } from "./sidebar/memories.js";
import { registerChanges } from "./scm.js";
import { sandboxedTerminal, terminalGit, terminalMenu } from "./terminal.js";
import { registerTrust } from "./trust.js";

/**
 * What the built-in extension tells the owner when the engine refused its session as
 * taken (CA-SEC-004): the IDE's access was used twice, by another extension, by a second
 * window starting at the same moment, or by this one again after the engine could not
 * remove the access file, so it names all three (rv-ds241 CA-SEC-004b N1).
 */
export const SESSION_TAKEN =
  "CrazyAgent ended this IDE's connection because its access was used twice: by another extension in it, by a second window of it opening at the same moment, or after CrazyAgent could not remove its access file. Close the IDE and open it again from CrazyAgent. If it happens again with one window, an extension you approved for this project may not be safe.";

/**
 * One CrazyAgent IDE feature. It registers its views, commands and event handlers with
 * the extension context and talks to the engine only through the bridge it is given.
 */
export type Feature = (context: vscode.ExtensionContext, engine: Engine) => void;

const features: Feature[] = [
  registerBaseContent,
  registerChanges,
  registerComments,
  sidebarMemories,
  sidebarAgents,
  sidebarChanges,
  sidebarContext,
  readonly,
  extensions,
  reveal,
  runCommand,
  sandboxedTerminal,
  terminalMenu,
  terminalGit,
  registerTrust,
  remoteGit,
];

/**
 * Starts the settings lock, then every feature when the CrazyAgent engine launched this IDE
 * server, which it shows by setting the capability and socket variables. The bridge keeps
 * its session token in the context's secret storage, and says `SESSION_TAKEN` once if the
 * engine refuses its session as taken.
 * Input: the extension context. Output: none; the bridge closes with the context.
 */
export function activate(context: vscode.ExtensionContext): void {
  startLock(context, process.env);
  const taken = () => void vscode.window.showErrorMessage(SESSION_TAKEN);
  const engine = Engine.fromEnv(process.env, context.secrets, taken);
  if (engine === undefined) return;
  context.subscriptions.push(engine);
  for (const feature of features) feature(context, engine);
}

/** Nothing to stop here: the bridge is one of the context's subscriptions. */
export function deactivate(): void {}
