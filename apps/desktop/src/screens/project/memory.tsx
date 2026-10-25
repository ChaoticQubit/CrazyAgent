import { deleteMemory, editProjectMemory, nextGeneration, savedMemories } from "../../api";
import type { Result, SavedMemory } from "../../api";
import { useEffect, useState } from "react";
import { failureCopy, loadProjectMemories, MEMORY_PAGES_MAX, promoteProjectMemory, reviewMemory } from "../../api-projects";
import type { MemoryDecision, MemoryItem } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { Button, ConfirmDialog, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { SuggestionEditor } from "../settings/memory";
import { memoryGroups, changeOutcome } from "./research";
import { useLoad } from "../../state";
import { NotReady, ProjectEmpty } from "./shared";
import type { ProjectData } from "./shared";

async function loadAll(id: string): Promise<Result<{ items: MemoryItem[]; more: boolean }>> {
  const items: MemoryItem[] = [];
  let after: string | null = null;
  for (let page = 0; page < MEMORY_PAGES_MAX; page += 1) {
    const batch = await loadProjectMemories(id, after);
    if (batch.kind !== "ok") return batch;
    if (batch.value.length === 0) return { kind: "ok", value: { items, more: false } };
    items.push(...batch.value);
    after = batch.value[batch.value.length - 1].id;
  }
  return { kind: "ok", value: { items, more: true } };
}

/** One research memory waiting for the owner, like the TUI's y Save to memory, n Dismiss and
 * e Edit first. The row shows the exact stored text with hidden characters as escapes, never a
 * formatted view, and Edit first swaps it for an editor that saves the owner's words.
 * Inputs: the project id, the memory, whether it is being edited or saved, and the actions. */
export function MemorySuggestion({ projectId, item, editing, busy, onSave, onDismiss, onEdit, onSaveEdited, onCancel }: {
  projectId: string;
  item: MemoryItem;
  editing: boolean;
  busy: boolean;
  onSave: () => void;
  onDismiss: () => void;
  onEdit: () => void;
  onSaveEdited: (text: string) => void;
  onCancel: () => void;
}) {
  if (editing) return <li className="pj-memory">
    <SuggestionEditor suggestion={{ project_id: projectId, project_name: "", id: item.id, version: item.version, text: item.text }}
      label="Edit before saving" pending={busy} onSave={onSaveEdited} onCancel={onCancel} />
  </li>;
  return <li className="pj-memory">
    <p className="pj-note-text">{displayText(item.text)}</p>
    <span className="pj-row"><StatusBadge word="Suggested" />
      <Button size="small" variant="primary" pending={busy} pendingLabel="Saving..." onClick={onSave}>Save to memory</Button>
      <Button size="small" icon="pencil" disabled={busy} onClick={onEdit}>Edit first</Button>
      <Button size="small" variant="quiet" disabled={busy} onClick={onDismiss}>Dismiss</Button></span>
  </li>;
}

/** One saved project memory with its exact stored text, hidden characters as escapes, next to
 * Use in all projects and Delete. Inputs: the memory, whether it is busy and the two actions.
 * Output: the row. */
export function SavedMemoryRow({ item, busy, onPromote, onDelete }: { item: MemoryItem; busy: boolean; onPromote: () => void; onDelete: () => void }) {
  return <li className="pj-memory">
    <p className="pj-note-text">{displayText(item.text)}</p>
    <span className="pj-row"><StatusBadge word="Saved" tone="ok" />
      <Button size="small" variant="quiet" disabled={busy} onClick={onPromote}>Use in all projects</Button>
      <Button size="small" variant="danger-text" disabled={busy} onClick={onDelete}>Delete</Button></span>
  </li>;
}

/** The project's memories in the Context tab: Suggested ones wait for Save to memory, Edit
 * first or Dismiss, Saved ones can be used in all projects with one click or deleted, and the owner's
 * own notes can be deleted. Input: the project data. Output: the panel. */
export function ProjectMemory({ data }: { data: ProjectData }) {
  const [memories, setMemories] = useState<Result<{ items: MemoryItem[]; more: boolean }>>();
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [deleting, setDeleting] = useState<MemoryItem | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [notes, reloadNotes] = useLoad(() => savedMemories(data.id, ""), [data.id]);
  const [forgetting, setForgetting] = useState<SavedMemory | null>(null);

  useEffect(() => {
    let live = true;
    void loadAll(data.id).then((result) => { if (live) setMemories(result); });
    return () => { live = false; };
  }, [data.id, round]);

  function settle(result: Result<IntentReceipt>, done: string, missing: string) {
    setBusy(null);
    setDeleting(null);
    const outcome = changeOutcome(result, done, missing);
    setNote(outcome.note);
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.reload) setRound((value) => value + 1);
  }

  async function review(item: MemoryItem, decision: MemoryDecision, done: string) {
    setBusy(item.id);
    setNote("");
    settle(await reviewMemory(data.id, item.id, item.version, decision, nextGeneration()), done, "reviewing memories");
  }

  async function saveEdited(item: MemoryItem, text: string) {
    setBusy(item.id);
    setNote("");
    const result = await editProjectMemory(data.id, item.id, item.version, text, nextGeneration());
    if (result.kind === "ok") setEditing(null);
    settle(result, "Saved to memory.", "editing memories");
  }

  async function promote(item: MemoryItem) {
    setBusy(item.id);
    setNote("");
    settle(await promoteProjectMemory(data.id, item.id, item.version, nextGeneration()), "Now used in all projects.", "using a memory in all projects");
  }

  async function forget(memory: SavedMemory) {
    setBusy(memory.id);
    setNote("");
    const result = await deleteMemory(memory.id);
    setBusy(null);
    setForgetting(null);
    if (result.kind === "ok") data.notify("Memory deleted.");
    else setNote(result.kind === "not_ready" ? "Not ready yet: deleting notes isn't in this build." : result.message);
    reloadNotes();
  }

  const value = memories?.kind === "ok" ? memories.value : null;
  const ownNotes = notes.kind === "ok" ? notes.value.items : [];
  const groups = memoryGroups(value?.items ?? []);
  return <div className="pj-stack">
    <div className="pj-reader-tb"><h2>Project memory</h2></div>
    <p className="pj-secondary">Facts agents keep for this project. Nothing is saved without your click.</p>
    {memories === undefined && <p className="pj-muted" role="status">Loading memories...</p>}
    {memories?.kind === "not_ready" && <NotReady what="the project memory list isn't in this build." />}
    {memories?.kind === "error" && <p className="pj-error" role="alert">Couldn't load memories. {failureCopy(memories.code)}</p>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {groups.suggested.length > 0 && <section className="pj-stack-s" aria-labelledby="pj-mem-suggested">
      <h3 id="pj-mem-suggested">Suggested</h3>
      <ul className="pj-rows">{groups.suggested.map((item) => <MemorySuggestion key={item.id} projectId={data.id} item={item}
        editing={editing === item.id} busy={busy === item.id}
        onSave={() => void review(item, { decision: "Keep" }, "Saved to memory.")}
        onDismiss={() => void review(item, { decision: "Reject" }, "Dismissed.")}
        onEdit={() => setEditing(item.id)} onCancel={() => setEditing(null)}
        onSaveEdited={(text) => void saveEdited(item, text)} />)}</ul>
    </section>}
    {groups.saved.length > 0 && <section className="pj-stack-s" aria-labelledby="pj-mem-saved">
      <h3 id="pj-mem-saved">Saved</h3>
      <ul className="pj-rows">{groups.saved.map((item) => <SavedMemoryRow key={item.id} item={item} busy={busy === item.id}
        onPromote={() => void promote(item)} onDelete={() => setDeleting(item)} />)}</ul>
    </section>}
    {ownNotes.length > 0 && <section className="pj-stack-s" aria-labelledby="pj-mem-notes">
      <h3 id="pj-mem-notes">Your notes</h3>
      <ul className="pj-rows">{ownNotes.map((memory) => <li key={memory.id} className="pj-memory">
        <p className="pj-note-text">{displayText(memory.text)}</p>
        <span className="pj-row"><StatusBadge word="Saved" tone="ok" />
          <Button size="small" variant="danger-text" disabled={busy === memory.id} onClick={() => setForgetting(memory)}>Delete</Button></span>
      </li>)}</ul>
      {notes.kind === "ok" && notes.value.next !== null && <p className="pj-small pj-muted">Showing your newest {ownNotes.length} notes. Settings, Memory lists them all.</p>}
    </section>}
    {value && value.more && <p className="pj-small pj-muted">Showing the first {value.items.length} memories.</p>}
    {value && groups.suggested.length === 0 && groups.saved.length === 0 && ownNotes.length === 0 && <ProjectEmpty title="No memories yet">
      Open a research report and use Save to memory, or select text in any file and use Save selection.
    </ProjectEmpty>}
    <ConfirmDialog open={deleting !== null} title="Delete this memory?" confirmLabel="Delete" pending={deleting !== null && busy === deleting.id}
      onCancel={() => setDeleting(null)} onConfirm={() => { if (deleting) void review(deleting, { decision: "Remove" }, "Memory deleted."); }}>
      Agents stop using it right away.
    </ConfirmDialog>
    <ConfirmDialog open={forgetting !== null} title="Delete this memory?" confirmLabel="Delete" pending={forgetting !== null && busy === forgetting.id}
      onCancel={() => setForgetting(null)} onConfirm={() => { if (forgetting) void forget(forgetting); }}>
      Agents stop using it right away.
    </ConfirmDialog>
  </div>;
}
