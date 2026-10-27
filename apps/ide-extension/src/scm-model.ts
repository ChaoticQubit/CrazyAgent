export type ChangeState = "pending" | "conflict" | "updating" | "accepted" | "discarded";
export type FileStatus = "added" | "modified" | "deleted" | "renamed";
export type FileKind = "text" | "binary" | "symlink" | "gitlink";

export interface IdeFile {
  path: string;
  old_path?: string | null;
  status: FileStatus;
  kind: FileKind;
  marks: string[];
}

export interface IdeChangeSet {
  id: string;
  agent: string;
  role: "master" | "sub";
  title: string;
  state: ChangeState;
  waiting: boolean;
  head: string;
  message: string;
  worktree: string;
  files: IdeFile[];
  restorable_until: number | null;
  problem: string | null;
}

export type AcceptOutcome = { kind: "landed"; commit: string; branch: string } | { kind: "conflict"; files: string[] };

export type Reply<T> = { kind: "ok"; value: T } | { kind: "error"; code: string };

export interface Engine {
  request<T>(name: string, args: Record<string, unknown>): Promise<Reply<T>>;
  on(event: string, handler: () => void): { dispose(): unknown };
}

export type Opens = "diff" | "base" | "file" | "none";

export interface ScmResource {
  path: string;
  file: string;
  base: string | null;
  open: Opens;
  letter: string;
  tooltip: string;
  strikeThrough: boolean;
}

export type GroupContext = "open" | "waiting" | "resolving" | "restorable";

export interface ScmGroup {
  id: string;
  set: string;
  label: string;
  context: GroupContext;
  accept: boolean;
  discard: boolean;
  restore: boolean;
  resources: ScmResource[];
}

export const BASE_SCHEME = "crazyagent-base";

