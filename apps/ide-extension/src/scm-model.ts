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

export interface BasePage {
  text: string;
  next: number | null;
}

export type Opens = "diff" | "base" | "file" | "none";

export interface ScmResource {
  path: string;
  file: string;
  base: string | null;
  config: string | null;
  head: string | null;
  open: Opens;
  letter: string;
  tooltip: string;
  strikeThrough: boolean;
}

export type GroupContext = "open" | "unread" | "waiting" | "resolving" | "restorable";

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

/** The scheme of an agent's IDE settings file as its change set has it, served by the
 * engine because the IDE server cannot read agent `.vscode` or `*.code-workspace` files. */
export const CONFIG_SCHEME = "crazyagent-config";

/** The mark the engine puts on a file the IDE reads as its settings. */
export const IDE_CONFIG = "ide_config";

/** The scheme of a diff's agent side: the file's text at the head the change set recorded,
 * which Accept lands, served by the engine and never the agent's live worktree. */
export const HEAD_SCHEME = "crazyagent-head";

/** What a row that opens the agent's worktree file adds to its tooltip: the engine serves
 * only text at the recorded head, so a binary file, link or submodule opens as the agent
 * has it now, which may have changed since the set was recorded. */
export const LIVE_FILE = "Opens the agent's current file: Accept lands the version it recorded";

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
  ide_config: "IDE settings: changes how the IDE behaves in this project",
};
const SUFFIXES: Record<GroupContext, string> = {
  open: "",
  unread: "",
  waiting: " (updated, waiting for the agent)",
  resolving: " (the agent is updating it)",
  restorable: " (discarded, can be restored)",
};
const CONFLICT_SUFFIX = " (conflicts with your branch)";
const UNREAD = " (CrazyAgent couldn't read these changes)";
const TOO_LARGE = " (too large to show here)";

const REFUSALS: Record<string, string> = {
  changes_owner_dirty: "Files in your project folder that these changes touch, or that you staged, have uncommitted changes. Commit or stash them, then accept again.",
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

const UNCONFIRMED: Record<string, string> = {
  ide_comment_outcome_unknown: "CrazyAgent couldn't confirm the agent got this comment. Check the agent's terminal before you send it again.",
  ide_owner_edit_outcome_unknown: "CrazyAgent couldn't confirm the file was opened for your edit. The agent may already have been told. Check before you try again.",
  ide_agent_message_outcome_unknown: "CrazyAgent couldn't confirm the agent got this message. Check its terminal before you send it again.",
  ide_memory_decide_outcome_unknown: "CrazyAgent couldn't confirm your decision on this memory. Check the CrazyAgent window before you decide again.",
  ide_extension_decide_outcome_unknown: "CrazyAgent couldn't confirm your decision on this update. Check the CrazyAgent app before you decide again.",
  ide_extension_request_outcome_unknown: "CrazyAgent couldn't confirm it got your request for this extension. Check the CrazyAgent app before you ask again.",
  ide_show_all_agents_outcome_unknown: "CrazyAgent couldn't confirm the agent folders changed. Check the Explorer before you try again.",
  ide_git_outcome_unknown: "CrazyAgent couldn't confirm this git command finished. It may have run, so check the branch and its remote before you run it again.",
};

/** The sentence for an IDE write the engine could not confirm in time: it may already have
 * taken effect, so the owner checks before trying again. Input: the engine code. Output:
 * the sentence, or undefined for any other code. */
export function unconfirmed(code: string): string | undefined {
  return Object.hasOwn(UNCONFIRMED, code) ? UNCONFIRMED[code] : undefined;
}

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

/** The path of a base-side, head-side or IDE settings document: the change set and the
 * file. Inputs: the set id and the file's path in the change set. Output: the URI path. */
export function basePath(id: string, path: string): string {
  return `/${id}/${path}`;
}

/** Reads a base-side, head-side or IDE settings document path back, refusing anything but
 * a change set id and a plain relative path. Input: the URI path. Output: the set and file,
 * or null. */
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
  if (file.kind !== "text") return file.status === "deleted" || file.marks.includes(IDE_CONFIG) ? "none" : "file";
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
    config: open === "diff" && file.marks.includes(IDE_CONFIG) ? basePath(set.id, file.path) : null,
    head: open === "diff" ? basePath(set.id, file.path) : null,
    open,
    letter: LETTERS[file.status],
    tooltip: open === "file" ? `${tooltip(file)}, ${LIVE_FILE}` : tooltip(file),
    strikeThrough: file.status === "deleted",
  };
}

