import * as vscode from "vscode";
import type { Engine, Reply } from "../engine.js";
import { unconfirmed } from "../scm-model.js";
import type { Row } from "./model.js";

/**
 * A sidebar tree over model rows: groups open expanded, every row's kind becomes its
 * context value for menus, and `decorate` adds a leaf's icon, tooltip and click command.
 * `show` replaces the rows; `say` puts a message above the tree, or clears it.
 */
export class RowTree implements vscode.TreeDataProvider<Row>, vscode.Disposable {
  readonly view: vscode.TreeView<Row>;
  readonly #changed = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.#changed.event;
  readonly #decorate: (row: Row, item: vscode.TreeItem) => void;
  #rows: Row[] = [];

  constructor(id: string, decorate: (row: Row, item: vscode.TreeItem) => void) {
    this.#decorate = decorate;
    this.view = vscode.window.createTreeView(id, { treeDataProvider: this });
  }

  show(rows: Row[]): void {
    this.#rows = rows;
    this.#changed.fire();
  }

  say(message: string | undefined): void {
    this.view.message = message;
  }

  getChildren(row?: Row): Row[] {
    return row === undefined ? this.#rows : row.children;
  }

  getTreeItem(row: Row): vscode.TreeItem {
    const state = row.kind === "group" ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None;
    const item = new vscode.TreeItem(row.label, state);
    item.id = row.key;
    item.description = row.description;
    item.contextValue = row.kind;
    if (row.kind !== "group") this.#decorate(row, item);
    return item;
  }

  dispose(): void {
    this.view.dispose();
    this.#changed.dispose();
  }
}

/**
 * Runs `load` again after it finishes whenever `refresh` was called while it ran, so a
 * burst of change events costs at most one extra load. Input: the load, which shows its
 * own failures. Output: the refresh function.
 */
export function coalesced(load: () => Promise<void>): () => void {
  let running = false;
  let again = false;
  const refresh = (): void => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    again = false;
    const done = (): void => {
      running = false;
      if (again) refresh();
    };
    load().then(done, done);
  };
  return refresh;
}

/**
 * The words to show for a reply that is not `ok`, or for a request that got no reply.
 * Input: the reply, or null when none came. Output: one plain sentence.
 */
export function trouble(reply: Reply<unknown> | null): string {
  if (reply === null) return "CrazyAgent is not reachable. It will try again on the next change.";
  if (reply.kind === "stale") return "This changed in CrazyAgent. The list now shows the current version.";
  if (reply.kind === "rejected") return unconfirmed(reply.code) ?? `CrazyAgent refused this (${reply.code}).`;
  return "";
}

/**
 * Sends one bridge call and turns a broken connection into a null reply, so callers only
 * branch on the reply. Input: the engine, the call and its arguments. Output: the reply or
 * null.
 */
export async function ask<T>(engine: Engine, name: string, args: Record<string, unknown> = {}): Promise<Reply<T> | null> {
  try {
    return await engine.request<T>(name, args);
  } catch {
    return null;
  }
}
