import { useEffect, useId, useState } from "react";
import type { ReactNode } from "react";
import type { Failure, Result } from "../../api";
import {
  KINDS, STALE_LINE, STATUSES, attachProject, filtersActive, isClosed, keyProblem, kindLabel, loadReady, loadTickets, moveTicket,
  notReady, opened, priorityLabel, statusLabel, suggestKey, trackedProject, trackerOp, trackerProjects, trackerRefusal, triageBadge,
} from "../../api-tracker";
import type { ListFilters, ReadyRow, ShowPage, TicketFields, TicketRow, TicketStatus, TrackedProject } from "../../api-tracker";
import { Button, Segmented, Select, TextField } from "../../ui";
import { displayText } from "../conversation";
import { Badge, NotReady, ProjectEmpty } from "./shared";
import type { ProjectData } from "./shared";
import { TicketDetail } from "./ticket-detail";
import "./tickets.css";

const LIST_POLL_MS = 5000;
const LIST_PAGES_MAX = 10;
const OPEN: readonly TicketStatus[] = ["triage", "backlog", "todo", "in_progress", "in_review"];
const NO_FILTERS: ListFilters = { status: "", kind: "", assignee: "" };

type View = "list" | "ready" | "board";

/** One board column: an open status, or Closed for done and cancelled, which takes no drop. */
export interface BoardColumn { status: TicketStatus | "closed"; label: string; rows: TicketRow[]; drop: boolean }

/** What a board drop does once the ticket is read again: move it at the version just read,
 * or nothing when it changed status since the board showed it. */
export type BoardMove = { kind: "move"; id: string; version: number; status: TicketStatus } | { kind: "stale" };

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? trackerRefusal({ code: failure.code, cause: failure.cause }) : "";
}

function ticketNumber(id: string): number {
  return Number(id.slice(id.lastIndexOf("-") + 1));
}

/** The board's columns: the five open statuses in order, then Closed. Each column lists its
 * tickets most urgent first, then by number. Input: the rows. Output: the columns. */
export function boardColumns(rows: readonly TicketRow[]): BoardColumn[] {
  const sorted = [...rows].sort((a, b) => a.priority - b.priority || ticketNumber(a.id) - ticketNumber(b.id));
  const open = OPEN.map((status) => ({ status, label: statusLabel(status), rows: sorted.filter((row) => row.status === status), drop: true }));
  return [...open, { status: "closed", label: "Closed", rows: sorted.filter((row) => isClosed(row.status)), drop: false }];
}

/** Tells whether a ticket can be dropped on a column: an open ticket onto another open
 * status. Inputs: the ticket and the column. Output: the answer. */
export function canDrop(row: TicketRow, column: BoardColumn): boolean {
  return column.drop && column.status !== row.status && !isClosed(row.status);
}

/** Decides a board drop from a fresh read of the ticket: the move at the version just read,
 * or stale when its status is no longer the one the board showed. Inputs: the row the owner
 * dragged, the fresh fields and the column's status. Output: the decision. */
export function boardMove(seen: TicketRow, fresh: TicketFields, status: TicketStatus): BoardMove {
  if (fresh.status !== seen.status) return { kind: "stale" };
  return { kind: "move", id: seen.id, version: fresh.version, status };
}

/** The status of a row: a badge while it waits in Triage, the status word otherwise. */
function RowStatus({ status }: { status: string }) {
  const badge = triageBadge(status);
  return badge ? <Badge word={badge.word} tone={badge.tone} /> : <span className="tk-status pj-small">{statusLabel(status)}</span>;
}

/** The fields of one listed ticket, as a button that opens it. */
function RowButton({ row, onOpen, children }: { row: TicketRow; onOpen: (id: string) => void; children?: ReactNode }) {
  return <button type="button" className="tk-row" onClick={() => onOpen(row.id)}>
    <span className="tk-id pj-small">{row.id}</span>
    <span className="tk-title">{displayText(row.title)}</span>
    <RowStatus status={row.status} />
    <span className="pj-small pj-secondary">{priorityLabel(row.priority)}</span>
    {row.assignee && <span className="pj-small pj-secondary">{displayText(row.assignee)}</span>}
    {children}
  </button>;
}

