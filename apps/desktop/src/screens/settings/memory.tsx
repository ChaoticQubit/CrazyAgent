import { useEffect, useState } from "react";
import {
  addMemory, approveMemory, deleteMemory, dismissSuggestion, importMemory, memoryEdit, memorySuggestions, pickFolder, moveMemory, saveEditedSuggestion, saveSuggestion, savedMemories, setAgentMemoryScope, setAgentMemoryWrites,
  MEMORY_TEXT_BYTES_MAX, type AgentMemoryScope, type AgentMemoryWrites, type Result, type SavedMemory, type SavedMemoryPage, type Suggestion,
} from "../../api";
import { useAppSession, useHeld, useLoad, type Loaded } from "../../state";
import { Button, ConfirmDialog, EmptyState, Icon, LoadError, NotReady, Notice, SearchField, Select, SettingSection, Skeleton, TextArea } from "../../ui";
import { SettingRow } from "../../ui/setting";
import { displayText } from "../conversation";
import "../project/projects.css";

const NOTE_MAX = 2000;
const NOT_READY = "This change isn't ready in this build yet.";

/** Says where a saved memory applies and where it came from. Inputs: the memory and project
 * names by id. Output: "App-wide", "App-wide, from Harbor Notes", "Harbor Notes only", with
 * "saved by" and the agent for an agent's memory, and the approval it waits for. */
export function memorySource(memory: SavedMemory, names: Map<string, string>): string {
  const only = `${names.get(memory.project_id ?? "") ?? "A removed project"} only`;
  const place = memory.scope === "project" ? memory.origin === "imported" && memory.source_path ? `${only}, imported from ${memory.source_path}` : only
    : memory.origin === "promoted" ? `App-wide, from ${names.get(memory.from_project_id ?? "") ?? "a removed project"}` : "App-wide";
  if (memory.origin === "requested") return `Waiting for your approval. ${place}, asked by ${memory.agent ?? "an agent"}`;
  if (memory.origin === "agent") return `${place}, saved by ${memory.agent ?? "an agent"}`;
  return place;
}

/** The Agent memory writes choices, the default first. */
export const AGENT_WRITES_OPTIONS: { value: AgentMemoryWrites; label: string }[] = [
  { value: "save", label: "Save directly" },
  { value: "ask", label: "Ask me first" },
];

/** The Where agents save memories choices, the default first. */
export const AGENT_SCOPE_OPTIONS: { value: AgentMemoryScope; label: string }[] = [
  { value: "project", label: "This project only" },
  { value: "agent", label: "Agent chooses" },
];

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
  return <SettingSection title="Add memory">
    <form className="set-body" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <TextArea id="memory-note" label={`Something agents should remember (${where})`} value={text} onValue={setText} rows={2}
        maxLength={NOTE_MAX} counterAt={NOTE_MAX - 200} error={error} />
      <span><Button type="submit" variant="primary" icon="brain" pending={pending} pendingLabel="Saving...">Add memory</Button></span>
    </form>
  </SettingSection>;
}

