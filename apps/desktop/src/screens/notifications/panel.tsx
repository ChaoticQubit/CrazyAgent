import { useEffect, useRef, useState } from "react";
import type { Result } from "../../api";
import {
  NOTICE_FILTERS, badgeText, countsQuery, foldsAction, kindLabel, loadNotifications, panelQuery, projectNeeds, projectNeedsQuery,
  routeOf, sendNotice, signInsQuery, type FoldChange, type Notice, type NoticeAction, type NoticeCounts, type NoticeFilter, type NoticePage,
} from "../../api-notifications";
import { Icon, LoadError } from "../../ui";
import { displayText } from "../conversation";
import "./notifications.css";

type Loaded<T> = { kind: "loading" } | Result<T>;

/** One project the panel's picker offers. */
export interface PickedProject {
  id: string;
  name: string;
}

export const COUNTS_POLL_MS = 3000;
export const NEEDS_POLL_MS = 5000;
const HIGHLIGHT_TRIES_MAX = 30;
const HIGHLIGHT_TRY_MS = 100;
const HIGHLIGHT_MS = 2500;
const NOT_READY_LINE = "The engine is not ready yet. Try again.";
const REFUSED_LINE = "Couldn't change your notifications. Try again.";

const EMPTY: Record<NoticeFilter, string> = {
  all: "No notifications yet.",
  unread: "You're all caught up.",
  pending: "Nothing waits for you.",
  done: "Nothing is done yet.",
};

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The words under a folded row. Input: the row. Output: how many it holds and how many of
 * them are unread. */
function foldWords(notice: Notice): string {
  assert(notice.folded > 1, "only a fold of two or more says how many it holds");
  return `${notice.folded} like this${notice.unread > 0 ? `, ${notice.unread} unread` : ""}`;
}

/** The bell top right on every page, with the unread and pending badges and both counts in
 * its name for a screen reader. Inputs: the counts, whether the panel is open and the toggle.
 * Output: the button. */
export function NoticeBellView({ counts, open, onToggle }: { counts: NoticeCounts; open: boolean; onToggle: () => void }) {
  const unread = badgeText(counts.unread);
  const pending = badgeText(counts.pending);
  const label = unread === null && pending === null ? "Notifications: nothing new" : `Notifications: ${counts.unread} unread, ${counts.pending} pending`;
  return <button type="button" className="notice-bell" aria-label={label} aria-expanded={open} aria-haspopup="dialog" onClick={onToggle}>
    <Icon name="bell" size={18} />
    {unread !== null && <span className="notice-badge unread" aria-hidden="true">{unread}</span>}
    {pending !== null && <span className="notice-badge pending" aria-hidden="true">{pending}</span>}
  </button>;
}

/** One row: its kind, its project, its text and, for a fold, how many it holds; a pick box
 * when rows can be picked. Inputs: the row, its project's name, whether it is picked, the
 * open action and the pick action. Output: the list item. */
function NoticeRow({ notice, projectName, picked, onOpen, onPick }: {
  notice: Notice;
  projectName: string | null;
  picked: boolean;
  onOpen: (notice: Notice) => void;
  onPick?: (id: number) => void;
}) {
  const classes = ["notice-row", notice.unread > 0 ? "unread" : "", notice.pending > 0 ? "pending" : ""].filter(Boolean).join(" ");
  return <li data-notice={notice.id} className={classes}>
    {onPick && <input type="checkbox" checked={picked} aria-label={`Pick ${kindLabel(notice.kind)}`} onChange={() => onPick(notice.id)} />}
    <button type="button" className="notice-open" onClick={() => onOpen(notice)}>
      <span className="notice-kind">{kindLabel(notice.kind)}</span>
      {projectName !== null && <span className="notice-project">{displayText(projectName)}</span>}
      <span className="notice-text">{displayText(notice.text)}</span>
      {notice.folded > 1 && <span className="notice-fold">{foldWords(notice)}</span>}
    </button>
  </li>;
}

/** The panel the bell opens: the four filters, the project picker, Mark all read, the
 * actions on picked rows, and one row per item or fold with its words when empty. Inputs:
 * the page, the filter, the project picked, the projects, the picked row ids and the
 * actions. Output: the dialog. */
