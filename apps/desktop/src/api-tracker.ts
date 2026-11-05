import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import { displayText } from "./screens/conversation";

/** Every operation trackwright's schema v1 names, the eight reads first. */
export const OP_NAMES = [
  "ready", "list", "show", "history", "roadmap", "policy_get", "export", "schema",
  "create", "edit", "move", "claim", "release", "depend", "undepend", "criterion_add", "criterion_edit", "check_add",
  "evidence_record", "criterion_set", "link", "unlink", "close", "reopen", "version_upsert", "cycle_start", "cycle_close",
  "import_preview", "import_apply", "policy_set",
] as const;

export type TrackerOpName = typeof OP_NAMES[number];

const READ_OPS: readonly TrackerOpName[] = ["ready", "list", "show", "history", "roadmap", "policy_get", "export", "schema"];

/** The schema v1 operations that write; each carries a request id. */
export const WRITE_OPS: readonly TrackerOpName[] = OP_NAMES.filter((name) => !READ_OPS.includes(name));

export const STATUSES = ["triage", "backlog", "todo", "in_progress", "in_review", "done", "cancelled"] as const;
export type TicketStatus = typeof STATUSES[number];
export const KINDS = ["feature", "bug", "chore", "spike"] as const;
export type TicketKind = typeof KINDS[number];

const STATUS_LABELS: Record<TicketStatus, string> = {
  triage: "Triage", backlog: "Backlog", todo: "To do", in_progress: "In progress", in_review: "In review", done: "Done", cancelled: "Cancelled",
};
const KIND_LABELS: Record<TicketKind, string> = { feature: "Feature", bug: "Bug", chore: "Chore", spike: "Spike" };
const PRIORITY_LABELS = ["Urgent", "High", "Normal", "Low", "Lowest"];
const LINK_KINDS: Record<string, string> = {
  agent: "Agent", task: "Task", worktree: "Worktree", branch: "Branch", change_set: "Change set", memory: "Memory", question: "Question",
};
const CHANGE_SET_ID = /^[0-9a-f]{32}$/;
const KEY = /^[A-Z]{2,8}$/;

export const SHOW_PAGES_MAX = 16;
export const HISTORY_PAGES_MAX = 20;

/** One next action a tracker reply proposes. */
export interface TrackerNext { op: string; id?: string; why: string }

/** The core's reply envelope for one operation: its data and cursor, or the refusal code
 * and message; `next` names what to do after. */
export type TrackerEnvelope =
  | { api: 1; ok: true; data: unknown; next: TrackerNext[]; cursor: string | null }
  | { api: 1; ok: false; code: string; message: string; next: TrackerNext[]; data?: unknown };

/** A CrazyAgent project that has tickets: the engine project, its tracker key and name. */
export interface TrackedProject { project: string; key: string; name: string }

/** One row of a ticket list page. */
export interface TicketRow { id: string; title: string; elided?: boolean; status: TicketStatus; priority: number; assignee: string | null }

/** One row of the ready queue: a list row with its cycle and release. */
export interface ReadyRow extends TicketRow { cycle: number | null; release: string | null }

/** A ticket's own fields as the tracker stores them; `version` goes back with a write. */
export interface TicketFields {
  id: string; version: number; title: string; body_bytes: number; kind: TicketKind; status: TicketStatus; priority: number;
  assignee: string | null; created_by: string; created_ms: number; updated_ms: number; closed_ms: number | null;
  closed_by: string | null; close_reason: string | null;
}

/** The latest evidence recorded for one check. */
export interface CheckEvidence { result: string; evidence: string; by: string; at_ms: number }

/** One check of an acceptance criterion and its latest evidence. */
export interface TicketCheck { kind: string; ref: string; ref_elided: boolean; latest: CheckEvidence | null }

/** One acceptance criterion with its checks. */
export interface TicketCriterion { position: number; text: string; state: string; version: number; checks: TicketCheck[] }

/** One link as the host sees its target now: live or gone, with the host's label and state. */
export interface TicketLink { kind: string; target: string; live: boolean; label: string; state: string }