/** The ticket list: one row per ticket with its id, title, status, priority and assignee.
 * Inputs: the rows and the handler that opens one. Output: the list. */
export function TicketList({ rows, onOpen }: { rows: readonly TicketRow[]; onOpen: (id: string) => void }) {
  return <ul className="tk-list" aria-label="Tickets">
    {rows.map((row) => <li key={row.id}><RowButton row={row} onOpen={onOpen} /></li>)}
  </ul>;
}

/** The owner's ready queue: open tickets nothing blocks, most urgent first, with their cycle
 * and release. Inputs: the rows and the handler that opens one. Output: the list. */
export function ReadyList({ rows, onOpen }: { rows: readonly ReadyRow[]; onOpen: (id: string) => void }) {
  return <ol className="tk-list" aria-label="Ready tickets">
    {rows.map((row) => <li key={row.id}><RowButton row={row} onOpen={onOpen}>
      {row.cycle !== null && <span className="pj-small pj-secondary">{`Cycle ${row.cycle}`}</span>}
      {row.release !== null && <span className="pj-small pj-secondary">{`Release ${displayText(row.release)}`}</span>}
    </RowButton></li>)}
  </ol>;
}

/** The board: a column per open status and Closed. An open ticket drags onto another open
 * status, which moves it as a versioned write; the detail view moves it from the keyboard.
 * Inputs: the columns, whether a move runs, and the move and open handlers. Output: the
 * board. */
export function TicketBoard({ columns, busy, onMove, onOpen }: {
  columns: readonly BoardColumn[];
  busy: boolean;
  onMove: (row: TicketRow, status: TicketStatus) => void;
  onOpen: (id: string) => void;
}) {
  const [dragged, setDragged] = useState<TicketRow | null>(null);
  return <div className="tk-board" aria-label="Board">
    {columns.map((column) => <section key={column.status} className={`tk-column${dragged && canDrop(dragged, column) ? " droppable" : ""}`}
      aria-label={column.label}
      onDragOver={(event) => { if (dragged && canDrop(dragged, column)) event.preventDefault(); }}
      onDrop={(event) => {
        event.preventDefault();
        const row = dragged;
        setDragged(null);
        if (row && canDrop(row, column) && column.status !== "closed") onMove(row, column.status);
      }}>
      <h4 className="tk-column-head">{`${column.label} (${column.rows.length})`}</h4>
      {column.rows.map((row) => <article key={row.id} className="tk-card" draggable={!busy && !isClosed(row.status)}
        onDragStart={(event) => { event.dataTransfer.setData("text/plain", row.id); setDragged(row); }}
        onDragEnd={() => setDragged(null)}>
        <RowButton row={row} onOpen={onOpen} />
      </article>)}
    </section>)}
  </div>;
}

/** The pages of a project's tickets the owner opened, read again from the first. Inputs: the
 * key, the filters and how many pages. Output: the rows and the cursor after the last page. */
async function readPages(key: string, filters: ListFilters, count: number): Promise<Result<{ tickets: TicketRow[]; cursor: string | null }>> {
  const tickets: TicketRow[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < count; page += 1) {
    const result: Result<{ tickets: TicketRow[]; cursor: string | null }> = await loadTickets(key, filters, cursor);
    if (result.kind !== "ok") return result;
    tickets.push(...result.value.tickets);
    cursor = result.value.cursor;
    if (cursor === null) break;
  }
  return { kind: "ok", value: { tickets, cursor } };
}

/** Turns tickets on for a project: the key, suggested from its name, and the button.
 * Inputs: the project data and the handler run once attached. Output: the form. */
