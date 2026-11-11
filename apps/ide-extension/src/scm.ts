import * as vscode from "vscode";
import type { Engine, Reply } from "./engine.js";
import {
  type AcceptOutcome,
  BASE_SCHEME,
  CONFIG_SCHEME,
  HEAD_SCHEME,
  type IdeChangeSet,
  type ScmGroup,
  type ScmResource,
  acceptedNote,
  plainLine,
  refusal,
  scmGroups,
} from "./scm-model.js";

const POLL_MS = 5_000;
const PROBLEM_GROUP = "crazyagent.problem";
const UNREACHABLE = "CrazyAgent couldn't reach the engine. The changes come back here when it is running.";
const DISCARD_ASK = "Discard these changes? You can restore them for 7 days.";
const DISCARD = "Discard";
const DISCARDED = "Discarded. You can restore these changes for 7 days.";
const RESTORED = "Restored. The changes are ready to review again.";
const ICONS: Record<string, string> = { A: "diff-added", M: "diff-modified", D: "diff-removed", R: "diff-renamed" };

interface Row {
  id: string;
  label: string;
  states: vscode.SourceControlResourceState[];
}

/** Shows each change set of this project as a Source Control group named for its agent,
 * whose files open VS Code's own diff from the base text to the text at the head the set
 * recorded, which is what Accept lands. Accept,
 * Discard and Restore ask the engine, which lands or refuses; the IDE never merges by
 * itself. The list follows the engine's change event and a slow poll. Inputs: the
 * extension context and the engine client. Output: none; all is disposed with the extension. */
export function registerChanges(context: vscode.ExtensionContext, engine: Engine): void {
  const control = vscode.scm.createSourceControl("crazyagent", "CrazyAgent changes");
  control.inputBox.visible = false;
  const groups = new Map<string, vscode.SourceControlResourceGroup>();
  const shownGroups = new Map<string, ScmGroup>();
  const sets = new Map<string, IdeChangeSet>();
  let loading = false;
  let again = false;

  const show = (rows: Row[]): void => {
    const current = [...groups.keys()];
    const same = rows.length === current.length && rows.every((row, at) => row.id === current[at]);
    if (!same) {
      for (const group of groups.values()) group.dispose();
      groups.clear();
      for (const row of rows) {
        const group = control.createResourceGroup(row.id, row.label);
        group.hideWhenEmpty = false;
        groups.set(row.id, group);
      }
    }
    for (const row of rows) {
      const group = groups.get(row.id);
      if (group === undefined) throw new Error("every row has its Source Control group");
      group.label = row.label;
      group.resourceStates = row.states;
    }
    control.count = [...shownGroups.values()].reduce((count, shown) => count + (shown.accept ? shown.resources.length : 0), 0);
  };

  const problem = (label: string): void => {
    sets.clear();
    shownGroups.clear();
    show([{ id: PROBLEM_GROUP, label, states: [] }]);
  };

  const load = async (): Promise<void> => {
    let reply: Reply<IdeChangeSet[]>;
    try {
      reply = await engine.request<IdeChangeSet[]>("ide.change_sets", {});
    } catch {
      problem(UNREACHABLE);
      return;
    }
    if (reply.kind !== "ok") return problem(refusal(reply.code));
    if (!Array.isArray(reply.data)) return problem(refusal("changes_outcome_unknown"));
    const listed = scmGroups(reply.data, Date.now());
    sets.clear();
    shownGroups.clear();
    for (const set of reply.data) sets.set(set.id, set);
    const rows = listed.map((shown) => {
      const set = sets.get(shown.set);
      if (set === undefined) throw new Error("every group names a listed change set");
      shownGroups.set(shown.id, shown);
      return { id: shown.id, label: shown.label, states: shown.resources.map((resource) => resourceState(set, resource)) };
    });
    show(rows);
  };

  const refresh = (): void => {
    if (loading) {
      again = true;
      return;
    }
    loading = true;
    void load().finally(() => {
      loading = false;
      if (again) {
        again = false;
        refresh();
      }
    });
  };

  const target = (group: vscode.SourceControlResourceGroup | undefined, allowed: (shown: ScmGroup) => boolean): IdeChangeSet | null => {
    const shown = group === undefined ? undefined : shownGroups.get(group.id);
    const set = shown === undefined ? undefined : sets.get(shown.set);
    if (shown === undefined || set === undefined || !allowed(shown)) {
      void vscode.window.showErrorMessage(refusal("changes_not_found"));
      return null;
    }
    return set;
  };

  const ask = async <T>(name: string, args: Record<string, unknown>): Promise<{ value: T } | null> => {
    try {
      const reply = await engine.request<T>(name, args);
      if (reply.kind === "ok") return { value: reply.data };
      void vscode.window.showErrorMessage(refusal(reply.code));
      return null;
    } catch {
      void vscode.window.showErrorMessage(refusal("changes_outcome_unknown"));
      return null;
    } finally {
      refresh();
    }
  };

  const accept = async (group?: vscode.SourceControlResourceGroup): Promise<void> => {
    const set = target(group, (shown) => shown.accept);
    if (set === null) return;
    const message = await vscode.window.showInputBox({
      title: `Accept ${plainLine(set.agent)}: ${plainLine(set.title)}`,
      prompt: "Commit message. CrazyAgent lands these changes on your branch as one commit.",
      value: set.message,
      ignoreFocusOut: true,
      validateInput: (text) => (text.trim() === "" ? "Write a commit message." : null),
    });
    if (message === undefined) return;
    const done = await ask<AcceptOutcome>("ide.accept", { id: set.id, head: set.head, message });
    if (done === null) return;
    const note = acceptedNote(done.value);
    if (done.value.kind === "landed") void vscode.window.showInformationMessage(note);
    else void vscode.window.showWarningMessage(note);
  };

  const discard = async (group?: vscode.SourceControlResourceGroup): Promise<void> => {
    const set = target(group, (shown) => shown.discard);
    if (set === null) return;
    const detail = `${plainLine(set.agent)}: ${plainLine(set.title)}`;
    const choice = await vscode.window.showWarningMessage(DISCARD_ASK, { modal: true, detail }, DISCARD);
    if (choice !== DISCARD) return;
    const done = await ask<number>("ide.discard", { id: set.id, head: set.head });
    if (done !== null) void vscode.window.showInformationMessage(DISCARDED);
  };

  const restore = async (group?: vscode.SourceControlResourceGroup): Promise<void> => {
    const set = target(group, (shown) => shown.restore);
    if (set === null) return;
    const done = await ask<null>("ide.restore", { id: set.id });
    if (done !== null) void vscode.window.showInformationMessage(RESTORED);
  };

  const timer = setInterval(refresh, POLL_MS);
  context.subscriptions.push(
    control,
    { dispose: () => clearInterval(timer) },
    engine.on("change_sets_changed", refresh),
    vscode.commands.registerCommand("crazyagent.changes.accept", accept),
    vscode.commands.registerCommand("crazyagent.changes.discard", discard),
    vscode.commands.registerCommand("crazyagent.changes.restore", restore),
    vscode.commands.registerCommand("crazyagent.changes.refresh", refresh),
  );
  refresh();
}

