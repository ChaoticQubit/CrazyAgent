import { useState } from "react";
import { forgetMemory, keepMemory, listMemories, type Memory } from "../../api";
import { useAppSession, useLoad } from "../../state";
import { Button, ConfirmDialog, EmptyState, Icon, LoadError, NotReady, Notice, Panel, SearchField, Skeleton } from "../../ui";

/** Filters saved memories by search text, case-insensitively, over text and source.
 * Inputs: memories and the search text. Output: the matching saved memories. */
export function savedMemories(memories: Memory[], search: string): Memory[] {
  const needle = search.trim().toLowerCase();
  return memories.filter((memory) => memory.state === "saved" &&
    (!needle || memory.text.toLowerCase().includes(needle) || memory.source.toLowerCase().includes(needle)));
}

/** Settings, Memory: suggestions to save or dismiss (one click is the owner's approval) and
 * the saved list with search and delete. */
export function MemorySettings() {
  const session = useAppSession();
  const [memories, reload] = useLoad(listMemories);
  const [search, setSearch] = useState("");
  const [deleting, setDeleting] = useState<Memory>();
  const [failure, setFailure] = useState("");
  async function act(id: string, keep: boolean) {
    setFailure("");
    const result = session.noteResult(await (keep ? keepMemory(id) : forgetMemory(id)));
    setDeleting(undefined);
    if (result.kind === "ok") { session.showToast(keep ? "Saved to memory." : "Memory removed."); reload(); }
    else setFailure(result.kind === "not_ready" ? "This change isn't ready in this build yet." : result.message);
  }
  if (memories.kind === "loading") return <Skeleton shape="row" count={5} />;
  if (memories.kind === "not_ready") return <NotReady what="Memory" />;
  if (memories.kind === "error") return <LoadError message="Couldn't load memory." onRetry={reload} />;
  const suggested = memories.value.filter((memory) => memory.state === "suggested");
  const saved = savedMemories(memories.value, search);
  return <div className="stack">
    <p className="secondary">Facts agents keep across projects. Nothing is saved here without your click.</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {suggested.length > 0 && <Panel title="Suggested" padded={false}><div className="rows">
      {suggested.map((memory) => <div className="item-row" key={memory.id}>
        <div className="stack-small"><span>{memory.text}</span><span className="muted small">{memory.source}</span></div>
        <span className="row"><Button variant="primary" size="small" icon="brain" onClick={() => void act(memory.id, true)}>Save to memory</Button>
          <Button size="small" onClick={() => void act(memory.id, false)}>Dismiss</Button></span>
      </div>)}
    </div></Panel>}
    {memories.value.every((memory) => memory.state !== "saved") ? <EmptyState icon="brain" title="No memories yet">
      When agents find something worth keeping, it shows up here for you to save.
    </EmptyState> : <Panel title="Saved" actions={<SearchField label="Search memory" value={search} onValue={setSearch} />} padded={false}>
      <div className="rows">{saved.map((memory) => <div className="item-row" key={memory.id}>
        <div className="stack-small"><span>{memory.text}</span><span className="muted small">{memory.source}{memory.saved_at ? `, ${memory.saved_at}` : ""}</span></div>
        <Button variant="danger-text" size="small" icon="trash" onClick={() => setDeleting(memory)}>Delete</Button>
      </div>)}
      {saved.length === 0 && <p className="item-row muted"><Icon name="search" size={14} /> No memories match.</p>}</div>
    </Panel>}
    <ConfirmDialog open={Boolean(deleting)} title="Delete this memory?" confirmLabel="Delete" onCancel={() => setDeleting(undefined)}
      onConfirm={() => { if (deleting) void act(deleting.id, false); }}>Agents stop using it right away.</ConfirmDialog>
  </div>;
}
