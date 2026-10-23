import { useEffect, useRef, useState } from "react";
import { call, failureCopy, loadContextText, nextGeneration, receiptState, saveResearchToMemory } from "../../api-projects";
import type { ContextItem, EngineResult, ResearchKind } from "../../api-projects";
import { displayText } from "../conversation";
import { formatBytes } from "../new-project/wizard";
import { useLinkOpener } from "./links";
import { MarkdownView, headings, markdownLinks, parseMarkdown } from "./markdown";
import { folderLabel } from "./overview";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

export const MEMORY_TEXT_BYTES_MAX = 4096;

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
const extension = (path: string) => path.slice(path.lastIndexOf(".") + 1).toLowerCase();

function Group({ title, items, selected, onSelect, running }: {
  title: string; items: ContextItem[]; selected: string | null; onSelect: (path: string) => void; running: Set<string>;
}) {
  return <li><span className="pj-folder">{title}</span><ul>{items.length
    ? items.map((item) => <li key={item.path}><button type="button" className="pj-titem" aria-current={selected === item.path || undefined} onClick={() => onSelect(item.path)}>
      {displayText(item.path.slice(item.path.lastIndexOf("/") + 1))}{running.has(item.path) && <span className="pj-badge t-info">Writing</span>}</button></li>)
    : <li><span className="pj-folder pj-muted">(empty)</span></li>}</ul></li>;
}

/** Context tab: the project folder's brief, documents and research reports, with a reader.
 * Input: the project data. Output: the tree and the reader; reports render as safe Markdown. */