/** One file of a change set as a Source Control row: its status icon and tooltip, struck
 * through when deleted, opening the diff from the base text to the recorded head's text,
 * the base text or the file. Both addresses carry the head, so a set that moved on opens
 * fresh text. An IDE settings file's agent side is the engine's read-only copy at the head
 * read as settings, never the worktree file. Inputs: the set and the file's row. Output:
 * the row. */
function resourceState(set: IdeChangeSet, resource: ScmResource): vscode.SourceControlResourceState {
  const file = vscode.Uri.file(resource.file);
  const base = resource.base === null ? null : vscode.Uri.from({ scheme: BASE_SCHEME, path: resource.base, query: set.head });
  const head = resource.head === null ? null : vscode.Uri.from({ scheme: HEAD_SCHEME, path: resource.head, query: set.head });
  const right = resource.config === null ? head : vscode.Uri.from({ scheme: CONFIG_SCHEME, path: resource.config, query: set.head });
  const name = plainLine(resource.path);
  let command: vscode.Command | undefined;
  if (resource.open === "diff" && base !== null && right !== null) {
    command = { command: "vscode.diff", title: "Open changes", arguments: [base, right, `${name} (${plainLine(set.agent)})`] };
  } else if (resource.open === "base" && base !== null) {
    command = { command: "vscode.open", title: "Open the file before the agent's changes", arguments: [base] };
  } else if (resource.open === "file") {
    command = { command: "vscode.open", title: "Open the file", arguments: [file] };
  }
  return {
    resourceUri: file,
    command,
    decorations: {
      strikeThrough: resource.strikeThrough,
      tooltip: resource.tooltip,
      iconPath: new vscode.ThemeIcon(ICONS[resource.letter] ?? "diff-modified"),
    },
  };
}