/** One recorded change: when, who, which operation and the fields it changed. */
export interface TicketEvent { at_ms: number; actor: string; op: string; fields: string[] }

/** One page of a ticket's `show` reply; later pages carry only id, version and size. */
export interface ShowPage {
  ticket: Partial<TicketFields> & { id: string; version: number; body_bytes: number };
  body_at?: number;
  body?: string;
  criteria?: TicketCriterion[];
  links?: TicketLink[];
  events?: TicketEvent[];
}

/** A whole ticket read from its `show` pages; `more` is true when pages were left unread. */
export interface TicketDetail { ticket: TicketFields; body: string; criteria: TicketCriterion[]; links: TicketLink[]; events: TicketEvent[]; more: boolean }

/** The list filters the owner chose; an empty value is no filter. */
export interface ListFilters { status: TicketStatus | ""; kind: TicketKind | ""; assignee: string }

/** What the owner reads when a ticket changed since it was shown. */
export const STALE_LINE = "This ticket changed. Review it again.";

const REFUSALS: Record<string, string> = {
  stale_version: "This ticket changed. Review it again.",
  active_plan_exists: "That release already has an active test plan.",
  actor_invalid: "CrazyAgent couldn't tell who made this change. Try again.",
  actor_not_engine: "Only CrazyAgent itself can record that.",
  actor_other_project: "That agent works on another project, so it can't change these tickets.",
  agent_edit_refused: "Agents can't change that part of a ticket.",
  already_claimed: "Someone else is already working on this ticket. Pick other ready work.",
  args_invalid: "The tickets engine couldn't read this request. Update the app, then try again.",
  case_link_exists: "That test case is already linked to this criterion.",
  case_link_unknown: "That test case isn't linked to this criterion any more. Review it again.",
  case_links_full: "This criterion has as many linked test cases as it can hold.",
  case_protected: "Only you can change protected test cases or the plans that hold them.",
  case_unknown: "That test case is no longer there. The list shows what is left.",
  cases_full: "This project has as many test cases as it can hold.",
  changes_unavailable: "The engine's records aren't available right now. Try again in a moment.",
  check_exists: "That criterion already has this check.",
  check_unknown: "That check isn't on this criterion any more. Review the ticket again.",
  checks_full: "This criterion has as many checks as it can hold.",
  close_needs_evidence: "This ticket can't close until its criteria have passing evidence.",
  close_owner_only: "Only you can close this ticket.",
  close_reason_open: "A closing reason is kept only for a closed ticket.",
  create_owner_only: "Only you can create tickets in this project.",
  criteria_full: "This ticket has as many criteria as it can hold.",
  criterion_has_checks: "Remove this criterion's checks first.",
  criterion_needs_check: "Add a check to this criterion first.",
  criterion_unknown: "That criterion isn't on this ticket any more. Review it again.",
  cross_project: "That belongs to another project, so it can't be used here.",
  cursor_invalid: "The list changed while it was read. Reload it.",
  cycle_closed: "That cycle is closed.",
  cycle_days_invalid: "A cycle runs for a whole number of days within the allowed range.",
  cycle_overlaps: "A new cycle has to start after the last one.",
  cycle_unknown: "That cycle is no longer there. The list shows what is left.",
  cycles_full: "This project has as many cycles as it can hold.",
  dependencies_full: "This ticket has as many dependencies as it can hold.",
  dependency_cycle: "That dependency would make these tickets wait on each other.",
  dependency_exists: "This ticket already depends on that one.",
  dependency_invalid: "A ticket can depend only on another ticket of this project.",
  dependency_unknown: "This ticket doesn't depend on that one any more. Review it again.",
  evidence_engine_only: "Only CrazyAgent records evidence for this check.",
  evidence_not_for_agents: "Agents can't record evidence for this check.",
  evidence_owner_only: "Only you can record evidence for this check.",
  export_path_refused: "Tickets can't be exported to that place.",
  export_too_large: "These tickets are too large to export at once.",
  field_invalid: "One of the values isn't allowed. Check it, then try again.",
  host_failed: "CrazyAgent couldn't check what this ticket links to. Try again in a moment.",
  import_format_unknown: "CrazyAgent can't read that file as tickets.",
  import_item_needs_version: "An imported ticket that already exists needs its version.",
  import_item_unknown: "An imported ticket names one that isn't in this project.",
  import_keys_collide: "The file names tickets of another project key.",
  import_not_text: "That file isn't text.",
  import_path_refused: "Tickets can't be imported from that place.",
  import_too_large: "That file is too large to import.",
  link_broken: "That link points at something that is gone.",
  link_cross_project: "That belongs to another project, so it can't be linked here.",
  link_exists: "This ticket already has that link.",
  link_kind_unknown: "CrazyAgent can't link that kind of thing.",
  link_unknown: "That link isn't on this ticket any more. Review it again.",
  links_full: "This ticket has as many links as it can hold.",
  missing_field: "A required value is missing. Fill it in, then try again.",
  not_claimant: "This ticket isn't claimed, or someone else claimed it. Review it again.",
  notify_too_wide: "That notice would reach too many people at once.",
  op_unavailable: "This part of tickets isn't in this engine yet.",
  open_tickets_full: "This project has as many open tickets as it can hold. Close some first.",
  option_needs_host: "That option needs each ticket's source, which CrazyAgent doesn't track here.",
  owner_authentication_failed: "CrazyAgent couldn't confirm this app with the engine. Restart the app, then try again.",
  owner_authority_unavailable: "The engine isn't ready for owner actions yet. Try again in a moment.",
  owner_busy: "The engine is busy with other owner requests. Try again in a moment.",
  owner_grant_stale_or_revoked: "This app's access to the engine changed. Reconnect, then try again.",
  owner_only: "Only you can do that.",
  owner_request_failed: "The engine couldn't take this request. Try again.",
  owner_root_replaced_or_unsafe: "The engine's data folder changed or isn't safe. Restart the engine, then try again.",
  owner_root_unavailable: "The engine's data folder isn't available. Restart the engine, then try again.",
  owner_socket_replaced: "The engine was restarted. Reconnect, then try again.",
  owner_socket_unsafe: "The engine connection isn't safe to use. Restart the engine, then try again.",
  parent_invalid: "That parent isn't a ticket of this project.",
  peer_owner_mismatch: "The engine runs as another user. Start it as yourself, then try again.",
  plan_closed: "That test plan is closed.",
  plan_item_exists: "That test plan already holds it.",
  plan_item_unknown: "That test plan doesn't hold it any more. Review the plan again.",
  plan_items_full: "This test plan holds as much as it can.",
  plan_unknown: "That test plan is no longer there. The list shows what is left.",
  plans_full: "This project has as many test plans as it can hold.",
  policy_in_settings: "That policy is set in the ticket settings.",
  policy_locked: "CrazyAgent keeps that setting at one value.",
  policy_value_invalid: "That policy value isn't allowed.",
  priority_invalid: "A priority runs from 0, the most urgent, to 4.",
  project_exists: "This project already has tickets.",
  project_key_invalid: "Use 2 to 8 capital letters for the key, like RB.",
  project_not_attached: "Tickets aren't on for this project yet.",
  project_unknown: "This project has no tickets yet.",
  rate_limited: "Too many ticket changes at once. Try again in a moment.",
  release_unknown: "That release is no longer there. The list shows what is left.",
  request_id_reused: "CrazyAgent sent two different changes under one key. Try again.",
  request_too_large: "That change is too large to send.",
  result_name_repeated: "Each result name appears once per test run.",
  results_empty: "Add at least one result first.",
  results_too_many: "That's too many results to record at once.",
  run_batch_invalid: "A batch name is 1 to 64 letters, digits, underscores, dots, dashes or colons.",
  run_closed: "This test run is finished and takes no more results.",
  run_finish_invalid: "This test run can't finish like that. Review its results, then try again.",
  run_integrity_invalid: "CrazyAgent couldn't read this run's integrity details.",
  run_policy_invalid: "CrazyAgent couldn't read this run's policy.",
  run_results_full: "This test run has as many results as it can hold.",
  run_source_invalid: "CrazyAgent can't record that source. A commit or tree is 40 lowercase hex digits.",
  run_unknown: "That test run is no longer there. The list shows what is left.",
  run_writer_refused: "Only CrazyAgent records automated runs, and only you record manual runs.",
  runs_full: "This project has used every test run number.",
  set_case_exists: "That test set already has this case.",
  set_case_unknown: "That test case isn't in this set any more. Review it again.",
  set_cases_full: "This test set holds as many cases as it can.",
  set_unknown: "That test set is no longer there. The list shows what is left.",
  sets_full: "This project has as many test sets as it can hold.",
  short_id_invalid: "That isn't a ticket id like RB-12.",
  stable_name_taken: "Another test case in this project already has that stable name.",
  status_closed_at_create: "A new ticket can't start closed.",
  status_invalid: "That status isn't one tickets use.",
  steps_full: "This test case has as many steps as it can hold.",
  store_failed: "CrazyAgent couldn't read or save tickets. Try again in a moment.",
  store_not_private: "The tickets store isn't private to you, so CrazyAgent won't use it.",
  store_path_refused: "The tickets store isn't in a place CrazyAgent uses.",
  store_schema_newer: "These tickets were saved by a newer CrazyAgent. Update the app.",
  test_id_invalid: "That isn't a test case, set, plan or run id.",
  text_empty: "Write some text first.",
  text_hidden_characters: "The text has hidden characters CrazyAgent can't keep. Remove them, then try again.",
  text_line_break: "This text must fit on one line.",
  text_too_long: "That text is too long.",
  ticket_closed: "This ticket is closed. Reopen it first.",
  ticket_open: "This ticket is still open.",
  ticket_unknown: "That ticket is no longer there. The list shows what is left.",
  tickets_full: "This project has as many tickets as it can hold.",
  tracker_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the ticket before you try again.",
  tracker_project_missing: "This project is no longer in CrazyAgent.",
  tracker_projects_full: "Tickets are already on for as many projects as CrazyAgent can hold.",
  tracker_reply_bounds: "The engine's answer was too large to show. Narrow the list, then try again.",
  tracker_request_bounds: "That request is too large to send.",
  tracker_request_invalid: "The engine couldn't read this request. Update the app, then try again.",
  tracker_unavailable: "Tickets aren't ready in this engine yet.",
  unknown_field: "CrazyAgent sent a value tickets don't take. Update the app, then try again.",
  unknown_op: "Tickets can't do that in this engine. Update the app, then try again.",
  use_close: "Close the ticket to mark it done or cancelled.",
  version_unknown: "That release isn't in this project.",
  versioned_item_invalid: "CrazyAgent couldn't record a change to this ticket. Review it again.",
  versioned_unavailable: "CrazyAgent couldn't record this right now. Try again in a moment.",
  versions_full: "This project has as many releases as it can hold.",
  changes_not_found: "That change set is not there any more.",
  ide_focus_desktop_only: "Only the desktop app can bring the IDE forward.",
  ide_not_running: "The IDE isn't open for this project. Open the IDE tab, then choose Open in IDE again.",
  ide_outcome_unknown: "CrazyAgent couldn't confirm the IDE showed it. Check the IDE tab.",
  ide_path_unreadable: "CrazyAgent couldn't read that folder.",
  ide_project_folder_invalid: "This project's folder can't open in the IDE.",
  ide_project_unknown: "This project is no longer in CrazyAgent.",
  ide_reply_bounds: "The IDE's answer was too large. Try again.",
  ide_request_bounds: "That request is too large for the IDE.",
  ide_request_invalid: "The IDE couldn't read this request. Update the app, then try again.",
  ide_reveal_busy: "The IDE is still showing earlier files. Try again.",
  ide_reveal_invalid: "CrazyAgent opens only files inside the project's folders.",
  ide_reveal_missing: "That change set's folder is not there any more.",
  ide_reveal_not_listed: "That agent's folder isn't in the IDE. Show all agents in the IDE, then try again.",
  ide_reveal_outside: "CrazyAgent opens only files inside the project's folders.",
  ide_reveal_unanswered: "The IDE didn't show it. Check the IDE tab, then try again.",
  ide_reveal_unavailable: "CrazyAgent couldn't open it in the IDE right now. Try again.",
  ide_unavailable: "The IDE isn't ready in this engine yet.",
  ide_workspace_unavailable: "The IDE's workspace isn't ready yet. Try again in a moment.",
};