function AttachTickets({ data, onAttached }: { data: ProjectData; onAttached: () => void }) {
  const [key, setKey] = useState(() => suggestKey(data.name));
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const field = useId();
  const problem = keyProblem(key);
  async function attach() {
    setBusy(true);
    const result = await attachProject(data.id, key, data.name);
    setBusy(false);
    if (result.kind === "ok") {
      data.notify(`Tickets are on. New tickets are numbered ${result.value.key}-1, ${result.value.key}-2 and on.`);
      onAttached();
    } else setNote(result.kind === "not_ready" ? "Tickets aren't ready in this engine yet." : refusalOf(result));
  }
  return <div className="pj-stack-s">
    <ProjectEmpty title="Tickets aren't on yet">
      Turn them on to track this project's work as tickets with acceptance criteria. Agents read and update them; you decide what closes.
    </ProjectEmpty>
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
    <TextField id={field} label="Ticket key" value={key} maxLength={8} onValue={(value) => setKey(value.toUpperCase())}
      helper="Ticket ids start with it, like RB-12." error={problem ?? undefined} />
    <span><Button variant="primary" disabled={busy || problem !== null} pending={busy} pendingLabel="Turning on..." onClick={() => void attach()}>Turn on tickets</Button></span>
  </div>;
}

/** The list filters: status, kind and assignee. Inputs: the filters and the change handler.
 * Output: the filter row. */
function Filters({ filters, onFilters }: { filters: ListFilters; onFilters: (filters: ListFilters) => void }) {
  const status = useId();
  const kind = useId();
  const assignee = useId();
  return <div className="pj-row tk-filters">
    <Select id={status} label="Status" value={filters.status} onValue={(value) => onFilters({ ...filters, status: value as ListFilters["status"] })}
      options={[{ value: "", label: "Any status" }, ...STATUSES.map((value) => ({ value, label: statusLabel(value) }))]} />
    <Select id={kind} label="Kind" value={filters.kind} onValue={(value) => onFilters({ ...filters, kind: value as ListFilters["kind"] })}
      options={[{ value: "", label: "Any kind" }, ...KINDS.map((value) => ({ value, label: kindLabel(value) }))]} />
    <TextField id={assignee} label="Assignee" value={filters.assignee} maxLength={128} placeholder="owner or agent:team-2"
      onValue={(value) => onFilters({ ...filters, assignee: value })} />
    {filtersActive(filters) && <span><Button variant="quiet" onClick={() => onFilters(NO_FILTERS)}>Clear filters</Button></span>}
  </div>;
}

/** The open tickets of one tracked project: the list with filters, the ready queue and the
 * board, rereading every few seconds; a ticket opens in the detail view. Inputs: the project
 * data and its tracked project. Output: the views. */