function ImportMemoryForm({ projects, initial, onImport }: {
  projects: { id: string; name: string }[];
  initial: string;
  onImport: (project: string) => Promise<string>;
}) {
  const [project, setProject] = useState(initial);
  const [pending, setPending] = useState(false);
  const [done, setDone] = useState("");
  const chosen = projects.some((item) => item.id === project) ? project : projects[0]?.id ?? "";
  const run = async () => {
    if (!chosen || pending) return;
    setPending(true);
    setDone(await onImport(chosen));
    setPending(false);
  };
  return <SettingSection title="Import memory">
    <div className="set-body">
      <p className="secondary">Choose a folder of markdown memory files, such as a Claude Code project memory folder. Each file becomes one memory of the chosen project. MEMORY.md, links and other files are left out.</p>
      {projects.length === 0 ? <p className="muted">Add a project first. Imported memories belong to one project.</p> : <>
        <Select id="memory-import-project" label="Into project" value={chosen} options={projects.map((item) => ({ value: item.id, label: item.name }))} onValue={setProject} />
        <span><Button icon="folder" pending={pending} pendingLabel="Importing..." onClick={() => void run()}>Import memory</Button></span>
      </>}
      {done && <p className="muted small" role="status">{done}</p>}
    </div>
  </SettingSection>;
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

/** The Suggested section: research suggestions to save, edit then save, or dismiss, with the
 * editor opening in place. Inputs: the suggestions read, the action running and the handlers.
 * Output: the section, or nothing while there is no suggestion. */
function SuggestedSection({ suggested, busy, onSave, onSaveEdited, onDismiss }: {
  suggested: Loaded<Suggestion[]>;
  busy: string | null;
  onSave: (suggestion: Suggestion) => void;
  onSaveEdited: (suggestion: Suggestion, text: string) => void;
  onDismiss: (suggestion: Suggestion) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  if (suggested.kind !== "ok" || suggested.value.length === 0) return null;
  return <SettingSection title="Suggested">
    {suggested.value.map((suggestion) => {
      const key = `${suggestion.project_id}/${suggestion.id}`;
      if (editing === key) return <div className="set-row wide" key={key}>
        <SuggestionEditor suggestion={suggestion} pending={busy === suggestion.id} onCancel={() => setEditing(null)}
          onSave={(text) => onSaveEdited(suggestion, text)} />
      </div>;
      return <div className="set-row" key={key}>
        <p className="set-label pj-note-text">{displayText(suggestion.text)}</p>
        <span className="set-help">From {displayText(suggestion.project_name)}</span>
        <div className="set-ctl set-line-ctl">
          <Button variant="primary" size="small" icon="brain" pending={busy === suggestion.id} pendingLabel="Saving..." onClick={() => onSave(suggestion)}>Save to memory</Button>
          <Button size="small" icon="pencil" disabled={busy === suggestion.id} onClick={() => setEditing(key)}>Edit</Button>
          <Button size="small" disabled={busy === suggestion.id} onClick={() => onDismiss(suggestion)}>Dismiss</Button>
        </div>
      </div>;
    })}
  </SettingSection>;
}

/** What Saved memories lists: skeletons, a not-ready or failed read, the empty lines, or one row
 * per memory with who saved it, Approve while it waits, Delete and Show older. Inputs: the read,
 * the search, project names by id, the action running and the handlers. */
/** Tells whether the owner may move a memory into one project: an app-wide memory an agent
 * saved or asked to save. Input: the memory. Output: true when it can move. */
export function movable(memory: SavedMemory): boolean {
  return memory.scope === "global" && (memory.origin === "agent" || memory.origin === "requested");
}

function SavedList({ saved, search, names, projects, busy, onDelete, onMore, onRetry, onApprove, onMove }: {
  saved: Loaded<SavedMemoryPage>;
  search: string;
  names: Map<string, string>;
  projects: { id: string; name: string }[];
  busy: string | null;
  onDelete: (memory: SavedMemory) => void;
  onMore: () => void;
  onRetry: () => void;
  onApprove?: (memory: SavedMemory) => void;
  onMove?: (memory: SavedMemory, project: string) => void;
}) {
  if (saved.kind === "loading") return <div className="set-body"><Skeleton shape="row" count={4} /></div>;
  if (saved.kind === "not_ready") return <NotReady what="Memory" compact />;
  if (saved.kind === "error") return <LoadError message={saved.message} onRetry={onRetry} />;
  if (saved.value.items.length === 0) {
    return search.trim() ? <p className="item-row muted"><Icon name="search" size={14} /> No memories match.</p>
      : <EmptyState icon="brain" title="No memories yet" compact>When agents find something worth keeping, it shows up here for you to save.</EmptyState>;
  }
  return <>
    {saved.value.items.map((memory) => <div className="set-row" key={memory.id}>
      <p className="set-label pj-note-text">{displayText(memory.text)}</p>
      <span className="set-help">{displayText(memorySource(memory, names))}</span>
      <div className="set-ctl set-line-ctl">
        {memory.origin === "requested" && onApprove && <Button variant="primary" size="small" icon="brain" pending={busy === memory.id} pendingLabel="Approving..." onClick={() => onApprove(memory)}>Approve</Button>}
        {movable(memory) && onMove && projects.length > 0 && <Select id={`memory-move-${memory.id}`} label="Move to a project" hideLabel value=""
          options={[{ value: "", label: "Move to a project" }, ...projects.map((project) => ({ value: project.id, label: project.name }))]}
          onValue={(project) => { if (project) onMove(memory, project); }} />}
        <Button variant="danger-text" size="small" icon="trash" onClick={() => onDelete(memory)}>Delete</Button>
      </div>
    </div>)}
    {saved.value.next !== null && <div className="item-row"><span /><Button size="small" onClick={onMore}>Show older</Button></div>}
  </>;
}

/** Settings, Memory as a pure view: the title in every state, the Agent memory writes setting,
 * research suggestions to save (one click is the owner's approval), saved memory for the chosen
 * scope with search and older pages, and forms to add and import memory. Inputs: the reads, the
 * first page's read the writes setting came from (older pages merged in make a new object, which
 * no read made), filters, last failure and the actions. */
export function MemoryView({ saved, writesRead, suggested, scope, projects, search, failure, busy, onSearch, onScope, onAdd, onImport, onSave, onSaveEdited, onDismiss, onDelete, onMore, onRetry, onApprove, onMove, onWrites, onReach }: {
  saved: Loaded<SavedMemoryPage>;
  writesRead: Loaded<SavedMemoryPage>;
  suggested: Loaded<Suggestion[]>;
  scope: string;
  projects: { id: string; name: string }[];
  search: string;
  failure: string;
  busy: string | null;
  onSearch: (search: string) => void;
  onScope: (scope: string) => void;
  onAdd: (text: string) => Promise<boolean>;
  onImport: (project: string) => Promise<string>;
  onSave: (suggestion: Suggestion) => void;
  onSaveEdited: (suggestion: Suggestion, text: string) => void;
  onDismiss: (suggestion: Suggestion) => void;
  onDelete: (memory: SavedMemory) => void;
  onMore: () => void;
  onRetry: () => void;
  onApprove?: (memory: SavedMemory) => void;
  onWrites?: (writes: AgentMemoryWrites) => Promise<string>;
  onReach?: (reach: AgentMemoryScope) => Promise<string>;
  onMove?: (memory: SavedMemory, project: string) => void;
}) {
  const [writes, holdWrites] = useHeld<AgentMemoryWrites | null>(saved.kind === "ok" ? saved.value.agent_writes : null, Object.is, writesRead);
  const [reach, holdReach] = useHeld<AgentMemoryScope | null>(saved.kind === "ok" ? saved.value.agent_scope : null, Object.is, writesRead);
  const names = new Map(projects.map((project) => [project.id, project.name]));
  const where = scope ? `${names.get(scope) ?? "this project"} only` : "app-wide";
  const filters = <span className="row">
    {projects.length > 0 && <Select id="memory-scope" label="Show" hideLabel value={scope} options={scopeOptions(projects)} onValue={onScope} />}
    <SearchField label="Search memory" value={search} onValue={onSearch} />
  </span>;
  return <div className="stack">
    <div className="settings-top"><div><h2>Memory</h2><p>Facts agents keep across projects, with who saved each one. Agents read them at launch.</p></div></div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {saved.kind === "ok" && onWrites && <SettingSection title="Agent memory writes">
      <SettingRow label="When an agent saves a memory">
        <Select id="memory-agent-writes" label="When an agent saves a memory" value={writes ?? saved.value.agent_writes} options={AGENT_WRITES_OPTIONS}
          helper={(writes ?? saved.value.agent_writes) === "ask"
            ? "Agents' memories wait here until you approve them."
            : "Agents' memories are saved at once. A sub-agent that reads hostile content could save a memory that steers every agent; delete any you don't trust."}
          onValue={(value) => {
            const chosen = value === "ask" ? "ask" : "save";
            const settled = holdWrites(chosen);
            void onWrites(chosen).then(settled);
          }} />
      </SettingRow>
      {onReach && <SettingRow label="Where agents save memories">
        <Select id="memory-agent-scope" label="Where agents save memories" value={reach ?? saved.value.agent_scope} options={AGENT_SCOPE_OPTIONS}
          helper={(reach ?? saved.value.agent_scope) === "agent"
            ? "Agents may save a memory app-wide, where every project's agents read it."
            : "An agent's memory stays in its own project, even when it asks to save it app-wide."}
          onValue={(value) => {
            const chosen = value === "agent" ? "agent" : "project";
            const settled = holdReach(chosen);
            void onReach(chosen).then(settled);
          }} />
      </SettingRow>}
    </SettingSection>}
    <SuggestedSection suggested={suggested} busy={busy} onSave={onSave} onSaveEdited={onSaveEdited} onDismiss={onDismiss} />
    <SettingSection title="Saved memories" actions={filters}>
      <SavedList saved={saved} search={search} names={names} projects={projects} busy={busy} onDelete={onDelete} onMore={onMore} onRetry={onRetry} onApprove={onApprove} onMove={onMove} />
    </SettingSection>
    <AddMemoryForm where={where} onAdd={onAdd} />
    <ImportMemoryForm projects={projects} initial={scope} onImport={onImport} />
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
  const [suggested, reloadSuggested] = useLoad(() => memorySuggestions(projects), [projectKey], "memory-suggestions");
  const [older, setOlder] = useState<SavedMemoryPage>();
  const [deleting, setDeleting] = useState<SavedMemory>();
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => setOlder(undefined), [first]);

  const saved: Loaded<SavedMemoryPage> = first.kind === "ok" && older
    ? { kind: "ok", value: { items: [...first.value.items, ...older.items], next: older.next, agent_writes: first.value.agent_writes, agent_scope: first.value.agent_scope } } : first;
  const plain = (result: Result<unknown>) => result.kind === "not_ready" ? NOT_READY : result.kind === "error" ? result.message : "";

  async function act(id: string, action: () => Promise<Result<void>>, done: string, reload: () => void): Promise<string> {
    setFailure("");
    setBusy(id);
    const result = session.noteResult(await action());
    setBusy(null);
    const failed = plain(result);
    if (result.kind === "ok") session.showToast(done);
    else setFailure(failed);
    reload();
    return failed;
  }

  const more = async () => {
    if (saved.kind !== "ok" || saved.value.next === null) return;
    const page = session.noteResult(await savedMemories(scope || null, search, saved.value.next));
    if (page.kind === "ok") setOlder({ items: [...(older?.items ?? []), ...page.value.items], next: page.value.next, agent_writes: page.value.agent_writes, agent_scope: page.value.agent_scope });
    else setFailure(plain(page));
  };

  return <>
    <MemoryView saved={saved} writesRead={first} suggested={suggested} scope={scope} projects={projects} search={search} failure={failure} busy={busy}
      onSearch={setSearch} onScope={setScope} onRetry={reloadSaved} onMore={() => void more()} onDelete={setDeleting}
      onApprove={(memory) => void act(memory.id, () => approveMemory(memory.id), "Memory approved. Agents get it from their next launch.", reloadSaved)}
      onWrites={(writes) => act("agent-writes", () => setAgentMemoryWrites(writes), writes === "ask" ? "Agents now ask you before a memory is saved." : "Agents now save memories directly.", reloadSaved)}
      onMove={(memory, project) => void act(memory.id, () => moveMemory(memory.id, project), `Memory moved. Only agents in ${projects.find((item) => item.id === project)?.name ?? "that project"} get it now.`, reloadSaved)}
      onReach={(reach) => act("agent-scope", () => setAgentMemoryScope(reach), reach === "agent" ? "Agents now choose where a memory is saved, app-wide included." : "Agents now save memories in their own project only.", reloadSaved)}
      onAdd={async (text) => {
        setFailure("");
        const result = session.noteResult(await addMemory(scope || null, text));
        if (result.kind !== "ok") { setFailure(plain(result)); return false; }
        session.showToast("Saved to memory.");
        reloadSaved();
        return true;
      }}
      onImport={async (project) => {
        setFailure("");
        const picked = session.noteResult(await pickFolder("memory"));
        if (picked.kind !== "ok") { setFailure(plain(picked)); return ""; }
        if (picked.value === null) return "";
        const result = session.noteResult(await importMemory(project, picked.value));
        reloadSaved();
        if (result.kind !== "ok") { setFailure(plain(result)); return ""; }
        const done = `Imported ${result.value.imported}, skipped ${result.value.skipped}.`;
        session.showToast(done);
        return done;
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