/** The tracker and Open in IDE codes that have their own sentence. */
export const TRACKER_REFUSALS = Object.keys(REFUSALS);

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** One plain sentence for a refused tracker or Open in IDE request. Inputs: the code and the
 * cause behind it. Output: the sentence, or the plain fallback naming the code. */
export function trackerRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** Tells whether a tracker answer means the view is not ready: this build has no tracker
 * commands, or the engine has no tracker or not this operation yet. Input: the answer. */
export function notReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && (result.code === "tracker_unavailable" || result.code === "op_unavailable"));
}

/** A fresh request id for one write, unique per call and within the schema's pattern.
 * Output: the id. */
export function requestId(): string {
  const id = `desk-${crypto.randomUUID()}`;
  assert(/^[A-Za-z0-9._:-]{1,64}$/.test(id), "a request id fits the schema pattern");
  return id;
}

/** The input a write sends: its own request id added unless the caller kept one for a
 * retry; a read is sent as given. Inputs: the operation, its input and a fresh id. Output:
 * the input to send. */
export function withRequestId(op: TrackerOpName, args: Record<string, unknown>, id: string): Record<string, unknown> {
  if (!WRITE_OPS.includes(op) || Object.hasOwn(args, "request_id")) return { ...args };
  return { ...args, request_id: id };
}

