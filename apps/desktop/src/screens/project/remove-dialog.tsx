import type { RemovalPreview } from "../../api-projects";
import { Button } from "../../ui";
import { displayText } from "../conversation";

export type RemoveChoice = "dashboard" | "everything";

const LOSS_WORDS: Record<string, string> = {
  cleanup_worktree_dirty: "has uncommitted edits",
  cleanup_worktree_unreadable: "may have uncommitted edits",
  cleanup_commits_unlanded: "has commits that never landed",
  cleanup_refs_unlanded: "has commits that never landed",
  cleanup_changes_open: "has a change set not accepted",
  cleanup_changes_restorable: "has a change set not accepted",
};

/** The ways a project can be removed. Input: the engine's preview, or null while it loads.
 * Output: Remove from dashboard first, then Delete everything when every folder is inside the
 * projects home. */
export function removeChoices(preview: RemovalPreview | null): RemoveChoice[] {
  return preview?.can_delete ? ["dashboard", "everything"] : ["dashboard"];
}

/** Whether Delete everything may be sent. Inputs: the preview and the typed name. Output: true
 * only when the preview allows a delete and the typed name is the project's exact name. */
export function deleteReady(preview: RemovalPreview | null, typed: string): boolean {
  return preview !== null && preview.can_delete && typed === preview.name;
}

const LOSS_LINES_MAX = 4;
const EDIT_LOSSES = ["cleanup_worktree_dirty", "cleanup_worktree_unreadable"];

function agentCount(count: number, one: string, many: string): string {
  return `${count} agent${count === 1 ? "" : "s"} ${count === 1 ? one : many}`;
}

function agentsWith(preview: RemovalPreview, wanted: (code: string) => boolean): number {
  return new Set(preview.losses.filter((loss) => wanted(loss.code)).map((loss) => loss.id)).size;
}

/** What Delete everything would lose, in plain lines that always fit the dialog: one line each
 * up to four lines, past that counts of each kind, so no kind of loss drops out. Input: the
 * preview. Output: the running agents, the open change sets and the agents' unlanded work. */
export function lossLines(preview: RemovalPreview): string[] {
  const sets = preview.open_change_sets;
  const setLine = sets > 0 ? [`${sets} change set${sets === 1 ? "" : "s"} not accepted or discarded`] : [];
  const lines = [
    ...preview.running.map((label) => `${label} is running and will be stopped`),
    ...setLine,
    ...preview.losses.map((loss) => `${loss.label} ${LOSS_WORDS[loss.code] ?? "has work that never landed"}`),
  ];
  if (lines.length <= LOSS_LINES_MAX) return lines;
  const dirty = agentsWith(preview, (code) => code === "cleanup_worktree_dirty");
  const unsure = agentsWith(preview, (code) => code === "cleanup_worktree_unreadable");
  const edits = agentsWith(preview, (code) => EDIT_LOSSES.includes(code));
  const unlanded = agentsWith(preview, (code) => !EDIT_LOSSES.includes(code));
  return [
    ...(preview.running.length > 0 ? [agentCount(preview.running.length, "is running and will be stopped", "are running and will be stopped")] : []),
    ...setLine,
    ...(edits > 0 ? [unsure === 0 ? agentCount(edits, "has uncommitted edits", "have uncommitted edits")
      : dirty === 0 ? agentCount(edits, "may have uncommitted edits", "may have uncommitted edits")
        : agentCount(edits, "has or may have uncommitted edits", "have or may have uncommitted edits")] : []),
    ...(unlanded > 0 ? [agentCount(unlanded, "has work that never landed", "have work that never landed")] : []),
  ];
}

/** The remove dialog's body. Inputs: the preview, the picked choice, the typed name, whether a
 * removal is running and the change, confirm and cancel handlers. Output: the choices, why
 * Delete everything is not offered when the engine refuses it, the loss list and the name box,
 * then Keep project and the button that sends the picked choice. Only the folder list scrolls
 * when the window is short, by mouse or keyboard, and the loss list never runs past four lines,
 * so at the app's smallest window the choices, the losses and the name box stay in the frame. */
export function RemoveDialogView({ preview, choice, typed, pending, onChoice, onTyped, onConfirm, onCancel }: {
  preview: RemovalPreview | null;
  choice: RemoveChoice;
  typed: string;
  pending: boolean;
  onChoice: (choice: RemoveChoice) => void;
  onTyped: (typed: string) => void;
  onConfirm?: () => void;
  onCancel?: () => void;
}) {
  const offered = removeChoices(preview).includes("everything");
  const everything = choice === "everything" && offered;
  const losses = preview ? lossLines(preview) : [];
  const ready = deleteReady(preview, typed);
  return <div className="pj-stack pj-remove">
    <div className="pj-radio-cards" role="radiogroup" aria-label="How to remove the project">
      <label className={`pj-radio-card ${everything ? "" : "selected"}`}>
        <input type="radio" name="remove-choice" aria-label="Remove from dashboard (keep files)" value="dashboard" checked={!everything} disabled={pending} onChange={() => onChoice("dashboard")} />
        <span className="pj-radio-body">
          <strong>Remove from dashboard (keep files)</strong>
          <span className="pj-secondary">Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk, with every file in it.</span>
        </span>
      </label>
      {offered && <label className={`pj-radio-card ${everything ? "selected" : ""}`}>
        <input type="radio" name="remove-choice" aria-label="Delete everything" value="everything" checked={everything} disabled={pending} onChange={() => onChoice("everything")} />
        <span className="pj-radio-body">
          <strong>Delete everything</strong>
          <span className="pj-secondary">Stops its agents, moves its folders to the Trash and removes its records. Its token counts stay under Removed projects.</span>
        </span>
      </label>}
    </div>
    {preview && !offered && preview.blocked && <p className="pj-note" role="note">{displayText(preview.blocked)}</p>}
    {preview && everything && <>
      <div className="pj-field pj-remove-trash">
        <span className="pj-label">Moves to the Trash</span>
        <ul className="pj-rows pj-remove-details" aria-label="Folders moved to the Trash" tabIndex={0}>{preview.folders.map((folder) => <li className="pj-mono" key={folder}>{displayText(folder)}</li>)}</ul>
      </div>
      {losses.length > 0
        ? <ul className="pj-note warn pj-remove-losses" aria-label="What would be lost">{losses.map((line) => <li key={line}>{displayText(line)}</li>)}</ul>
        : <p className="pj-secondary">No agent is running and no work is unlanded.</p>}
      <label className="pj-field">
        <span className="pj-label">Type {displayText(preview.name)} to confirm</span>
        <input className="pj-input" aria-label="Type the project name" value={typed} disabled={pending} autoComplete="off" spellCheck={false} onChange={(event) => onTyped(event.currentTarget.value)} />
      </label>
    </>}
    <div className="dialog-foot">
      <Button disabled={pending} onClick={onCancel} data-autofocus>Keep project</Button>
      {everything
        ? <Button variant="danger" pending={pending} disabled={!ready} onClick={onConfirm}>Delete everything</Button>
        : <Button variant="danger" pending={pending} onClick={onConfirm}>Remove project</Button>}
    </div>
  </div>;
}
