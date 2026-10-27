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

const MESSAGE_BYTES_MAX = 4096;
const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

/** Names the agent whose work a change set holds, a sub-agent by the engine's label such as
 * "Ada (@onboarding-fix) · Claude Code". Input: the change set. Output: the name. */
export function agentName(set: ChangeSet): string {
  return set.agent.kind === "master" ? "Master" : displayText(set.agent.label);
}

/** The size of a change set in files and lines. Input: the change set. Output: the line. */
export function sizeLine(set: ChangeSet): string {
  return `${plural(set.files, "file", "files")}, ${plural(set.added, "line", "lines")} added, ${set.removed} removed`;
}

/** The status word and tone of a change set's state. Input: the change set. */
export function stateBadge(set: ChangeSet): { word: string; tone: Tone } {
  return set.state === "updating" && set.waiting ? { word: "Updated, waiting for the agent", tone: "warn" } : STATES[set.state];
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

/** What the owner is reviewing of one change set: its id, the head its diff pages are read
 * at and that Accept and Discard send, whether that head moved on since the owner opened
 * it, and whether every page of the moved head was shown since. */
export interface Review {
  id: string;
  head: string;
  moved: boolean;
  seenAll: boolean;
}

/** Starts or carries the owner's review as the list rereads: a different change set starts
 * a fresh review, a new head on the same one marks it moved and unseen, anything else keeps
 * the review as it is. Inputs: the review so far, or null, and the change set shown now.
 * Output: the review, the same object when nothing changed. */
export function reviewOf(previous: Review | null, set: ChangeSet): Review {
  if (previous === null || previous.id !== set.id) return { id: set.id, head: set.head, moved: false, seenAll: false };
  if (previous.head !== set.head) return { id: set.id, head: set.head, moved: true, seenAll: false };
  return previous;
}

/** Records that the last diff page read at `head` was shown. Inputs: the review and the
 * head that page was read at. Output: the review, unchanged for a page of another head. */
export function lastPageSeen(review: Review, head: string): Review {
  return review.head === head && !review.seenAll ? { ...review, seenAll: true } : review;
}

/** Whether Accept and Discard may act on what the owner reviewed: not after the change set
 * moved on until every page of its new head was shown. Input: the review. Output: true when
 * they may act. */
export function reviewed(review: Review): boolean {
  return !review.moved || review.seenAll;
}

/** The note shown while a change set moved on since the owner opened it and its new head is
 * not yet read to the last page. Input: the review. Output: the note, or null. */
export function movedNote(review: Review): string | null {
  return reviewed(review) ? null : "These changes moved on since you opened them. Review them again: Accept and Discard come back once you reach the last page.";
}

/** The actions a change set's state allows now. Inputs: the change set and now in
 * milliseconds. Output: which of Accept, Discard, Ask agent to update and Restore apply. */
export function actions(set: ChangeSet, now: number): ChangeActions {
  const open = set.state === "pending" || set.state === "conflict";
  const waiting = set.state === "updating" && set.waiting;
  return {
    accept: open,
    discard: open || waiting,
    update: set.state === "conflict" || waiting,
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
  if (set.state !== "updating") return null;
  return set.waiting
    ? "Your branch is merged into the agent's copy, but the agent wasn't handed the work to resolve it. Ask it to update again, or discard."
    : "The agent is resolving the conflicts with your branch. Review again when it finishes.";
}

/** Names the files a conflicting change set clashes on. Input: the change set. Output:
 * the note, or null in any other state. */
export function conflictNote(set: ChangeSet): string | null {
  return set.state === "conflict" ? changesRefusal({ code: "changes_conflict", files: set.conflicts.map(displayText) }) : null;
}

/** The owner's edited commit message as Accept sends it: line ends made \n, trimmed.
 * Input: the editor text. Output: the message, or null when it is empty or longer than
 * the engine takes. */
export function commitMessage(text: string): string | null {
  const message = text.replace(/\r\n/g, "\n").trim();
  return message !== "" && new TextEncoder().encode(message).length <= MESSAGE_BYTES_MAX ? message : null;
}

/** What a landed Accept did, in one sentence; a conflict is told by the change set's own
 * note instead. Input: the engine's outcome. Output: the line, or null for a conflict. */
export function acceptNote(outcome: AcceptOutcome): string | null {
  return outcome.kind === "landed" ? `Accepted as ${outcome.commit.slice(0, 7)} on ${displayText(outcome.branch)}.` : null;
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