const SET_ID = /^[0-9a-f]{32}$/;
const CONFLICT_SHOWN = 5;
const UNSAFE = /[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

const LETTERS: Record<FileStatus, string> = { added: "A", modified: "M", deleted: "D", renamed: "R" };
const STATUS_WORDS: Record<FileStatus, string> = { added: "Added", modified: "Modified", deleted: "Deleted", renamed: "Renamed" };
const KIND_MARKS: Record<string, string> = {
  binary: "Binary file, no text diff",
  symlink: "Symbolic link: points to another path",
  gitlink: "Submodule commit",
};
const MARKS: Record<string, string> = {
  symlink: KIND_MARKS.symlink,
  executable: "Can run as a program",
  mode: "File mode changed",
  attributes: ".gitattributes: changes how Git treats files",
  modules: ".gitmodules: changes the project's submodules",
};
const SUFFIXES: Record<GroupContext, string> = {
  open: "",
  waiting: " (updated, waiting for the agent)",
  resolving: " (the agent is updating it)",
  restorable: " (discarded, can be restored)",
};
const CONFLICT_SUFFIX = " (conflicts with your branch)";

const REFUSALS: Record<string, string> = {
  changes_owner_dirty: "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again.",
  changes_detached_head: "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again.",
  changes_git_busy: "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again.",
  changes_conflict: "These changes conflict with your branch. Ask the agent to update, or discard.",
  changes_stale: "These changes moved on since you opened them. Review them again, then accept.",
  changes_branch_moved: "Your branch moved while these changes were landing, so nothing changed. Review and accept again.",
  changes_git_unsupported: "The Git on this computer can't land changes. Update Git, then try again.",
  changes_ignored_files: "These changes add files your project's ignore rules leave out. Ask the agent to remove them.",
  changes_message_invalid: "The commit message is empty, too long or has characters Git can't keep. Edit it, then accept again.",
  changes_restore_expired: "These changes can no longer be restored. Discarded changes are kept for 7 days.",
  changes_agent_has_pending: "This agent has newer changes waiting. Accept or discard those first.",
  changes_agent_busy: "The agent is working right now. Try again when it finishes.",
  changes_unavailable: "Landing changes isn't ready in this engine yet.",
  changes_git_failed: "Git couldn't finish this step in your project folder. Try again; if it keeps failing, check the folder with Git.",
  changes_not_found: "These changes are no longer there. The list shows what is left.",
  changes_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  changes_owner_identity_missing: "Git has no name and email for you in your project folder. Set user.name and user.email, then accept again.",
  changes_owner_unavailable: "Your project folder couldn't be read. Check that it is still there, then try again.",
  changes_too_large: "These changes are larger than CrazyAgent can show or land. Ask the agent to split them up.",
  changes_workspace_invalid: "The agent's working copy was moved or changed outside CrazyAgent, so these changes can't land from it.",
};

/** The Changes codes the IDE has its own sentence for, the same words as the desktop. */
export const REFUSAL_CODES = Object.keys(REFUSALS);

/** Shows engine or agent text as one plain line: control and direction-changing characters
 * become a replacement mark. Input: the text. Output: the line. */
export function plainLine(text: string): string {
  return text.replace(UNSAFE, "\ufffd");
}

/** One plain sentence for a refused Changes request. Input: the engine code. Output: the
 * desktop's sentence, or the plain fallback naming the code. */
export function refusal(code: string): string {
  return Object.hasOwn(REFUSALS, code) ? REFUSALS[code] : `Something went wrong. Try again. (${plainLine(code)})`;
}

/** What an Accept did, in the desktop's words. Input: the engine's outcome. Output: the
 * landed commit and branch, or the conflicting files, at most five named. */
export function acceptedNote(outcome: AcceptOutcome): string {
  if (outcome.kind === "landed") return `Accepted as ${outcome.commit.slice(0, 7)} on ${plainLine(outcome.branch)}.`;
  const shown = outcome.files.slice(0, CONFLICT_SHOWN).map(plainLine).join(", ");
  const more = outcome.files.length > CONFLICT_SHOWN ? ` and ${outcome.files.length - CONFLICT_SHOWN} more` : "";
  return `These files also changed on your branch: ${shown}${more}. Ask the agent to update, or discard.`;
}

/** The path of a base-side document: the change set and the file. Inputs: the set id and
 * the file's path in the change set. Output: the URI path. */
export function basePath(id: string, path: string): string {
  return `/${id}/${path}`;
}

/** Reads a base-side document path back, refusing anything but a change set id and a
 * plain relative path. Input: the URI path. Output: the set and file, or null. */
export function parseBasePath(text: string): { id: string; path: string } | null {
  const match = /^\/([^/]+)\/(.+)$/s.exec(text);
  if (match === null || !SET_ID.test(match[1])) return null;
  const parts = match[2].split("/");
  if (parts.some((part) => part === "" || part === "." || part === "..")) return null;
  return { id: match[1], path: match[2] };
}

function context(set: IdeChangeSet, now: number): GroupContext | null {
  switch (set.state) {
    case "pending":
    case "conflict":
      return "open";
    case "updating":
      return set.waiting ? "waiting" : "resolving";
    case "discarded":
      return set.restorable_until !== null && set.restorable_until > now ? "restorable" : null;
    case "accepted":
      return null;
  }
}

function opens(file: IdeFile): Opens {
  if (file.kind !== "text") return file.status === "deleted" ? "none" : "file";
  return file.status === "deleted" ? "base" : "diff";
}

function tooltip(file: IdeFile): string {
  const status = file.status === "renamed" && file.old_path ? `Renamed from ${plainLine(file.old_path)}` : STATUS_WORDS[file.status];
  const marks = [KIND_MARKS[file.kind], ...file.marks.map((mark) => MARKS[mark] ?? `Marked ${plainLine(mark).replaceAll("_", " ")}`)];
  return [...new Set([status, ...marks.filter((mark): mark is string => mark !== undefined)])].join(", ");
}

function resource(set: IdeChangeSet, file: IdeFile): ScmResource {
  const open = opens(file);
  return {
    path: file.path,
    file: `${set.worktree.replace(/\/+$/, "")}/${file.path}`,
    base: open === "diff" || open === "base" ? basePath(set.id, file.path) : null,
    open,
    letter: LETTERS[file.status],
    tooltip: tooltip(file),
    strikeThrough: file.status === "deleted",
  };
}

/** The Source Control groups for the engine's change sets, in the engine's order: one per
 * set that has one, named "<agent>: <title>" with its state when not open, carrying the
 * actions the desktop offers for that state. Inputs: the sets and now in milliseconds.
 * Output: the groups. */
export function scmGroups(sets: IdeChangeSet[], now: number): ScmGroup[] {
  const groups: ScmGroup[] = [];
  for (const set of sets) {
    const shown = context(set, now);
    if (shown === null) continue;
    const name = `${plainLine(set.agent)}: ${plainLine(set.title) || "Changes"}`;
    const state = set.state === "conflict" ? CONFLICT_SUFFIX : SUFFIXES[shown];
    const problem = set.problem === null ? "" : " (CrazyAgent couldn't read these changes)";
    groups.push({
      id: `crazyagent.${set.id}.${shown}`,
      set: set.id,
      label: `${name}${state}${problem}`,
      context: shown,
      accept: shown === "open",
      discard: shown === "open" || shown === "waiting",
      restore: shown === "restorable",
      resources: set.problem === null ? set.files.map((file) => resource(set, file)) : [],
    });
  }
  return groups;
}