/** Runs one schema v1 operation as the owner. Inputs: the operation and its input object as
 * the schema names its fields. Output: the core's envelope, or a bridge failure. */
export function trackerOp(op: TrackerOpName, args: Record<string, unknown>): Promise<Result<TrackerEnvelope>> {
  return call<TrackerEnvelope>("desktop_tracker_op", { op, args: withRequestId(op, args, requestId()) });
}

/** Lists the projects that have tickets. Output: each with its tracker key and name. */
export function trackerProjects(): Promise<Result<TrackedProject[]>> {
  return call<TrackedProject[]>("desktop_tracker_projects", {});
}

/** Gives a project its tickets under a key the owner chose. Inputs: the project id, the key
 * and the name. Output: the tracked project, or the refusal. */
export function attachProject(projectId: string, key: string, name: string): Promise<Result<TrackedProject>> {
  return call<TrackedProject>("desktop_tracker_attach", { projectId, key, name });
}

/** Shows a linked change set in the project's open IDE. Inputs: the project id and the change
 * set id. Output: ok once the IDE showed it, or the refusal. */
export function openChangeSet(projectId: string, changeSet: string): Promise<Result<void>> {
  return call<void>("desktop_tracker_open_change_set", { projectId, changeSet });
}

/** Opens an envelope: its data and cursor, or a refused envelope as a failure whose message
 * is the owner's sentence and whose cause is the core's own message. Input: the answer.
 * Output: the data or the failure. */
