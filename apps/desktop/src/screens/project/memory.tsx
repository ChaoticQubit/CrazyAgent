import { approveMemory, deleteMemory, editProjectMemory, loadAllProjectMemories, nextGeneration, savedMemories } from "../../api";
import type { Result, SavedMemory } from "../../api";
import { useEffect, useRef, useState } from "react";
import { failureCopy, promoteProjectMemory, reviewMemory } from "../../api-projects";
import type { MemoryDecision, MemoryItem } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { Button, ConfirmDialog, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { SuggestionEditor } from "../settings/memory";
import { memoryGroups, changeOutcome } from "./research";
import { useLoad } from "../../state";
import { NotReady, ProjectEmpty } from "./shared";
import type { ProjectData } from "./shared";

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

/** One note in Your notes with its exact stored text, hidden characters as escapes, next to
 * Delete. A note an agent wrote names that agent, so the owner never takes it for their own,
 * and one it asked for reads Needs approval with Approve right in the row, since no agent
 * reads it before the owner approves it (OP-22). Inputs: the memory, whether it is busy, and
 * the approve and delete actions. Output: the row. */
export function OwnNoteRow({ memory, busy, onApprove, onDelete }: { memory: SavedMemory; busy: boolean; onApprove: () => void; onDelete: () => void }) {
  const byAgent = memory.origin === "agent" || memory.origin === "requested";
  const waiting = memory.origin === "requested";
  return <li className="pj-memory">
    <p className="pj-note-text">{displayText(memory.text)}</p>
    <span className="pj-row">{byAgent && <span className="pj-small pj-muted">From {displayText(memory.agent ?? "an agent")}</span>}
      {waiting ? <StatusBadge word="Needs approval" /> : <StatusBadge word="Saved" tone="ok" />}
      {waiting && <Button size="small" variant="primary" icon="brain" pending={busy} pendingLabel="Approving..." onClick={onApprove}>Approve</Button>}
      <Button size="small" variant="danger-text" disabled={busy} onClick={onDelete}>Delete</Button></span>
  </li>;
}

/** The line at the top of Project memory while notes wait for the owner. It counts the notes
 * loaded so far, and says older notes may hold more while Show older remains.
 * Inputs: the loaded notes and whether older notes remain. Output: the line, or "" when none wait. */
export function waitingLine(notes: SavedMemory[], more: boolean): string {
  const count = notes.filter((memory) => memory.origin === "requested").length;
  if (count === 0) return "";
  const lead = count === 1 ? "1 memory waits for your approval." : `${count} memories wait for your approval.`;
  return more ? `${lead} Older notes may hold more.` : lead;
}

/** Joins the newest page of notes with the older pages shown so far, keeping each note once
 * when a reload moved it onto the newest page. Inputs: the newest page and the older notes.
 * Output: the notes, newest first. */
export function mergeNotes(first: SavedMemory[], older: SavedMemory[]): SavedMemory[] {
  const seen = new Set(first.map((memory) => memory.id));
  return [...first, ...older.filter((memory) => !seen.has(memory.id))];
}

/** Your notes in Project memory: each note with Approve while it waits and Delete, then Show
 * older while the engine has older notes for this project. Inputs: the notes, whether older
 * ones remain, the busy note, whether an older page is loading, and the actions.
 * Output: the section. */
export function OwnNotes({ items, more, busy, paging, onApprove, onDelete, onMore }: {
  items: SavedMemory[];
  more: boolean;
  busy: string | null;
  paging: boolean;
  onApprove: (memory: SavedMemory) => void;
  onDelete: (memory: SavedMemory) => void;
  onMore: () => void;
}) {
  return <section className="pj-stack-s" aria-labelledby="pj-mem-notes">
    <h3 id="pj-mem-notes">Your notes</h3>
    <ul className="pj-rows">{items.map((memory) => <OwnNoteRow key={memory.id} memory={memory} busy={busy === memory.id}
      onApprove={() => onApprove(memory)} onDelete={() => onDelete(memory)} />)}</ul>
    {more && <span><Button size="small" pending={paging} pendingLabel="Loading..." onClick={onMore}>Show older</Button></span>}
  </section>;
}

/** The top of Project memory: the title, a quiet Memory settings link to the app-wide Agent
 * memory writes setting, and the waiting line while notes wait for the owner.
 * Inputs: the waiting line and the navigate action. Output: the head. */
export function ProjectMemoryHead({ waiting, navigate }: { waiting: string; navigate: (path: string) => void }) {
  return <>
    <div className="pj-reader-tb"><h2>Project memory</h2>
      <Button size="small" variant="quiet" onClick={() => navigate("/settings/memory")}>Memory settings</Button></div>
    {waiting && <p className="pj-note" role="status">{waiting}</p>}
  </>;
}

/** The project's memories in the Context tab: Suggested ones wait for Save to memory, Edit
 * first or Dismiss, Saved ones can be used in all projects with one click or deleted, and Your
 * notes can be approved while they wait, deleted, and paged with Show older, all in the project.
 * Memory settings opens the app-wide Agent memory writes setting. Input: the project data.
 * Output: the panel. */
export function ProjectMemory({ data }: { data: ProjectData }) {
  const [memories, setMemories] = useState<Result<{ items: MemoryItem[]; more: boolean }>>();
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [deleting, setDeleting] = useState<MemoryItem | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [notes, reloadNotes] = useLoad(() => savedMemories(data.id, ""), [data.id], "project-memories");
  const [forgetting, setForgetting] = useState<SavedMemory | null>(null);
  const [older, setOlder] = useState<{ items: SavedMemory[]; next: number | null }>();
  const [paging, setPaging] = useState(false);
  const project = useRef(data.id);
  useEffect(() => {
    project.current = data.id;
    setOlder(undefined);
  }, [data.id]);

  useEffect(() => {
    let live = true;
    void loadAllProjectMemories(data.id).then((result) => { if (live) setMemories(result); });
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
    if (result.kind === "ok") {
      data.notify("Memory deleted.");
      setOlder((page) => page && { ...page, items: page.items.filter((item) => item.id !== memory.id) });
    } else setNote(result.kind === "not_ready" ? "Not ready yet: deleting notes isn't in this build." : result.message);
    reloadNotes();
  }

  async function approve(memory: SavedMemory) {
    setBusy(memory.id);
    setNote("");
    const result = await approveMemory(memory.id);
    setBusy(null);
    if (result.kind === "ok") {
      data.notify("Memory approved. Agents get it from their next launch.");
      setOlder((page) => page && { ...page, items: page.items.map((item) => item.id === memory.id ? { ...item, origin: "agent" as const } : item) });
    } else setNote(result.kind === "not_ready" ? "Not ready yet: approving notes isn't in this build." : result.message);
    reloadNotes();
  }

  async function showOlder(next: number) {
    const asked = data.id;
    setPaging(true);
    setNote("");
    const page = await savedMemories(asked, "", next);
    setPaging(false);
    if (project.current !== asked) return;
    if (page.kind === "ok") setOlder((shown) => ({ items: [...(shown?.items ?? []), ...page.value.items], next: page.value.next }));
    else setNote(page.kind === "not_ready" ? "Not ready yet: older notes aren't in this build." : page.message);
  }

  const value = memories?.kind === "ok" ? memories.value : null;
  const ownNotes = notes.kind === "ok" ? mergeNotes(notes.value.items, older?.items ?? []) : [];
  const next = notes.kind === "ok" ? (older ? older.next : notes.value.next) : null;
  const waiting = waitingLine(ownNotes, next !== null);
  const groups = memoryGroups(value?.items ?? []);
  return <div className="pj-stack">
    <ProjectMemoryHead waiting={waiting} navigate={data.navigate} />
    <p className="pj-secondary">Facts agents keep for this project. Nothing is saved without your click.</p>
    {memories === undefined && <p className="pj-muted" role="status">Loading memories...</p>}
    {memories?.kind === "not_ready" && <NotReady what="the project memory list isn't in this build." />}
    {memories?.kind === "error" && <p className="pj-error" role="alert">Couldn't load memories. {failureCopy(memories)}</p>}
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
    {ownNotes.length > 0 && <OwnNotes items={ownNotes} more={next !== null} busy={busy} paging={paging}
      onApprove={(memory) => void approve(memory)} onDelete={setForgetting}
      onMore={() => { if (next !== null) void showOlder(next); }} />}
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
