import * as vscode from "vscode";
import type { Engine } from "../engine.js";
import { memoryGroups, nextPage, type Memory, type Row } from "./model.js";
import { RowTree, ask, coalesced, trouble } from "./tree.js";

const ICONS: Record<string, string> = { "memory.pending": "lightbulb", "memory.approved": "pass", "memory.aside": "circle-slash" };

/**
 * The Memories view: the project's memories grouped by state, with Approve and Reject on
 * the ones waiting for the owner. Each decision goes to the engine with the revision the
 * owner saw and only asks: the owner confirms it in the CrazyAgent window, which the IDE
 * cannot click. The extension never writes memory itself. Refreshes on `memories_changed`.
 * Input: the extension context and the engine bridge. Output: none.
 */
export function sidebarMemories(context: vscode.ExtensionContext, engine: Engine): void {
  const tree = new RowTree("crazyagent.memories", (row, item) => {
    item.iconPath = new vscode.ThemeIcon(ICONS[row.kind] ?? "circle-outline");
    item.tooltip = row.memory?.text;
  });
  const load = async (): Promise<void> => {
    const memories: Memory[] = [];
    let after: string | null = null;
    for (let pages = 1; ; pages += 1) {
      const reply = await ask<Memory[]>(engine, "ide.memories", after === null ? {} : { after });
      if (reply?.kind !== "ok") {
        tree.say(trouble(reply));
        return;
      }
      memories.push(...reply.data);
      after = nextPage(reply.data, pages);
      if (after === null) break;
    }
    tree.say(memories.length === 0 ? "No memories yet." : undefined);
    tree.show(memoryGroups(memories));
  };
  const refresh = coalesced(load);
  const decide = (decision: "approve" | "reject") => async (row?: Row): Promise<void> => {
    const memory = row?.memory;
    if (memory === undefined || memory.state !== "pending") return;
    const reply = await ask(engine, "ide.memory_decide", { id: memory.id, revision: memory.revision, decision });
    if (reply?.kind !== "ok") void vscode.window.showWarningMessage(trouble(reply));
    else void vscode.window.showInformationMessage(`Confirm in the CrazyAgent window to ${decision === "approve" ? "save" : "reject"} this memory.`);
    refresh();
  };
  context.subscriptions.push(
    tree,
    engine.on("memories_changed", refresh),
    vscode.commands.registerCommand("crazyagent.memories.approve", decide("approve")),
    vscode.commands.registerCommand("crazyagent.memories.reject", decide("reject")),
    vscode.commands.registerCommand("crazyagent.memories.refresh", refresh),
  );
  refresh();
}
