import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useState } from "react";
import { failureCopy, loadProjectMemories, MEMORY_PAGES_MAX, receiptState, reviewMemory } from "../../api-projects";
import type { MemoryDecision, MemoryItem } from "../../api-projects";
import { Button, ConfirmDialog, EmptyState, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { memoryGroups } from "./research";
import { NotReady } from "./shared";
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

/** The project's memories in the Context tab: Suggested ones wait for Save to memory or
 * Dismiss, Saved ones can be deleted. Input: the project data. Output: the panel. */
export function ProjectMemory({ data }: { data: ProjectData }) {
  const [memories, setMemories] = useState<Result<{ items: MemoryItem[]; more: boolean }>>();
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [deleting, setDeleting] = useState<MemoryItem | null>(null);

  useEffect(() => {
    let live = true;
    void loadAll(data.id).then((result) => { if (live) setMemories(result); });
    return () => { live = false; };
  }, [data.id, round]);

  async function review(item: MemoryItem, decision: MemoryDecision, done: string) {
    setBusy(item.id);
    setNote("");
    const result = await reviewMemory(data.id, item.id, item.version, decision, nextGeneration());
    setBusy(null);
    setDeleting(null);
    if (result.kind === "not_ready") return setNote("Not ready yet: reviewing memories isn't in this build.");
    if (result.kind === "error") return setNote(failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind === "applied") data.notify(done);
    else setNote(state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check the list before trying again.");
    setRound((value) => value + 1);
  }

  const value = memories?.kind === "ok" ? memories.value : null;
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
      <ul className="pj-rows">{groups.suggested.map((item) => <li key={item.id} className="pj-memory">
        <p className="pj-pre">{displayText(item.text)}</p>
        <span className="pj-row"><StatusBadge word="Suggested" />
          <Button size="small" variant="primary" pending={busy === item.id} pendingLabel="Saving..." onClick={() => void review(item, { decision: "Keep" }, "Saved to memory.")}>Save to memory</Button>
          <Button size="small" variant="quiet" disabled={busy === item.id} onClick={() => void review(item, { decision: "Reject" }, "Dismissed.")}>Dismiss</Button></span>
      </li>)}</ul>
    </section>}
    {groups.saved.length > 0 && <section className="pj-stack-s" aria-labelledby="pj-mem-saved">
      <h3 id="pj-mem-saved">Saved</h3>
      <ul className="pj-rows">{groups.saved.map((item) => <li key={item.id} className="pj-memory">
        <p className="pj-pre">{displayText(item.text)}</p>
        <span className="pj-row"><StatusBadge word="Saved" tone="ok" />
          <Button size="small" variant="danger-text" disabled={busy === item.id} onClick={() => setDeleting(item)}>Delete</Button></span>
      </li>)}</ul>
    </section>}
    {value && value.more && <p className="pj-small pj-muted">Showing the first {value.items.length} memories.</p>}
    {value && groups.suggested.length === 0 && groups.saved.length === 0 && <EmptyState icon="brain" title="No memories yet" compact>
      Open a research report and use Save to memory. It shows up here for you to approve.
    </EmptyState>}
    <ConfirmDialog open={deleting !== null} title="Delete this memory?" confirmLabel="Delete" pending={deleting !== null && busy === deleting.id}
      onCancel={() => setDeleting(null)} onConfirm={() => { if (deleting) void review(deleting, { decision: "Remove" }, "Memory deleted."); }}>
      Agents stop using it right away.
    </ConfirmDialog>
  </div>;
}
