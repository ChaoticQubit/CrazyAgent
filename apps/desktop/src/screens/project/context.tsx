import { addMemory, call, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState } from "react";
import { addProjectDocuments, deleteContextItem, failureCopy, loadContextText, pickDocuments, receiptState, saveResearchToMemory } from "../../api-projects";
import type { ContextItem, ResearchKind } from "../../api-projects";
import { Button, ConfirmDialog, Icon, Segmented, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { formatBytes } from "../new-project/wizard";
import { useLinkOpener } from "./links";
import { MarkdownView, headings, markdownLinks, parseMarkdown } from "./markdown";
import { folderLabel } from "./overview";
import { ProjectMemory } from "./memory";
import { deletable, deleteQuestion, fileIcon } from "./research";
import { NotReady, ProjectEmpty, valueOf } from "./shared";
import type { ProjectData } from "./shared";

export const MEMORY_TEXT_BYTES_MAX = 4096;
export const NOTE_BYTES_MAX = 2000;

/** Cuts the part of a report the engine saves for one heading, the same way the engine does:
 * from the heading line to the next heading of the same or a higher level.
 * Inputs: the report and a heading, or null for the whole report. Output: the text, or null. */
export function sectionText(report: string, heading: string | null): string | null {
  if (heading === null) return report.trim();
  const level = (line: string) => {
    const hashes = /^#+/u.exec(line)?.[0].length ?? 0;
    return hashes > 0 && line[hashes] === " " ? hashes : 0;
  };
  const lines = report.split(/\r?\n/u);
  const start = lines.findIndex((line) => level(line) > 0 && line.slice(level(line)).trim() === heading.trim());
  if (start < 0) return null;
  const depth = level(lines[start]);
  const offset = lines.slice(start + 1).findIndex((line) => level(line) > 0 && level(line) <= depth);
  const end = offset < 0 ? lines.length : start + 1 + offset;
  return lines.slice(start, end).join("\n").trim();
}

/** Cleans text the owner selected in the reader before it is saved: line endings become "\n"
 * and outer space goes. Input: the raw selection. Output: the text, or null when nothing is left. */
export function selectionText(raw: string): string | null {
  const text = raw.replace(/\r\n?/gu, "\n").trim();
  return text === "" ? null : text;
}

/** Checks a memory note the owner is about to save, with the engine's limit in bytes.
 * Input: the note as typed. Output: plain error copy, or null when it can be saved. */
export function noteError(text: string): string | null {
  const note = text.trim();
  if (note === "") return "Write what agents should remember.";
  return new TextEncoder().encode(note).length > NOTE_BYTES_MAX ? "This is too long for one memory. Keep it under 2,000 characters." : null;
}

/** Finds which research kind wrote a context file.
 * Inputs: the item path and the project's research jobs. Output: the kind, or null. */
export function researchKindOf(path: string, reports: { kind: ResearchKind; report_path: string }[]): ResearchKind | null {
  return reports.find((report) => report.report_path === path)?.kind ?? null;
}

function hashParam(name: string): string | null {
  if (typeof window === "undefined") return null;
  const query = window.location.hash.split("?")[1];
  return query ? new URLSearchParams(query).get(name) : null;
}

const TEXT_TYPES = ["md", "txt", "csv"];
const MEMORY = "@memory";
const extension = (path: string) => path.slice(path.lastIndexOf(".") + 1).toLowerCase();

function Group({ title, items, selected, onSelect, running }: {
  title: string; items: ContextItem[]; selected: string | null; onSelect: (path: string) => void; running: Set<string>;
}) {
  return <li><span className="pj-folder"><Icon name="folder" size={13} />{title}</span><ul>{items.length
    ? items.map((item) => <li key={item.path}><button type="button" className="pj-titem" aria-current={selected === item.path || undefined} onClick={() => onSelect(item.path)}>
      <Icon name={fileIcon(item.path)} size={14} />{displayText(item.path.slice(item.path.lastIndexOf("/") + 1))}{running.has(item.path) && <StatusBadge word="Writing" tone="info" />}</button></li>)
    : <li><span className="pj-folder pj-muted">(empty)</span></li>}</ul></li>;
}

/** Reads one context file for the reader, and again every five seconds while an agent is
 * still writing it. Inputs: the project, the file path, whether it is being written and where
 * to deliver each read. Output: a stop function; a read that answers after stop is dropped, so
 * a slow read never replaces the file the reader moved on to. */
export function followContext(project: string, path: string, writing: boolean, apply: (result: Result<{ text: string | null; truncated: boolean }>) => void): () => void {
  let current = true;
  const read = () => void loadContextText(project, path).then((result) => { if (current) apply(result); });
  read();
  const timer = writing ? window.setInterval(read, 5000) : undefined;
  return () => {
    current = false;
    if (timer !== undefined) window.clearInterval(timer);
  };
}

/** Context tab: the project folder's brief, documents and research reports, with a reader.
 * Input: the project data. Output: the tree and the reader; reports render as safe Markdown. */
export function ContextTab({ data }: { data: ProjectData }) {
  const context = valueOf(data.context);
  const jobs = valueOf(data.research)?.jobs ?? [];
  const items = context?.items ?? [];
  const [selected, setSelected] = useState<string | null>(() => hashParam("file"));
  const [file, setFile] = useState<Result<{ text: string | null; truncated: boolean }>>();
  const [saving, setSaving] = useState<{ heading: string | null } | null>(null);
  const [noting, setNoting] = useState<{ text: string; scope: "project" | "all"; busy: boolean; error: string } | null>(null);
  const [selection, setSelection] = useState<string | null>(null);
  const reading = useRef<HTMLDivElement>(null);
  const [saveState, setSaveState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  const [actionNote, setActionNote] = useState("");
  const [deleting, setDeleting] = useState<{ path: string; busy: boolean } | null>(null);
  const [adding, setAdding] = useState<{ path: string; busy: boolean; error: string } | null>(null);
  const open = useLinkOpener(data.notify);
  const chosen = selected ?? items.find((item) => item.kind === "brief")?.path ?? items[0]?.path ?? null;
  const item = items.find((entry) => entry.path === chosen) ?? null;
  const running = new Set(jobs.filter((job) => job.status === "running").map((job) => job.report_path));
  const writing = chosen !== null && running.has(chosen);
  const kind = chosen ? researchKindOf(chosen, jobs) : null;

  useEffect(() => {
    setFile(undefined);
    setSaving(null);
    setNoting(null);
    setSelection(null);
    setActionNote("");
    if (!chosen || chosen === MEMORY) return;
    return followContext(data.id, chosen, writing, setFile);
  }, [data.id, chosen, writing]);

  const text = file?.kind === "ok" ? file.value.text : null;
  const markdown = chosen !== null && extension(chosen) === "md";
  const blocks = text !== null && markdown ? parseMarkdown(text) : [];
  const sources = markdownLinks(blocks);
  const preview = saving && text !== null ? sectionText(text, saving.heading) : null;

  const readSelection = () => {
    const chosenText = window.getSelection();
    const inside = chosenText !== null && chosenText.rangeCount > 0 && reading.current?.contains(chosenText.getRangeAt(0).commonAncestorContainer);
    setSelection(inside ? selectionText(chosenText.toString()) : null);
  };
  const previewBytes = preview === null ? 0 : new TextEncoder().encode(preview).length;

  async function save() {
    if (!saving || !kind) return;
    setSaveState({ busy: true, error: "" });
    const result = await saveResearchToMemory(data.id, kind, saving.heading, nextGeneration());
    if (result.kind !== "ok") {
      setSaveState({ busy: false, error: result.kind === "not_ready" ? "Not ready yet: saving to memory isn't in this build." : failureCopy(result.code) });
      return;
    }
    const state = receiptState(result.value);
    if (state.kind !== "applied") {
      setSaveState({ busy: false, error: state.kind === "unconfirmed" ? "We couldn't confirm your last change. Check Project memory before saving again." : failureCopy(state.code) });
      return;
    }
    setSaveState({ busy: false, error: "" });
    setSaving(null);
    data.notify("Saved to memory as Suggested. Approve it in Project memory.");
  }

  async function saveNote() {
    if (!noting) return;
    const problem = noteError(noting.text);
    if (problem) return setNoting({ ...noting, error: problem });
    setNoting({ ...noting, busy: true, error: "" });
    const result = await addMemory(noting.scope === "project" ? data.id : null, noting.text.trim());
    if (result.kind !== "ok") return setNoting({ ...noting, busy: false, error: result.kind === "not_ready" ? "Not ready yet: saving a memory isn't in this build." : result.message });
    setNoting(null);
    setSelection(null);
    data.notify(noting.scope === "project" ? "Saved to this project's memory." : "Saved to memory for every project.");
  }

  async function fileAction(command: string, label: string) {
    if (!chosen) return;
    const result = await call<unknown>(command, { id: data.id, path: chosen });
    setActionNote(result.kind === "ok" ? "" : result.kind === "not_ready" ? `Not ready yet: ${label} isn't in this build.` : failureCopy(result.code));
  }

  async function confirmDelete() {
    if (!deleting) return;
    const path = deleting.path;
    setDeleting({ path, busy: true });
    const result = await deleteContextItem(data.id, path, nextGeneration());
    setDeleting(null);
    if (result.kind !== "ok") return setActionNote(result.kind === "not_ready" ? "Not ready yet: deleting files isn't in this build." : failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind !== "applied") return setActionNote(state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check the folder before trying again.");
    data.notify(`${path.slice(path.lastIndexOf("/") + 1)} deleted.`);
    setSelected(null);
    data.navigate(`/projects/${encodeURIComponent(data.id)}/context`);
    data.reload();
  }

  async function addDocuments(paths: string[]) {
    setAdding({ path: "", busy: true, error: "" });
    const result = await addProjectDocuments(data.id, paths, nextGeneration());
    if (result.kind !== "ok") return setAdding({ path: paths.join(""), busy: false, error: result.kind === "not_ready" ? "Not ready yet: adding documents isn't in this build." : failureCopy(result.code) });
    const state = receiptState(result.value);
    if (state.kind !== "applied") return setAdding({ path: paths.join(""), busy: false, error: state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check the folder before trying again." });
    setAdding(null);
    data.notify(paths.length === 1 ? "Document added." : `${paths.length} documents added.`);
    data.reload();
  }

  async function chooseDocuments() {
    const picked = await pickDocuments();
    if (picked.kind === "ok") { if (picked.value.length) void addDocuments(picked.value); return; }
    setAdding({ path: "", busy: false, error: picked.kind === "not_ready" ? "" : failureCopy(picked.code) });
  }

  const select = (path: string) => {
    setSelected(path);
    data.navigate(`/projects/${encodeURIComponent(data.id)}/context?file=${encodeURIComponent(path)}`);
  };

  return <div className="pj-ctx">
    <nav className="pj-tree" aria-label="Project folder">
      {data.context === undefined && <p className="pj-muted" role="status">Loading the project folder...</p>}
      {data.context?.kind === "not_ready" && <NotReady what="the project folder view isn't in this build." />}
      {data.context?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(data.context.code)}</p>}
      {context && <>
        <div className="pj-stack-s pj-tree-root"><span className="pj-mono" title={context.folder}>{displayText(folderLabel(context.folder))}</span>
          <div className="pj-row"><Button size="small" icon="folder-open" onClick={() => void fileAction("desktop_reveal_project_folder", "Show in Finder")}>Show in Finder</Button>
            <Button size="small" icon="plus" onClick={() => void chooseDocuments()}>Add documents</Button></div>
          {adding && <form className="pj-stack-s" onSubmit={(event) => { event.preventDefault(); if (adding.path.trim()) void addDocuments([adding.path.trim()]); }}>
            <label htmlFor="pj-add-doc" className="pj-small">{adding.error ? "Or type the full path of a file on this Mac:" : "The file picker isn't in this build. Full path of a file on this Mac:"}</label>
            <input id="pj-add-doc" className="pj-input" value={adding.path} onChange={(event) => setAdding({ ...adding, path: event.target.value, error: "" })} />
            {adding.error && <p className="pj-error" role="alert">{displayText(adding.error)}</p>}
            <span className="pj-row"><Button type="submit" size="small" variant="primary" pending={adding.busy} pendingLabel="Adding..." disabled={!adding.path.trim()}>Add file</Button>
              <Button size="small" variant="quiet" onClick={() => setAdding(null)}>Cancel</Button></span>
          </form>}</div>
        <ul>
          <li><button type="button" className="pj-titem pin" aria-current={chosen === MEMORY || undefined} onClick={() => select(MEMORY)}><Icon name="brain" size={14} />Project memory</button></li>
          <li><span className="pj-folder"><Icon name="folder" size={13} />context/</span><ul>
            {items.filter((entry) => entry.kind === "brief").map((entry) => <li key={entry.path}><button type="button" className="pj-titem" aria-current={chosen === entry.path || undefined} onClick={() => select(entry.path)}><Icon name="file-text" size={14} />brief.md</button></li>)}
            <Group title="documents/" items={items.filter((entry) => entry.kind === "document")} selected={chosen} onSelect={select} running={running} />
            <Group title="research/" items={items.filter((entry) => entry.kind === "research")} selected={chosen} onSelect={select} running={running} />
          </ul></li>
        </ul>
      </>}
    </nav>
    <div className="pj-reader">
      {chosen === MEMORY && <ProjectMemory data={data} />}
      {chosen !== MEMORY && !item && context && <ProjectEmpty title={items.length ? "Pick a file" : "Nothing here yet"}>
        {items.length ? "Pick a file on the left to read it." : "Research reports appear here as agents finish."}</ProjectEmpty>}
      {chosen !== MEMORY && item && <>
        <div className="pj-reader-tb">
          <h2 className="pj-mono">{displayText(item.path)}</h2>
          <span className="pj-row">
            {selection && <Button icon="brain" onClick={() => { setSaving(null); setNoting({ text: selection, scope: "project", busy: false, error: "" }); }}>Save selection</Button>}
            {kind && <button type="button" className="btn" disabled={text === null} onClick={() => setSaving({ heading: null })}>Save to memory</button>}
            {item.kind === "brief" && <button type="button" className="btn" title="The brief is written from the project details"
              onClick={() => data.navigate(`/projects/${encodeURIComponent(data.id)}/settings?section=general`)}>Edit</button>}
            <button type="button" className="btn" onClick={() => void fileAction("desktop_open_context_item", "Open in default app")}>Open in default app</button>
            {deletable(item) && <button type="button" className="btn danger-text" onClick={() => setDeleting({ path: item.path, busy: false })}>Delete</button>}
          </span>
        </div>
        {actionNote && <p className="pj-note warn" role="status">{actionNote}</p>}
        <ConfirmDialog open={deleting !== null} title={deleteQuestion(deleting?.path ?? item.path)} confirmLabel="Delete" pending={deleting?.busy}
          onCancel={() => setDeleting(null)} onConfirm={() => void confirmDelete()}>
          It's removed from the project folder. Agents stop using it.
        </ConfirmDialog>
        {writing && <p className="pj-note">{displayText(jobs.find((job) => job.report_path === chosen)?.kind_label ?? "Research")} is still writing this file. It updates as it goes.</p>}
        {saving && <div className="pj-popover" role="dialog" aria-labelledby="pj-save-title">
          <h3 id="pj-save-title">Save to memory</h3>
          <label htmlFor="pj-save-part" className="pj-label">What to save</label>
          <select id="pj-save-part" className="pj-select" value={saving.heading ?? ""} onChange={(event) => setSaving({ heading: event.target.value || null })}>
            <option value="">The whole report</option>
            {headings(blocks).map((heading) => <option key={heading} value={heading}>Section: {heading}</option>)}
          </select>
          <p className="pj-small pj-muted">Saved for: this project.</p>
          <pre className="pj-preview">{displayText(preview ?? "")}</pre>
          {previewBytes > MEMORY_TEXT_BYTES_MAX && <p className="pj-note warn">This is {formatBytes(previewBytes)}. Memories can be up to 4 KB. Pick one section instead.</p>}
          {saveState.error && <p className="pj-error" role="alert">{saveState.error}</p>}
          <div className="pj-row end">
            <button type="button" className="btn" onClick={() => setSaving(null)}>Cancel</button>
            <button type="button" className="btn primary" disabled={saveState.busy || previewBytes === 0 || previewBytes > MEMORY_TEXT_BYTES_MAX} onClick={() => void save()}>{saveState.busy ? "Saving..." : "Save to memory"}</button>
          </div>
        </div>}
        {noting && <form className="pj-popover" role="dialog" aria-labelledby="pj-note-title" onSubmit={(event) => { event.preventDefault(); void saveNote(); }}>
          <h3 id="pj-note-title">Save to memory</h3>
          <label htmlFor="pj-note-text" className="pj-label">This is what agents will remember</label>
          <textarea id="pj-note-text" className="pj-input" rows={4} value={noting.text} aria-invalid={noting.error ? true : undefined}
            aria-describedby={noting.error ? "pj-note-error" : undefined} onChange={(event) => setNoting({ ...noting, text: event.target.value, error: "" })} />
          <span className="pj-label">Use it in</span>
          <Segmented label="Use it in" value={noting.scope} onValue={(scope) => setNoting({ ...noting, scope })}
            options={[{ value: "project", label: "This project" }, { value: "all", label: "All projects" }]} />
          {noting.error && <p className="pj-error" id="pj-note-error" role="alert">{displayText(noting.error)}</p>}
          <div className="pj-row end">
            <Button onClick={() => setNoting(null)}>Cancel</Button>
            <Button type="submit" variant="primary" icon="brain" pending={noting.busy} pendingLabel="Saving...">Save to memory</Button>
          </div>
        </form>}
        {file === undefined && <p className="pj-muted" role="status">Opening {displayText(item.path)}...</p>}
        {file?.kind === "not_ready" && <NotReady what="reading project files isn't in this build." />}
        {file?.kind === "error" && <p className="pj-error" role="alert">{failureCopy(file.code)}</p>}
        {file?.kind === "ok" && text === null && <p className="pj-secondary">{formatBytes(item.bytes)}. A preview isn't shown for this kind of file. Use Open in default app.</p>}
        <div ref={reading} onMouseUp={readSelection} onKeyUp={readSelection}>
        {text !== null && markdown && <article className="pj-md">
          <MarkdownView source={text} onOpenLink={open} headingAction={kind ? (heading) => <button type="button" className="btn quiet small" aria-label={`Save section ${heading} to memory`} onClick={() => setSaving({ heading })}>Save section</button> : undefined} />
          {sources.length > 0 && <section className="pj-sources" aria-labelledby="pj-sources"><h3 id="pj-sources">Sources</h3>
            <ol>{sources.map((source) => <li key={source.href}><a href={source.href} target="_blank" rel="noopener noreferrer" onClick={(event) => { event.preventDefault(); open(source.href); }}>{displayText(source.text)}</a>
              {source.text !== source.href && <span className="pj-small pj-muted pj-mono"> {displayText(source.href)}</span>}</li>)}</ol></section>}
        </article>}
        {text !== null && !markdown && TEXT_TYPES.includes(extension(item.path)) && <pre className="pj-preview">{displayText(text)}</pre>}
        </div>
        {file?.kind === "ok" && file.value.truncated && <p className="pj-note">Only the first 1 MB is shown.</p>}
      </>}
    </div>
  </div>;
}