export function NoticePanelView({ page, filter, projectId, projects, selected, problem, onFilter, onProject, onOpen, onSelect, onMarkAllRead,
  onReadSelected, onUnreadSelected, onDeleteSelected, onRetry }: {
  page: Loaded<NoticePage>;
  filter: NoticeFilter;
  projectId: string | null;
  projects: readonly PickedProject[];
  selected: ReadonlySet<number>;
  problem: string;
  onFilter: (filter: NoticeFilter) => void;
  onProject: (projectId: string | null) => void;
  onOpen: (notice: Notice) => void;
  onSelect: (id: number) => void;
  onMarkAllRead: () => void;
  onReadSelected: () => void;
  onUnreadSelected: () => void;
  onDeleteSelected: () => void;
  onRetry: () => void;
}) {
  const names = new Map(projects.map((project) => [project.id, project.name]));
  const nameOf = (notice: Notice) => notice.project_id === null ? null : names.get(notice.project_id) ?? "A removed project";
  return <div role="dialog" aria-label="Notifications" className="notice-panel">
    <div className="notice-head">
      <fieldset className="notice-filters">
        <legend className="visually-hidden">Show</legend>
        {NOTICE_FILTERS.map((item) => <label key={item.id} className={filter === item.id ? "on" : undefined}>
          <input type="radio" name="notice-filter" value={item.id} checked={filter === item.id} onChange={() => onFilter(item.id)} />{item.label}
        </label>)}
      </fieldset>
      <select aria-label="Project" value={projectId ?? ""} onChange={(event) => onProject(event.target.value === "" ? null : event.target.value)}>
        <option value="">All projects</option>
        {projects.map((project) => <option key={project.id} value={project.id}>{displayText(project.name)}</option>)}
      </select>
      <button type="button" className="btn small quiet" onClick={onMarkAllRead}>Mark all read</button>
    </div>
    {selected.size > 0 && <div className="notice-bulk" role="toolbar" aria-label="Picked notifications">
      <span>{selected.size} selected</span>
      <button type="button" className="btn small" onClick={onReadSelected}>Mark read</button>
      <button type="button" className="btn small" onClick={onUnreadSelected}>Mark unread</button>
      <button type="button" className="btn small danger" onClick={onDeleteSelected}>Delete</button>
    </div>}
    {problem && <p className="notice-problem" role="alert">{problem}</p>}
    {page.kind === "loading" && <p className="notice-empty">Loading notifications…</p>}
    {page.kind === "not_ready" && <p className="notice-empty">Notifications aren't ready in this engine yet.</p>}
    {page.kind === "error" && <LoadError message={`Couldn't load notifications. ${page.message}`} onRetry={onRetry} />}
    {page.kind === "ok" && page.value.items.length === 0 && <p className="notice-empty">{EMPTY[filter]}</p>}
    {page.kind === "ok" && page.value.items.length > 0 && <ul className="notice-list">
      {page.value.items.map((notice) => <NoticeRow key={notice.id} notice={notice} projectName={nameOf(notice)} picked={selected.has(notice.id)}
        onOpen={onOpen} onPick={onSelect} />)}
    </ul>}
    {page.kind === "ok" && page.value.more && <p className="notice-empty">Showing the newest {page.value.items.length}.</p>}
  </div>;
}

/** Sends one change to the store and says how it went. Inputs: the change and the way to
 * send it. Output: an empty string once it stands, else the plain refusal to show. */
export async function changeProblem(action: NoticeAction, send: (action: NoticeAction) => Promise<Result<void>>): Promise<string> {
  const result = await send(action);
  if (result.kind === "ok") return "";
  if (result.kind === "not_ready") return NOT_READY_LINE;
  return result.message === "" ? REFUSED_LINE : result.message;
}

/** Opens one row's exact page and marks what the row holds read: one item by its id, a fold
 * of several by the fold, so a single item never reaches older items of its kind. An Action
 * stays pending until its state is fixed. Inputs: the row, the filter it was listed under,
 * and the way to go and to send. Output: the anchor of the part to highlight, or null, and
 * the refusal of the read, empty once it stands or when nothing was unread. */
export async function openNotice(notice: Notice, filter: NoticeFilter, { go, send }: {
  go: (hash: string) => void;
  send: (action: NoticeAction) => Promise<Result<void>>;
}): Promise<{ anchor: string | null; problem: string }> {
  const { hash, anchor } = routeOf(notice.target);
  assert(hash.startsWith("#/"), "a notification opens a page of the app");
  assert(notice.folded >= 1, "a row holds at least one item");
  go(hash);
  if (notice.unread === 0) return { anchor, problem: "" };
  const action = notice.folded === 1 ? { read: { ids: [notice.id] } } : foldsAction([notice], filter, "read");
  assert(action !== null, "one row is always a valid change");
  return { anchor, problem: await changeProblem(action, send) };
}

/** The picks still listed after a reload: a fold that gained an item lists under a newer
 * id, so its old pick no longer names a row. Inputs: the picks and the page. Output: the
 * picks the page lists. */
export function listedPicks(selected: ReadonlySet<number>, page: NoticePage): Set<number> {
  const listed = new Set(page.items.map((item) => item.id));
  const kept = new Set([...selected].filter((id) => listed.has(id)));
  assert(kept.size <= selected.size, "pruning never adds a pick");
  return kept;
}

/** Marks the part a notification opened once the page draws it: scrolls to it and lights it
 * for a moment. Input: the anchor. Output: none; the page may never draw it. */
export function highlight(anchor: string | null): void {
  if (anchor === null || typeof document === "undefined") return;
  let tries = 0;
  const look = () => {
    tries += 1;
    const element = document.getElementById(anchor) ?? document.querySelector(`[data-notice-anchor="${CSS.escape(anchor)}"]`);
    if (element instanceof HTMLElement) {
      element.scrollIntoView({ block: "center" });
      element.classList.add("notice-highlight");
      window.setTimeout(() => element.classList.remove("notice-highlight"), HIGHLIGHT_MS);
      return;
    }
    if (tries < HIGHLIGHT_TRIES_MAX) window.setTimeout(look, HIGHLIGHT_TRY_MS);
  };
  look();
}

/** Goes to one location hash, as a click on a link would. Input: the hash. Output: none. */
export function goTo(hash: string): void {
  window.location.hash = hash;
}

/** The panel with its reads: the folded page for the filter and project, reloaded when the
 * store's newest item changes, and the actions that change the store. Inputs: the projects,
 * the store's newest id, the close action and the counts refresh. Output: the panel. */
function NoticePanel({ projects, newest, onClose, onChanged }: {
  projects: readonly PickedProject[];
  newest: number;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [filter, setFilter] = useState<NoticeFilter>("all");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<number>>(new Set());
  const [page, setPage] = useState<Loaded<NoticePage>>({ kind: "loading" });
  const [version, setVersion] = useState(0);
  const [problem, setProblem] = useState("");
  useEffect(() => {
    let live = true;
    void loadNotifications(panelQuery(filter, projectId)).then((result) => {
      if (!live) return;
      setPage(result);
      if (result.kind === "ok") setSelected((before) => listedPicks(before, result.value));
    });
    return () => { live = false; };
  }, [filter, projectId, newest, version]);
  const run = (action: NoticeAction) => void changeProblem(action, sendNotice).then((refused) => {
    setProblem(refused);
    if (refused !== "") return;
    setSelected(new Set());
    setVersion((before) => before + 1);
    onChanged();
  });
  const picked = (change: FoldChange) => {
    const rows = page.kind === "ok" ? page.value.items.filter((item) => selected.has(item.id)) : [];
    const action = foldsAction(rows, filter, change);
    if (action !== null) run(action);
  };
  return <NoticePanelView page={page} filter={filter} projectId={projectId} projects={projects} selected={selected} problem={problem}
    onFilter={(next) => { setFilter(next); setSelected(new Set()); setProblem(""); }}
    onProject={(next) => { setProjectId(next); setSelected(new Set()); setProblem(""); }}
    onOpen={(notice) => void openNotice(notice, filter, { go: goTo, send: sendNotice }).then(({ anchor, problem: refused }) => {
      setProblem(refused);
      if (refused === "") onClose();
      highlight(anchor);
      onChanged();
    })}
    onSelect={(id) => setSelected((before) => { const next = new Set(before); if (!next.delete(id)) next.add(id); return next; })}
    onMarkAllRead={() => run({ read_all: { project_id: projectId } })}
    onReadSelected={() => picked("read")} onUnreadSelected={() => picked("unread")} onDeleteSelected={() => picked("delete")}
    onRetry={() => setVersion((before) => before + 1)} />;
}

/** The bell top right on every page: reads both counts every few seconds and opens the
 * panel, which Escape or a click outside closes. Input: the projects the picker offers.
 * Output: the bell and, when open, the panel. */
export function NoticeBell({ projects }: { projects: readonly PickedProject[] }) {
  const [counts, setCounts] = useState<NoticeCounts>({ unread: 0, pending: 0 });
  const [newest, setNewest] = useState(0);
  const [open, setOpen] = useState(false);
  const [tick, setTick] = useState(0);
  const corner = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let live = true;
    let timer = 0;
    const read = async () => {
      const result = await loadNotifications(countsQuery());
      if (!live) return;
      if (result.kind === "ok") { setCounts(result.value.counts); setNewest(result.value.newest); }
      timer = window.setTimeout(() => void read(), COUNTS_POLL_MS);
    };
    void read();
    return () => { live = false; window.clearTimeout(timer); };
  }, [tick]);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setOpen(false); };
    const onDown = (event: MouseEvent) => { if (!corner.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("mousedown", onDown); };
  }, [open]);
  return <div className="notice-corner" ref={corner}>
    <NoticeBellView counts={counts} open={open} onToggle={() => setOpen(!open)} />
    {open && <NoticePanel projects={projects} newest={newest} onClose={() => setOpen(false)} onChanged={() => setTick((before) => before + 1)} />}
  </div>;
}