function TrackedTickets({ data, tracked }: { data: ProjectData; tracked: TrackedProject }) {
  const [view, setView] = useState<View>("list");
  const [filters, setFilters] = useState<ListFilters>(NO_FILTERS);
  const [pages, setPages] = useState(1);
  const [rows, setRows] = useState<Result<{ tickets: TicketRow[]; cursor: string | null }>>();
  const [ready, setReady] = useState<Result<{ tickets: ReadyRow[]; cursor: string | null }>>();
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => {
    if (open !== null) return;
    let current = true;
    if (view === "ready") void loadReady(tracked.key, null).then((result) => { if (current) setReady(result); });
    else void readPages(tracked.key, filters, Math.min(pages, LIST_PAGES_MAX)).then((result) => { if (current) setRows(result); });
    const timer = setTimeout(() => setRound((value) => value + 1), LIST_POLL_MS);
    return () => { current = false; clearTimeout(timer); };
  }, [tracked.key, view, filters, pages, round, open]);

  async function move(row: TicketRow, status: TicketStatus) {
    setBusy(true);
    setNote("");
    const read = opened<ShowPage>(await trackerOp("show", { id: row.id }));
    if (read.kind !== "ok") {
      setBusy(false);
      setNote(refusalOf(read));
      return setRound((value) => value + 1);
    }
    const decided = boardMove(row, read.value.data.ticket as TicketFields, status);
    const result = decided.kind === "move" ? await moveTicket(decided.id, decided.version, decided.status) : null;
    setBusy(false);
    if (result === null) setNote(STALE_LINE);
    else if (result.kind === "ok") data.notify(`${row.id} moved to ${statusLabel(status)}.`);
    else setNote(refusalOf(result));
    setRound((value) => value + 1);
  }

  if (open !== null) return <TicketDetail key={open} data={data} id={open} onBack={() => { setOpen(null); setRound((value) => value + 1); }} />;
  const shown = view === "ready" ? ready : rows;
  return <div className="pj-stack">
    <div className="pj-row between">
      <Segmented<View> label="Tickets view" value={view} onValue={setView}
        options={[{ value: "list", label: "List" }, { value: "ready", label: "Ready" }, { value: "board", label: "Board" }]} />
      <span className="pj-small pj-secondary">{`${displayText(tracked.name)} tickets, key ${tracked.key}`}</span>
    </div>
    {view !== "ready" && <Filters filters={filters} onFilters={(next) => { setFilters(next); setPages(1); }} />}
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
    {shown === undefined && <p className="pj-muted pj-small" role="status">Loading tickets...</p>}
    {shown !== undefined && notReady(shown) && <NotReady what={view === "ready" ? "the ready queue isn't in this engine yet." : "tickets aren't in this engine yet."} />}
    {shown?.kind === "error" && !notReady(shown) && <p className="pj-error" role="alert">{`Couldn't load tickets. ${displayText(refusalOf(shown))}`}</p>}
    {view === "ready" && ready?.kind === "ok" && (ready.value.tickets.length === 0
      ? <ProjectEmpty title="Nothing ready">Open tickets that nothing blocks show here, the most urgent first.</ProjectEmpty>
      : <ReadyList rows={ready.value.tickets} onOpen={setOpen} />)}
    {view !== "ready" && rows?.kind === "ok" && rows.value.tickets.length === 0 && <ProjectEmpty title={filtersActive(filters) ? "No tickets match" : "No tickets yet"}>
      {filtersActive(filters) ? "Clear the filters to see every ticket." : "Tickets you, the master or an agent create show here."}
    </ProjectEmpty>}
    {view === "list" && rows?.kind === "ok" && rows.value.tickets.length > 0 && <TicketList rows={rows.value.tickets} onOpen={setOpen} />}
    {view === "board" && rows?.kind === "ok" && rows.value.tickets.length > 0 && <TicketBoard columns={boardColumns(rows.value.tickets)} busy={busy}
      onMove={(row, status) => void move(row, status)} onOpen={setOpen} />}
    {view !== "ready" && rows?.kind === "ok" && rows.value.cursor !== null && (pages < LIST_PAGES_MAX
      ? <span><Button disabled={busy} onClick={() => setPages((value) => value + 1)}>Show more tickets</Button></span>
      : <p className="pj-small pj-secondary">Showing the first tickets only. Filter the list to find the rest.</p>)}
  </div>;
}

/** Tickets tab: the project's tickets as a list, the ready queue and a board, with a detail
 * view; a project without tickets offers to turn them on. Input: the project data. Output:
 * the tab. */
export function TicketsTab({ data }: { data: ProjectData }) {
  const [projects, setProjects] = useState<Result<TrackedProject[]>>();
  const [round, setRound] = useState(0);
  useEffect(() => {
    let current = true;
    void trackerProjects().then((result) => { if (current) setProjects(result); });
    return () => { current = false; };
  }, [data.id, round]);
  if (projects === undefined) return <p className="pj-muted pj-small" role="status">Loading tickets...</p>;
  if (notReady(projects)) return <NotReady what="tickets aren't in this engine yet." />;
  if (projects.kind !== "ok") return <p className="pj-error" role="alert">{`Couldn't load tickets. ${displayText(refusalOf(projects))}`}</p>;
  const tracked = trackedProject(projects.value, data.id);
  if (tracked === null) return <AttachTickets data={data} onAttached={() => setRound((value) => value + 1)} />;
  return <TrackedTickets key={tracked.key} data={data} tracked={tracked} />;
}
