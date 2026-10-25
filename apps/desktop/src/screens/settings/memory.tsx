import { useEffect, useState } from "react";
import {
  addMemory, deleteMemory, dismissSuggestion, memoryEdit, memorySuggestions, saveEditedSuggestion, saveSuggestion, savedMemories,
  MEMORY_TEXT_BYTES_MAX, type Result, type SavedMemory, type SavedMemoryPage, type Suggestion,
} from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, EmptyState, Icon, LoadError, NotReady, Notice, Panel, SearchField, Select, Skeleton, TextArea } from "../../ui";
import { displayText } from "../conversation";
import "../project/projects.css";

const NOTE_MAX = 2000;
const NOT_READY = "This change isn't ready in this build yet.";

/** Says where a saved memory applies and where it came from. Inputs: the memory and project
 * names by id. Output: "App-wide", "App-wide, from Harbor Notes" or "Harbor Notes only". */
export function memorySource(memory: SavedMemory, names: Map<string, string>): string {
  if (memory.scope === "project") return `${names.get(memory.project_id ?? "") ?? "A removed project"} only`;
  if (memory.origin !== "promoted") return "App-wide";
  return `App-wide, from ${names.get(memory.from_project_id ?? "") ?? "a removed project"}`;
}

/** The Show filter: app-wide memory, then each project. Input: the projects.
 * Output: select options; "" means app-wide. */
export function scopeOptions(projects: { id: string; name: string }[]): { value: string; label: string }[] {
  return [{ value: "", label: "App-wide" }, ...projects.map((project) => ({ value: project.id, label: project.name }))];
}

function AddMemoryForm({ where, onAdd }: { where: string; onAdd: (text: string) => Promise<boolean> }) {
  const [text, setText] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const save = async () => {
    const trimmed = text.trim();
    const invalid = !trimmed ? "Write something to remember." : [...trimmed].length > NOTE_MAX ? "Keep the memory under 2000 characters." : undefined;
    setError(invalid);
    if (invalid) return;
    setPending(true);
    if (await onAdd(trimmed)) setText("");
    setPending(false);
  };
  return <Panel title="Add memory">
    <form className="stack-small" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <TextArea id="memory-note" label={`Something agents should remember (${where})`} value={text} onValue={setText} rows={2}
        maxLength={NOTE_MAX} counterAt={NOTE_MAX - 200} error={error} />
      <span><Button type="submit" variant="primary" icon="brain" pending={pending} pendingLabel="Saving...">Add memory</Button></span>
    </form>
  </Panel>;
}

/** One research suggestion rewritten in the owner's words before it is saved, like the TUI's
 * edit key. Inputs: the suggestion, an optional field label, whether a save is running, and
 * the save and cancel actions. Output: the inline editor, starting from the suggested text with
 * hidden characters shown as escapes. */
export function SuggestionEditor({ suggestion, label, pending, onSave, onCancel }: {
  suggestion: Suggestion;
  label?: string;
  pending: boolean;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(() => displayText(suggestion.text));
  const [error, setError] = useState<string>();
  const id = `memory-edit-${suggestion.project_id}-${suggestion.id}`;
  const save = () => {
    const edit = memoryEdit(text);
    setError(edit.kind === "invalid" ? edit.reason : undefined);
    if (edit.kind === "ok") onSave(edit.text);
  };
  return <form className="stack-small grow" onSubmit={(event) => { event.preventDefault(); save(); }}
    onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); onCancel(); } }}>
    <TextArea id={id} label={label ?? `Edit before saving (from ${displayText(suggestion.project_name)})`} value={text} onValue={setText} rows={3}
      maxLength={MEMORY_TEXT_BYTES_MAX} counterAt={MEMORY_TEXT_BYTES_MAX - 400} error={error} />
    <span className="row">
      <Button type="submit" variant="primary" size="small" icon="brain" pending={pending} pendingLabel="Saving...">Save edited</Button>
      <Button size="small" onClick={onCancel}>Cancel</Button>
    </span>
  </form>;
}

/** Settings, Memory as a pure view: research suggestions to save, edit then save, or dismiss
 * (one click is the owner's approval), saved memory for the chosen scope with search, older
 * pages and delete, and a form to add a memory. Inputs: the reads, filters, last failure and
 * the actions. */
