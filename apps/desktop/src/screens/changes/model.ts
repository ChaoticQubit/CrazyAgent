import { changesRefusal } from "../../api-changes";
import type { AcceptOutcome, ChangedFile, ChangeSet, FileStatus } from "../../api-changes";
import type { Tone } from "../../ui";
import { displayText } from "../conversation";

export interface ChangeActions {
  accept: boolean;
  discard: boolean;
  update: boolean;
  restore: boolean;
}

const STATES: Record<ChangeSet["state"], { word: string; tone: Tone }> = {
  pending: { word: "Ready to review", tone: "info" },
  conflict: { word: "Conflicts with your branch", tone: "warn" },
  updating: { word: "Agent is updating", tone: "info" },
  accepted: { word: "Accepted", tone: "ok" },
  discarded: { word: "Discarded", tone: "neutral" },
};

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

const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

/** Names the agent whose work a change set holds. Input: the change set. Output: the name. */
export function agentName(set: ChangeSet): string {
  return set.agent.kind === "master" ? "Master" : `Sub-agent ${displayText(set.agent.label)}`;
}

/** The size of a change set in files and lines. Input: the change set. Output: the line. */
export function sizeLine(set: ChangeSet): string {
  return `${plural(set.files, "file", "files")}, ${plural(set.added, "line", "lines")} added, ${set.removed} removed`;
}

/** The status word and tone of a change set's state. Input: the change set. */
export function stateBadge(set: ChangeSet): { word: string; tone: Tone } {
  return STATES[set.state];
}

/** Says plainly that CrazyAgent did not check a change set. Input: the change set.
 * Output: the note, or null once the engine has checked it. */
export function checkedNote(set: ChangeSet): string | null {
  return set.checked ? null : "Not checked by CrazyAgent. Any test results in it are reported by the agent.";
}

/** Says how the agent's last turn ended when it did not succeed. Input: the change set.
 * Output: the note, or null after a turn that succeeded. */
export function outcomeNote(set: ChangeSet): string | null {
  if (set.last_outcome === "failed") return "The agent's last turn failed. These are the changes it made before it stopped.";
  if (set.last_outcome === "cancelled") return "The agent's last turn was stopped. These are the changes it made before it stopped.";
  return null;
}

/** The actions a change set's state allows now. Inputs: the change set and now in
 * milliseconds. Output: which of Accept, Discard, Ask agent to update and Restore apply. */
export function actions(set: ChangeSet, now: number): ChangeActions {
  const open = set.state === "pending" || set.state === "conflict";
  return {
    accept: open,
    discard: open,
    update: set.state === "conflict",
    restore: set.state === "discarded" && restorable(set, now),
  };
}

function restorable(set: ChangeSet, now: number): boolean {
  return set.restorable_until_ms !== null && set.restorable_until_ms > now;
}

/** Says until when discarded changes can be restored. Inputs: the change set and now in
 * milliseconds. Output: the note, or null for a change set that is not discarded. */
export function restoreNote(set: ChangeSet, now: number): string | null {
  if (set.state !== "discarded") return null;
  if (!restorable(set, now)) return "These changes can no longer be restored.";
  const until = new Date(set.restorable_until_ms ?? now).toLocaleString("en-US", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  return `You can restore these changes until ${until}.`;
}

/** Says what happens while the agent resolves conflicts. Input: the change set. Output:
 * the note, or null in any other state. */
export function updatingNote(set: ChangeSet): string | null {
  return set.state === "updating" ? "The agent is resolving the conflicts with your branch. Review again when it finishes." : null;
}

/** Names the files a conflicting change set clashes on. Input: the change set. Output:
 * the note, or null in any other state. */
export function conflictNote(set: ChangeSet): string | null {
  return set.state === "conflict" ? changesRefusal({ code: "changes_conflict", files: set.conflicts.map(displayText) }) : null;
}

/** What an Accept did, in one sentence. Input: the engine's outcome. Output: the line. */
export function acceptNote(outcome: AcceptOutcome): string {
  return outcome.kind === "landed"
    ? `Accepted as ${outcome.commit.slice(0, 7)} on ${displayText(outcome.branch)}.`
    : changesRefusal({ code: "changes_conflict", files: outcome.files.map(displayText) });
}

/** One line for a changed file: how it changed and its path. Input: the file. */
export function fileLine(file: ChangedFile): string {
  const path = displayText(file.path);
  if (file.status === "renamed" && file.old_path) return `Renamed ${displayText(file.old_path)} to ${path}`;
  return `${STATUS_WORDS[file.status] ?? "Changed"} ${path}`;
}

/** The marks of a file that can change more than its text, each once, in plain words; a
 * mark this build does not know is shown by its name. Input: the file. Output: the marks. */
export function fileMarks(file: ChangedFile): string[] {
  const words = [KIND_MARKS[file.kind], ...file.marks.map((mark) => MARKS[mark] ?? `Marked ${displayText(mark).replaceAll("_", " ")}`)];
  return [...new Set(words.filter((word): word is string => word !== undefined))];
}

/** Diff text made safe to show as plain text: control characters, such as terminal color
 * codes, become visible escapes; newlines and tabs stay. Input: the text. Output: the text. */
export function diffText(text: string): string {
  return displayText(text);
}