/** One project's Needs you view: its pending items from the store, folded, with the
 * pending sign-ins of its pool's accounts and the refusal of the last read on open;
 * nothing while it loads or when nothing waits. Inputs: the merged page, the refusal, the
 * open action and a retry. Output: the section, or nothing. */
export function ProjectNeedsYouView({ page, problem = "", onOpen, onRetry }: {
  page: Loaded<NoticePage>;
  problem?: string;
  onOpen: (notice: Notice) => void;
  onRetry: () => void;
}) {
  if (page.kind === "loading" || page.kind === "not_ready") return null;
  if (page.kind === "error") return <LoadError message={`Couldn't load what needs you. ${page.message}`} onRetry={onRetry} />;
  if (page.value.items.length === 0) return null;
  return <section className="pj-stack-s notice-needs" aria-labelledby="pj-needs-you">
    <h2 id="pj-needs-you">Needs you</h2>
    {problem && <p className="notice-problem" role="alert">{problem}</p>}
    <ul className="notice-list">
      {page.value.items.map((notice) => <NoticeRow key={notice.id} notice={notice} projectName={null} picked={false} onOpen={onOpen} />)}
    </ul>
  </section>;
}

/** Reads one project's Needs you view every few seconds and opens a row's page. Inputs: the
 * project and the account ids of its pool. Output: the view. */
export function ProjectNeedsYou({ projectId, accounts }: { projectId: string; accounts: readonly string[] }) {
  const [page, setPage] = useState<Loaded<NoticePage>>({ kind: "loading" });
  const [version, setVersion] = useState(0);
  const [problem, setProblem] = useState("");
  const pool = accounts.join("\n");
  useEffect(() => {
    let live = true;
    let timer = 0;
    const read = async () => {
      const [own, signIns] = await Promise.all([loadNotifications(projectNeedsQuery(projectId)), loadNotifications(signInsQuery())]);
      if (!live) return;
      setPage(projectNeeds(own, signIns, accounts));
      timer = window.setTimeout(() => void read(), NEEDS_POLL_MS);
    };
    void read();
    return () => { live = false; window.clearTimeout(timer); };
  }, [projectId, pool, version]);
  return <ProjectNeedsYouView page={page} problem={problem} onRetry={() => setVersion((before) => before + 1)}
    onOpen={(notice) => void openNotice(notice, "pending", { go: goTo, send: sendNotice }).then(({ anchor, problem: refused }) => {
      setProblem(refused);
      highlight(anchor);
      setVersion((before) => before + 1);
    })} />;
}