export function opened<T>(result: Result<TrackerEnvelope>): Result<{ data: T; cursor: string | null }> {
  if (result.kind !== "ok") return result;
  const envelope = result.value;
  if (envelope.ok) return { kind: "ok", value: { data: envelope.data as T, cursor: envelope.cursor } };
  return { kind: "error", code: envelope.code, message: trackerRefusal({ code: envelope.code }), cause: envelope.message };
}

/** The `list` input for one page: the project, each filter the owner chose and the cursor.
 * Inputs: the key, the filters and the cursor or null. Output: the input. */
export function listArgs(key: string, filters: ListFilters, cursor: string | null): Record<string, unknown> {
  const args: Record<string, unknown> = { project: key };
  if (filters.status) args.status = filters.status;
  if (filters.kind) args.kind = filters.kind;
  const assignee = filters.assignee.trim();
  if (assignee) args.assignee = assignee;
  if (cursor !== null) args.cursor = cursor;
  return args;
}

/** Tells whether any list filter is set. Input: the filters. Output: true when one is. */
export function filtersActive(filters: ListFilters): boolean {
  return Boolean(filters.status || filters.kind || filters.assignee.trim());
}

/** Reads one page of a project's tickets. Inputs: the key, the filters and the cursor or
 * null. Output: the rows and the next page's cursor. */
