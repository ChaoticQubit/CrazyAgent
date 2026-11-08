import { changesRefusal } from "../../api-changes";
import type { AcceptOutcome, ChangedFile, ChangeSet, FileStatus, TargetBranches } from "../../api-changes";
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
  owner_edited: "Edited by you",
  ide_config: "IDE settings: changes how the IDE behaves in this project",
  instructions: "Instruction file: changes what every agent of this project is told",
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

/** The status word and tone of a change set's state, saying when the engine closed it
 * because its work was landed or removed outside the app, and when its agent is fixing
 * conflicts with main (OS-8). Input: the change set. */
export function stateBadge(set: ChangeSet): { word: string; tone: Tone } {
  if (set.outside && set.state === "accepted") return { word: "Landed outside the app", tone: "ok" };
  if (set.outside && set.state === "discarded") return { word: "Removed outside the app", tone: "neutral" };
  if (set.state === "updating" && set.waiting) return { word: "Updated, waiting for the agent", tone: "warn" };
  if (set.state === "updating" && set.conflicts.length > 0) return { word: "Agent is fixing conflicts with main", tone: "info" };
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

/** Says until when discarded changes can be restored, or that work landed or removed
 * outside the app needs nothing more. Inputs: the change set and now in milliseconds.
 * Output: the note, or null for a change set that is neither discarded nor closed outside. */
export function restoreNote(set: ChangeSet, now: number): string | null {
  if (set.outside && set.state === "accepted") return "Your branch already has these changes. They were landed outside the app.";
  if (set.outside && set.state === "discarded") return "The agent's copy of these changes was removed outside the app, so they cannot be restored.";
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
 * Input: the editor text. Output: the message, or null when it is empty, longer than the
 * engine takes or holds a character displayText would show as an escape. */
export function commitMessage(text: string): string | null {
  const message = text.replace(/\r\n/g, "\n").trim();
  return message !== "" && new TextEncoder().encode(message).length <= MESSAGE_BYTES_MAX && displayText(message) === message ? message : null;
}

/** Where an Accept lands and how the dialog says it: the picked branch to send, null when it
 * is where the engine lands with no branch named, the team's Land on branch, else the
 * checked-out one, and the sentence naming that branch, or the engine's sentence naming a
 * missing Land on branch while none is picked. Inputs: the listed branches, null before they
 * load, and the picked branch, empty for none. Output: the branch and sentence. */
export function acceptTarget(branches: TargetBranches | null, target: string): { picked: string | null; text: string } {
  const problem = branches?.land_on_problem ?? null;
  if (problem !== null && target === "") return { picked: null, text: displayText(problem) };
  const fallback = branches === null || problem !== null ? null : branches.land_on ?? branches.current;
  const lands = target !== "" ? target : fallback;
  const picked = branches !== null && target !== "" && target !== fallback ? target : null;
  const where = lands === null
    ? "They land as one commit on your project's Land on branch, under your own Git name."
    : lands === branches?.current
      ? `They land as one commit on ${displayText(lands)}, the branch checked out in your project folder, under your own Git name.`
      : `They land as one commit on ${displayText(lands)}, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are.`;
  return { picked, text: `${where} CrazyAgent did not check them.` };
}

/** Whether the Accept dialog must wait for the owner to pick a branch: the Land on branch is
 * missing, so no branch is where the changes land by default. Inputs: the listed branches,
 * null before they load, and the picked branch, empty for none. Output: true to wait. */
export function acceptBlocked(branches: TargetBranches | null, target: string): boolean {
  return (branches?.land_on_problem ?? null) !== null && target === "";
}

/** One branch as the Accept dialog lists it, marked as the Land on branch and as checked out
 * where it is. Inputs: the listed branches and the name. Output: the label. */
export function branchLabel(branches: TargetBranches, name: string): string {
  const marks = [name === branches.land_on ? "Land on" : null, name === branches.current ? "checked out" : null].filter((mark) => mark !== null);
  return marks.length === 0 ? name : `${name} (${marks.join(", ")})`;
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

/** Counts the change sets waiting for the owner: ready to review or conflicting with their
 * branch. One its agent is updating waits on the agent instead. Input: the change sets.
 * Output: the count. */
export function waitingCount(sets: readonly ChangeSet[]): number {
  return sets.filter((set) => set.state === "pending" || set.state === "conflict").length;
}

/** The line that tells the owner change sets wait where they are reviewed: Changes, or the
 * IDE's Source Control once the IDE replaces Changes (DEC-097). Inputs: the count and the
 * place. Output: the line, or null when none wait. */
export function waitingLine(count: number, place: "changes" | "ide" = "changes"): string | null {
  if (count <= 0) return null;
  const where = place === "ide" ? "the IDE, under Source Control" : "Changes";
  return count === 1 ? `1 change set is waiting for you in ${where}` : `${count} change sets are waiting for you in ${where}`;
}