/** The Source Control groups for the engine's change sets, in the engine's order: one per
 * set that has one, named "<agent>: <title>" with its state when not open, carrying the
 * actions the desktop offers for that state; a set whose files are not shown cannot be
 * accepted. Inputs: the sets and now in milliseconds. Output: the groups. */
export function scmGroups(sets: IdeChangeSet[], now: number): ScmGroup[] {
  const groups: ScmGroup[] = [];
  for (const set of sets) {
    const state = context(set, now);
    if (state === null) continue;
    const shown = state === "open" && set.problem !== null ? "unread" : state;
    const name = `${plainLine(set.agent)}: ${plainLine(set.title) || "Changes"}`;
    const suffix = set.state === "conflict" ? CONFLICT_SUFFIX : SUFFIXES[shown];
    const problem = set.problem === null ? "" : set.problem === "changes_too_large" ? TOO_LARGE : UNREAD;
    groups.push({
      id: `crazyagent.${set.id}.${shown}`,
      set: set.id,
      label: `${name}${suffix}${problem}`,
      context: shown,
      accept: shown === "open",
      discard: shown === "open" || shown === "unread" || shown === "waiting",
      restore: shown === "restorable",
      resources: set.problem === null ? set.files.map((file) => resource(set, file)) : [],
    });
  }
  return groups;
}

const COMMENT_STATES: ReadonlySet<ChangeState> = new Set(["pending", "conflict", "updating"]);

/** The most UTF-8 bytes one comment may hold, as the engine bounds it, so with its file and
 * line it fits one typed agent prompt. */
export const COMMENT_BYTES_MAX = 2048;

const COMMENT_REFUSALS: Record<string, string> = {
  changes_closed: "These changes were already accepted or discarded, so the agent can't act on this comment.",
  ide_comment_master_unavailable: "Comments here reach team agents only. Write to this master in its conversation in CrazyAgent.",
  ide_comment_too_long: "This comment is too long. Shorten it and send it again.",
  team_agent_not_found: "That agent is no longer part of this team.",
  team_agent_not_running: "That agent isn't running, so it can't read this comment. Start it again, then send the comment.",
  team_agent_unreachable: "The agent's terminal didn't take this comment. Try again.",
  team_conversation_full: "Too many of your messages are waiting for the master. Send more once it has answered.",
  team_message_invalid: "The message is empty or too long.",
  project_setup_missing: "This project was created before project setup existed.",
};

/** What the owner sees once a comment went out. Input: the agent's name. Output: the
 * sentence naming it. */
export function commentSent(agent: string): string {
  return `Sent to ${plainLine(agent)} with its file and line.`;
}

/** Whether a comment is over the engine's bound, counted in UTF-8 bytes as the engine counts
 * it. Input: the comment. Output: true when it is too long to send. */
export function commentTooLong(text: string): boolean {
  return new TextEncoder().encode(text).length > COMMENT_BYTES_MAX;
}

/** The change set file a document is the agent side of, so a comment on it can go to that
 * set's agent: an open team agent's set whose files are shown, and a file it changed that
 * still exists. An owner conversation master's set takes no comment here, since the IDE
 * cannot reach that conversation. Inputs: the listed sets and the document's file path.
 * Output: the set, path and agent, or null. */
export function commentTarget(sets: IdeChangeSet[], fsPath: string): { id: string; path: string; agent: string } | null {
  for (const set of sets) {
    if (!COMMENT_STATES.has(set.state) || set.problem !== null || set.role !== "sub") continue;
    const root = `${set.worktree.replace(/\/+$/, "")}/`;
    if (!fsPath.startsWith(root)) continue;
    const path = fsPath.slice(root.length);
    if (set.files.some((file) => file.path === path && file.status !== "deleted")) return { id: set.id, path, agent: set.agent };
  }
  return null;
}

/** The change set file a head-side document shows, so a comment on the agent side of a diff
 * goes to that set's agent on the same terms as one on the worktree file. Inputs: the
 * listed sets and the document's URI path. Output: the set, path and agent, or null. */
export function headCommentTarget(sets: IdeChangeSet[], uriPath: string): { id: string; path: string; agent: string } | null {
  const shown = parseBasePath(uriPath);
  const set = shown === null ? undefined : sets.find((listed) => listed.id === shown.id);
  if (shown === null || set === undefined) return null;
  return commentTarget([set], `${set.worktree.replace(/\/+$/, "")}/${shown.path}`);
}

/** One plain sentence for a refused comment: the desktop's words for the team codes, the
 * Changes words otherwise. Input: the engine code. Output: the sentence. */
export function commentRefusal(code: string): string {
  return Object.hasOwn(COMMENT_REFUSALS, code) ? COMMENT_REFUSALS[code] : (unconfirmed(code) ?? refusal(code));
}