export async function loadTickets(key: string, filters: ListFilters, cursor: string | null): Promise<Result<{ tickets: TicketRow[]; cursor: string | null }>> {
  const result = opened<{ tickets: TicketRow[] }>(await trackerOp("list", listArgs(key, filters, cursor)));
  return result.kind === "ok" ? { kind: "ok", value: { tickets: result.value.data.tickets, cursor: result.value.cursor } } : result;
}

/** Reads one page of the owner's ready queue, the most urgent first. Inputs: the key and the
 * cursor or null. Output: the rows and the next page's cursor. */
export async function loadReady(key: string, cursor: string | null): Promise<Result<{ tickets: ReadyRow[]; cursor: string | null }>> {
  const result = opened<{ tickets: ReadyRow[] }>(await trackerOp("ready", cursor === null ? { project: key } : { project: key, cursor }));
  return result.kind === "ok" ? { kind: "ok", value: { tickets: result.value.data.tickets, cursor: result.value.cursor } } : result;
}

/** Merges a ticket's `show` pages: the first page's fields, the body parts in order, then
 * criteria, links and recent events. Inputs: the pages, at least one, and whether pages were
 * left unread. Output: the ticket. */
export function mergeShow(pages: readonly ShowPage[], more = false): TicketDetail {
  assert(pages.length > 0, "a ticket is read from at least one page");
  return {
    ticket: pages[0].ticket as TicketFields,
    body: pages.map((page) => page.body ?? "").join(""),
    criteria: pages.flatMap((page) => page.criteria ?? []),
    links: pages.flatMap((page) => page.links ?? []),
    events: pages.flatMap((page) => page.events ?? []),
    more,
  };
}

/** Reads a whole ticket, following its cursor up to `SHOW_PAGES_MAX` pages. Input: the
 * ticket id. Output: the ticket. */
export async function loadTicket(id: string): Promise<Result<TicketDetail>> {
  const pages: ShowPage[] = [];
  let cursor: string | null = null;
  do {
    const result: Result<{ data: ShowPage; cursor: string | null }> = opened<ShowPage>(await trackerOp("show", cursor === null ? { id } : { id, cursor }));
    if (result.kind !== "ok") return result;
    pages.push(result.value.data);
    cursor = result.value.cursor;
  } while (cursor !== null && pages.length < SHOW_PAGES_MAX);
  return { kind: "ok", value: mergeShow(pages, cursor !== null) };
}

/** Reads a ticket's whole history, oldest first, up to `HISTORY_PAGES_MAX` pages. Input: the
 * ticket id. Output: the pages and whether pages were left unread. */
export async function loadHistory(id: string): Promise<Result<{ pages: TicketEvent[][]; more: boolean }>> {
  const pages: TicketEvent[][] = [];
  let cursor: string | null = null;
  do {
    const result: Result<{ data: { events: TicketEvent[] }; cursor: string | null }> = opened<{ events: TicketEvent[] }>(await trackerOp("history", cursor === null ? { id } : { id, cursor }));
    if (result.kind !== "ok") return result;
    pages.push(result.value.data.events);
    cursor = result.value.cursor;
  } while (cursor !== null && pages.length < HISTORY_PAGES_MAX);
  return { kind: "ok", value: { pages, more: cursor !== null } };
}