export function MemoryView({ saved, suggested, scope, projects, search, failure, busy, onSearch, onScope, onAdd, onSave, onSaveEdited, onDismiss, onDelete, onMore, onRetry }: {
  saved: Loaded<SavedMemoryPage>;
  suggested: Loaded<Suggestion[]>;
  scope: string;
  projects: { id: string; name: string }[];
  search: string;
  failure: string;
  busy: string | null;
  onSearch: (search: string) => void;
  onScope: (scope: string) => void;
  onAdd: (text: string) => Promise<boolean>;
  onSave: (suggestion: Suggestion) => void;
  onSaveEdited: (suggestion: Suggestion, text: string) => void;
  onDismiss: (suggestion: Suggestion) => void;
  onDelete: (memory: SavedMemory) => void;
  onMore: () => void;
  onRetry: () => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const names = new Map(projects.map((project) => [project.id, project.name]));
  const where = scope ? `${names.get(scope) ?? "this project"} only` : "app-wide";
  const filters = <span className="row">
    {projects.length > 0 && <Select id="memory-scope" label="Show" hideLabel value={scope} options={scopeOptions(projects)} onValue={onScope} />}
    <SearchField label="Search memory" value={search} onValue={onSearch} />
  </span>;
  let list;
  if (saved.kind === "loading") list = <Skeleton shape="row" count={4} />;
  else if (saved.kind === "not_ready") list = <NotReady what="Memory" compact />;
  else if (saved.kind === "error") list = <LoadError message={saved.message} onRetry={onRetry} />;
  else if (saved.value.items.length === 0) {
    list = search.trim() ? <p className="item-row muted"><Icon name="search" size={14} /> No memories match.</p>
      : <EmptyState icon="brain" title="No memories yet" compact>When agents find something worth keeping, it shows up here for you to save.</EmptyState>;
  } else {
    list = <div className="rows">
      {saved.value.items.map((memory) => <div className="item-row" key={memory.id}>
        <div className="stack-small"><p className="pj-note-text">{displayText(memory.text)}</p><span className="muted small">{displayText(memorySource(memory, names))}</span></div>
        <Button variant="danger-text" size="small" icon="trash" onClick={() => onDelete(memory)}>Delete</Button>
      </div>)}
      {saved.value.next !== null && <div className="item-row"><span /><Button size="small" onClick={onMore}>Show older</Button></div>}
    </div>;
  }
  return <div className="stack">
    <p className="secondary">Facts agents keep across projects. Nothing is saved here without your click.</p>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {suggested.kind === "ok" && suggested.value.length > 0 && <Panel title="Suggested" padded={false}><div className="rows">
      {suggested.value.map((suggestion) => {
        const key = `${suggestion.project_id}/${suggestion.id}`;
        if (editing === key) return <div className="item-row" key={key}>
          <SuggestionEditor suggestion={suggestion} pending={busy === suggestion.id} onCancel={() => setEditing(null)}
            onSave={(text) => onSaveEdited(suggestion, text)} />
        </div>;
        return <div className="item-row" key={key}>
          <div className="stack-small"><p className="pj-note-text">{displayText(suggestion.text)}</p><span className="muted small">From {displayText(suggestion.project_name)}</span></div>
          <span className="row">
            <Button variant="primary" size="small" icon="brain" pending={busy === suggestion.id} pendingLabel="Saving..." onClick={() => onSave(suggestion)}>Save to memory</Button>
            <Button size="small" icon="pencil" disabled={busy === suggestion.id} onClick={() => setEditing(key)}>Edit</Button>
            <Button size="small" onClick={() => onDismiss(suggestion)}>Dismiss</Button>
          </span>
        </div>;
      })}
    </div></Panel>}
    <Panel title="Saved" actions={filters} padded={false}>{list}</Panel>
    <AddMemoryForm where={where} onAdd={onAdd} />
  </div>;
}

/** Settings, Memory: reads saved memory and research suggestions from the engine; saving a
 * suggestion keeps it in its project and makes it app-wide in one click. */
export function MemorySettings() {
  const session = useAppSession();
  const projects = session.projects.kind === "ok" ? session.projects.value.map((project) => ({ id: project.id, name: project.name })) : [];
  const projectKey = projects.map((project) => project.id).join(",");
  const [scope, setScope] = useState("");
  const [search, setSearch] = useState("");
  const [first, reloadSaved] = useLoad(() => savedMemories(scope || null, search), [scope, search]);
  const [suggested, reloadSuggested] = useLoad(() => memorySuggestions(projects), [projectKey]);
  const [older, setOlder] = useState<SavedMemoryPage>();
  const [deleting, setDeleting] = useState<SavedMemory>();
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => setOlder(undefined), [first]);

  const saved: Loaded<SavedMemoryPage> = first.kind === "ok" && older
    ? { kind: "ok", value: { items: [...first.value.items, ...older.items], next: older.next } } : first;
  const plain = (result: Result<unknown>) => result.kind === "not_ready" ? NOT_READY : result.kind === "error" ? result.message : "";

  async function act(id: string, action: () => Promise<Result<void>>, done: string, reload: () => void) {
    setFailure("");
    setBusy(id);
    const result = session.noteResult(await action());
    setBusy(null);
    if (result.kind === "ok") session.showToast(done);
    else setFailure(plain(result));
    reload();
  }

  const more = async () => {
    if (saved.kind !== "ok" || saved.value.next === null) return;
    const page = session.noteResult(await savedMemories(scope || null, search, saved.value.next));
    if (page.kind === "ok") setOlder({ items: [...(older?.items ?? []), ...page.value.items], next: page.value.next });
    else setFailure(plain(page));
  };

  return <>
    <MemoryView saved={saved} suggested={suggested} scope={scope} projects={projects} search={search} failure={failure} busy={busy}
      onSearch={setSearch} onScope={setScope} onRetry={reloadSaved} onMore={() => void more()} onDelete={setDeleting}
      onAdd={async (text) => {
        setFailure("");
        const result = session.noteResult(await addMemory(scope || null, text));
        if (result.kind !== "ok") { setFailure(plain(result)); return false; }
        session.showToast("Saved to memory.");
        reloadSaved();
        return true;
      }}
      onSave={(suggestion) => void act(suggestion.id, () => saveSuggestion(suggestion), "Saved to memory.", () => { reloadSuggested(); reloadSaved(); })}
      onSaveEdited={(suggestion, text) => void act(suggestion.id, () => saveEditedSuggestion(suggestion, text), "Saved to memory.", () => { reloadSuggested(); reloadSaved(); })}
      onDismiss={(suggestion) => void act(suggestion.id, () => dismissSuggestion(suggestion), "Suggestion dismissed.", reloadSuggested)} />
    <ConfirmDialog open={Boolean(deleting)} title="Delete this memory?" confirmLabel="Delete" onCancel={() => setDeleting(undefined)}
      onConfirm={() => { const memory = deleting; setDeleting(undefined); if (memory) void act(memory.id, () => deleteMemory(memory.id), "Memory deleted.", reloadSaved); }}>
      Agents stop using it right away.
    </ConfirmDialog>
  </>;
}