export function ContextTab({ data }: { data: ProjectData }) {
  const context = valueOf(data.context);
  const jobs = valueOf(data.research)?.jobs ?? [];
  const items = context?.items ?? [];
  const [selected, setSelected] = useState<string | null>(() => hashParam("file"));
  const [file, setFile] = useState<EngineResult<{ text: string | null; truncated: boolean }>>();
  const [saving, setSaving] = useState<{ heading: string | null } | null>(null);
  const [saveState, setSaveState] = useState<{ busy: boolean; error: string }>({ busy: false, error: "" });
  const [actionNote, setActionNote] = useState("");
  const open = useLinkOpener(data.notify);
  const live = useRef(selected);
  live.current = selected;
  const chosen = selected ?? items.find((item) => item.kind === "brief")?.path ?? items[0]?.path ?? null;
  const item = items.find((entry) => entry.path === chosen) ?? null;
  const running = new Set(jobs.filter((job) => job.status === "running").map((job) => job.report_path));
  const writing = chosen !== null && running.has(chosen);
  const kind = chosen ? researchKindOf(chosen, jobs) : null;

  useEffect(() => {
    setFile(undefined);
    setSaving(null);
    setActionNote("");
    if (!chosen) return;
    const path = chosen;
    const read = () => void loadContextText(data.id, path).then((result) => { if ((live.current ?? path) === path) setFile(result); });
    read();
    if (!writing) return;
    const timer = window.setInterval(read, 5000);
    return () => window.clearInterval(timer);
  }, [data.id, chosen, writing]);

  const text = file?.kind === "ok" ? file.value.text : null;
  const markdown = chosen !== null && extension(chosen) === "md";
  const blocks = text !== null && markdown ? parseMarkdown(text) : [];
  const sources = markdownLinks(blocks);
  const preview = saving && text !== null ? sectionText(text, saving.heading) : null;
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
      setSaveState({ busy: false, error: state.kind === "unconfirmed" ? "We couldn't confirm your last change. Check Settings, Memory before saving again." : failureCopy(state.code) });
      return;
    }
    setSaveState({ busy: false, error: "" });
    setSaving(null);
    data.notify("Saved to memory as Suggested. Approve it in Settings, Memory.");
  }

  async function fileAction(command: string, label: string) {
    if (!chosen) return;
    const result = await call<unknown>(command, { id: data.id, path: chosen });
    setActionNote(result.kind === "ok" ? "" : result.kind === "not_ready" ? `Not ready yet: ${label} isn't in this build.` : failureCopy(result.code));
  }

  const select = (path: string) => {
    setSelected(path);
    data.navigate(`/projects/${encodeURIComponent(data.id)}/context?file=${encodeURIComponent(path)}`);
  };

  return <div className="pj-ctx">
    <nav className="pj-tree" aria-label="Project folder">
      {data.context === undefined && <p className="pj-muted" role="status">Loading the project folder...</p>}
      {data.context?.kind === "not_ready" && <NotReady what="the project folder view isn't in this build." />}
      {data.context?.kind === "failed" && <p className="pj-error" role="alert">{failureCopy(data.context.code)}</p>}
      {context && <>
        <div className="pj-stack-s pj-tree-root"><span className="pj-mono">{displayText(folderLabel(context.folder))}</span>
          <div><button type="button" className="pj-btn" onClick={() => void fileAction("desktop_reveal_project_folder", "Show in Finder")}>Show in Finder</button></div></div>
        <ul>
          <li><button type="button" className="pj-titem pin" aria-current={chosen === "@memory" || undefined} onClick={() => setSelected("@memory")}>Project memory</button></li>
          <li><span className="pj-folder">context/</span><ul>
            {items.filter((entry) => entry.kind === "brief").map((entry) => <li key={entry.path}><button type="button" className="pj-titem" aria-current={chosen === entry.path || undefined} onClick={() => select(entry.path)}>brief.md</button></li>)}
            <Group title="documents/" items={items.filter((entry) => entry.kind === "document")} selected={chosen} onSelect={select} running={running} />
            <Group title="research/" items={items.filter((entry) => entry.kind === "research")} selected={chosen} onSelect={select} running={running} />
          </ul></li>
        </ul>
      </>}
    </nav>
    <div className="pj-reader">
      {chosen === "@memory" && <><div className="pj-reader-tb"><h2>Project memory</h2></div>
        <NotReady what="the project memory list isn't in this build. Saved and suggested memories show in Settings, Memory." /></>}
      {chosen !== "@memory" && !item && context && <div className="pj-empty"><h3>{items.length ? "Pick a file" : "Nothing here yet"}</h3>
        <p className="pj-secondary">Research reports appear here as agents finish.</p></div>}
      {chosen !== "@memory" && item && <>
        <div className="pj-reader-tb">
          <h2 className="pj-mono">{displayText(item.path)}</h2>
          <span className="pj-row">
            {kind && <button type="button" className="pj-btn" disabled={text === null} onClick={() => setSaving({ heading: null })}>Save to memory</button>}
            <button type="button" className="pj-btn" onClick={() => void fileAction("desktop_open_context_item", "Open in default app")}>Open in default app</button>
            {item.kind !== "brief" && <button type="button" className="pj-btn danger-text" onClick={() => void fileAction("desktop_delete_context_item", "Delete")}>Delete</button>}
          </span>
        </div>
        {actionNote && <p className="pj-note warn" role="status">{actionNote}</p>}
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
            <button type="button" className="pj-btn" onClick={() => setSaving(null)}>Cancel</button>
            <button type="button" className="pj-btn primary" disabled={saveState.busy || previewBytes === 0 || previewBytes > MEMORY_TEXT_BYTES_MAX} onClick={() => void save()}>{saveState.busy ? "Saving..." : "Save to memory"}</button>
          </div>
        </div>}
        {file === undefined && <p className="pj-muted" role="status">Opening {displayText(item.path)}...</p>}
        {file?.kind === "not_ready" && <NotReady what="reading project files isn't in this build." />}
        {file?.kind === "failed" && <p className="pj-error" role="alert">{failureCopy(file.code)}</p>}
        {file?.kind === "ok" && text === null && <p className="pj-secondary">{formatBytes(item.bytes)}. A preview isn't shown for this kind of file. Use Open in default app.</p>}
        {text !== null && markdown && <article className="pj-md">
          <MarkdownView source={text} onOpenLink={open} headingAction={kind ? (heading) => <button type="button" className="pj-btn quiet pj-small" aria-label={`Save section ${heading} to memory`} onClick={() => setSaving({ heading })}>Save section</button> : undefined} />
          {sources.length > 0 && <section className="pj-sources" aria-labelledby="pj-sources"><h3 id="pj-sources">Sources</h3>
            <ol>{sources.map((source) => <li key={source.href}><a href={source.href} target="_blank" rel="noopener noreferrer" onClick={(event) => { event.preventDefault(); open(source.href); }}>{displayText(source.text)}</a>
              {source.text !== source.href && <span className="pj-small pj-muted pj-mono"> {displayText(source.href)}</span>}</li>)}</ol></section>}
        </article>}
        {text !== null && !markdown && TEXT_TYPES.includes(extension(item.path)) && <pre className="pj-preview">{displayText(text)}</pre>}
        {file?.kind === "ok" && file.value.truncated && <p className="pj-note">Only the first 1 MB is shown.</p>}
      </>}
    </div>
  </div>;
}
