import type * as vscode from "vscode";
import { registerBaseContent } from "./base-content.js";
import { registerComments } from "./comments.js";
import { Engine } from "./engine.js";
import { extensions } from "./extensions.js";
import { remoteGit } from "./git.js";
import { startLock } from "./lock.js";
import { readonly } from "./readonly.js";
import { reveal } from "./reveal.js";
import { sidebarAgents } from "./sidebar/agents.js";
import { sidebarChanges } from "./sidebar/changes.js";
import { sidebarContext } from "./sidebar/context.js";
import { sidebarMemories } from "./sidebar/memories.js";
import { registerChanges } from "./scm.js";
import { sandboxedTerminal, terminalGit, terminalMenu } from "./terminal.js";
import { registerTrust } from "./trust.js";

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
  sandboxedTerminal,
  terminalMenu,
  terminalGit,
  registerTrust,
  remoteGit,
];

/**
 * Starts the settings lock, then every feature when the CrazyAgent engine launched this IDE
 * server, which it shows by setting the capability and socket variables. The bridge keeps
 * its session token in the context's secret storage.
 * Input: the extension context. Output: none; the bridge closes with the context.
 */
export function activate(context: vscode.ExtensionContext): void {
  startLock(context, process.env);
  const engine = Engine.fromEnv(process.env, context.secrets);
  if (engine === undefined) return;
  context.subscriptions.push(engine);
  for (const feature of features) feature(context, engine);
}

/** Nothing to stop here: the bridge is one of the context's subscriptions. */
export function deactivate(): void {}