/** Moves a ticket to an open status at the version the owner read. Inputs: the id, the
 * version and the status. Output: the ticket's new fields, or `stale_version`. */
export async function moveTicket(id: string, version: number, status: TicketStatus): Promise<Result<TicketFields>> {
  const result = opened<TicketFields>(await trackerOp("move", { id, version, status }));
  return result.kind === "ok" ? { kind: "ok", value: result.value.data } : result;
}

/** A ticket's events newest first: the recent events `show` sent, already newest first, or
 * the full history, whose pages run oldest first. Inputs: the recent events and the history
 * pages, or null until read. Output: a new list. */
export function newestFirst(recent: readonly TicketEvent[], history: readonly (readonly TicketEvent[])[] | null): TicketEvent[] {
  return history === null ? [...recent] : history.flat().reverse();
}

/** Tells whether a status is closed. Input: the status. Output: true for done or cancelled. */
export function isClosed(status: string): boolean {
  return status === "done" || status === "cancelled";
}

/** The plain word for a status. Input: the status. Output: the word. */
export function statusLabel(status: string): string {
  return Object.hasOwn(STATUS_LABELS, status) ? STATUS_LABELS[status as TicketStatus] : displayText(status);
}

/** The plain word for a kind. Input: the kind. Output: the word. */
export function kindLabel(kind: string): string {
  return Object.hasOwn(KIND_LABELS, kind) ? KIND_LABELS[kind as TicketKind] : displayText(kind);
}

/** The plain word for a priority, 0 the most urgent. Input: the priority. Output: the word. */
export function priorityLabel(priority: number): string {
  return PRIORITY_LABELS[priority] ?? `Priority ${priority}`;
}

/** The badge a ticket waiting in Triage carries. Input: the status. Output: the badge, or
 * null for any other status. */
export function triageBadge(status: string): { word: "Triage"; tone: "warn" } | null {
  return status === "triage" ? { word: "Triage", tone: "warn" } : null;
}

/** How one link shows: its kind, a title from the host's label or the target, the host's
 * state or Gone, and the change set id that Open in IDE shows, only for a live change set.
 * Input: the link. Output: the view; host text goes through `displayText`. */
export function linkView(link: TicketLink): { kind: string; title: string; state: string; live: boolean; changeSet: string | null } {
  const kind = Object.hasOwn(LINK_KINDS, link.kind) ? LINK_KINDS[link.kind] : displayText(link.kind);
  const title = link.live && link.label && link.label !== kind ? displayText(link.label) : `${kind} ${displayText(link.target)}`;
  return {
    kind,
    title,
    state: link.live ? displayText(link.state) : "Gone",
    live: link.live,
    changeSet: link.kind === "change_set" && link.live && CHANGE_SET_ID.test(link.target) ? link.target : null,
  };
}

/** A tracker key from a project's name: the initials of its words of A to Z letters, or the
 * start of its one word, 2 to 8 letters. Input: the name. Output: the key. */
export function suggestKey(name: string): string {
  const words = name.toUpperCase().split(/\s+/).map((word) => word.replace(/[^A-Z]/g, "")).filter(Boolean);
  const key = (words.length > 1 ? words.map((word) => word[0]).join("") : words[0] ?? "").slice(0, 8);
  const shown = key.length === 0 ? "PROJ" : key.length === 1 ? key.repeat(2) : key;
  assert(KEY.test(shown), "a suggested key is 2 to 8 capital letters");
  return shown;
}

/** Why a typed key can't be used. Input: the key. Output: the sentence, or null for a key of
 * 2 to 8 capital letters. */
export function keyProblem(key: string): string | null {
  return KEY.test(key) ? null : REFUSALS.project_key_invalid;
}

/** The tracked project of an engine project. Inputs: the tracked projects and the project
 * id. Output: it, or null when the project has no tickets yet. */
export function trackedProject(projects: readonly TrackedProject[], projectId: string): TrackedProject | null {
  return projects.find((item) => item.project === projectId) ?? null;
}
