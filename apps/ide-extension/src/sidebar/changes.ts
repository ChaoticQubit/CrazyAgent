import * as vscode from "vscode";
import type { Engine } from "../engine.js";
import { changeGroups, type ChangeSet } from "./model.js";
import { RowTree, ask, coalesced, trouble } from "./tree.js";

/**
 * The Change sets view: every change set that has a Source Control group, named the way
 * that group is; a click opens Source Control, where Accept, Discard and Restore live.
 * Refreshes on `change_sets_changed`. Input: the extension context and the engine bridge.
 * Output: none.
 */
export function sidebarChanges(context: vscode.ExtensionContext, engine: Engine): void {
  const tree = new RowTree("crazyagent.changeSets", (_row, item) => {
    item.iconPath = new vscode.ThemeIcon("git-pull-request");
    item.tooltip = "Open in Source Control";
    item.command = { command: "workbench.view.scm", title: "Open in Source Control" };
  });
  const load = async (): Promise<void> => {
    const reply = await ask<ChangeSet[]>(engine, "ide.change_sets");
    if (reply?.kind !== "ok") {
      tree.say(trouble(reply));
      return;
    }
    const rows = changeGroups(reply.data, Date.now());
    tree.say(rows.length === 0 ? "No change sets waiting." : undefined);
    tree.show(rows);
  };
  const refresh = coalesced(load);
  context.subscriptions.push(
    tree,
    engine.on("change_sets_changed", refresh),
    vscode.commands.registerCommand("crazyagent.changeSets.refresh", refresh),
  );
  refresh();
}
