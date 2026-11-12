import { call, nextGeneration, receiptResult, type Result } from "./api";
import type { IntentReceipt } from "./engine-client";
import { formatRoute, type ProjectTab } from "./ui/router";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Which items need the owner (Action, pending until their state is fixed) and which only
 * tell (Info, read or unread), the engine's `NoticeClass`. */
export type NoticeClass = "action" | "info";

/** One notification kind, the engine's `NoticeKind` wire name. */
export type NoticeKind =
  | "question" | "account_sign_in" | "research_pool" | "memory_review" | "change_set_review"
  | "agent_finished" | "agent_failed" | "agent_stopped" | "master_off"
  | "change_set_landed" | "change_set_conflict" | "change_set_rolled_back"
  | "account_near_limit" | "account_at_limit" | "pool_failover"
  | "app_update_ready" | "engine_restarted" | "engine_lost" | "harness_update" | "team_alert";

/** Every kind in the engine's order, with its class and the label both clients show
 * (`NoticeKind::label`, checked against the engine source by a test). */
export const NOTICE_KINDS: readonly { kind: NoticeKind; class: NoticeClass; label: string }[] = [
  { kind: "question", class: "action", label: "Question" },
  { kind: "account_sign_in", class: "action", label: "Account sign-in" },
  { kind: "research_pool", class: "action", label: "Waiting for a pool" },
  { kind: "memory_review", class: "action", label: "Memories to review" },
  { kind: "change_set_review", class: "action", label: "Change set to review" },
  { kind: "agent_finished", class: "info", label: "Agent finished" },
  { kind: "agent_failed", class: "info", label: "Agent failed" },
  { kind: "agent_stopped", class: "info", label: "Agent stopped" },
  { kind: "master_off", class: "info", label: "Master turned off" },
  { kind: "change_set_landed", class: "info", label: "Change set landed" },
  { kind: "change_set_conflict", class: "info", label: "Change set conflict" },
  { kind: "change_set_rolled_back", class: "info", label: "Change set rolled back" },
  { kind: "account_near_limit", class: "info", label: "Account near its limit" },
  { kind: "account_at_limit", class: "info", label: "Account at its limit" },
  { kind: "pool_failover", class: "info", label: "Pool failover" },
  { kind: "app_update_ready", class: "info", label: "App update ready" },
  { kind: "engine_restarted", class: "info", label: "Engine restarted" },
  { kind: "engine_lost", class: "info", label: "Engine lost" },
  { kind: "harness_update", class: "info", label: "Harness update" },
  { kind: "team_alert", class: "info", label: "Team alert" },
];

/** The page a click opens, the engine's `NoticeTarget`: never an address, since item text
 * can come from agents. */
export type NoticeTarget =
  | { page: "questions"; project_id: string; question: number | null; agent: number | null }
  | { page: "account_sign_in"; account: string }
  | { page: "project_pool"; project_id: string }
  | { page: "memories"; project_id: string }
  | { page: "change_set"; project_id: string; change_set: string | null }
  | { page: "agents"; project_id: string }
  | { page: "conversation"; project_id: string }
  | { page: "accounts" }
  | { page: "usage" }
  | { page: "updates" }
  | { page: "harnesses" }
  | { page: "home" };

/** One item or folded row, the engine's `Notice`. */
export interface Notice {
  id: number;
  kind: NoticeKind;
  class: NoticeClass;
  project_id: string | null;
  text: string;
  target: NoticeTarget;
  created_ms: number;
  read_ms: number | null;
  resolved_ms: number | null;
  folded: number;
  unread: number;
  pending: number;
  ids: number[];
  banner: boolean;
}

export type NoticeFilter = "all" | "unread" | "pending" | "done";

export const NOTICE_FILTERS: readonly { id: NoticeFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "unread", label: "Unread" },
  { id: "pending", label: "Pending" },
  { id: "done", label: "Done" },
];

/** One read of the store, the engine's `NoticeQuery`. */
export interface NoticeQuery {
  filter: NoticeFilter;
  project_id: string | null;
  kind: NoticeKind | null;
  fold: boolean;
  before: number | null;
  after: number | null;
  limit: number;
}

export interface NoticeCounts {
  unread: number;
  pending: number;
}

/** One page of the store, the engine's `NoticePage`: the items, both counts, whether more
 * are older, and the newest id in the store. */
export interface NoticePage {
  items: Notice[];
  counts: NoticeCounts;
  more: boolean;
  newest: number;
}

export type Delivery = "on" | "bell_only" | "off";
export type Banners = "off" | "action_only" | "all";

/** The stored settings, the engine's `NoticeSettings`. */
export interface NoticeSettings {
  kinds: { kind: NoticeKind; delivery: Delivery }[];
  retention_days: number;
  banners: Banners;
}

/** One folded row as the panel listed it, the engine's `NoticeFold`. */
export interface NoticeFold {
  kind: NoticeKind;
  project_id: string | null;
  filter: NoticeFilter;
  newest: number;
}

export type FoldChange = "read" | "unread" | "delete";

/** One change to the store, the engine's `NoticeAction`. */
export type NoticeAction =
  | { read: { ids: number[] } }
  | { unread: { ids: number[] } }
  | { delete: { ids: number[] } }
  | { folds: { folds: NoticeFold[]; change: FoldChange } }
  | { read_all: { project_id: string | null } }
  | { save_settings: { settings: NoticeSettings } }
  | { report: { report: { kind: "app_update_ready"; version: string } | { kind: "engine_lost" } } };

export const PAGE_MAX = 50;
export const BADGE_MAX = 99;
export const RETENTION_DAYS_MIN = 1;
export const RETENTION_DAYS_DEFAULT = 30;
export const RETENTION_DAYS_MAX = 365;

/** What the engine holds before the first save: every kind On, Done items kept 30 days,
 * macOS notifications for all kinds. */
export const NOTICE_DEFAULTS: NoticeSettings = {
  kinds: NOTICE_KINDS.map((entry) => ({ kind: entry.kind, delivery: "on" })),
  retention_days: RETENTION_DAYS_DEFAULT,
  banners: "all",
};

/** The label of one kind. Input: the kind. Output: the words both clients show. */
export function kindLabel(kind: NoticeKind): string {
  const entry = NOTICE_KINDS.find((item) => item.kind === kind);
  assert(entry !== undefined, `unknown notification kind ${kind}`);
  return entry.label;
}

/** The delivery choices one kind offers: Action kinds can be silenced, never turned off.
 * Input: the kind. Output: the choices in order. */
export function deliveryChoices(kind: NoticeKind): Delivery[] {
  const entry = NOTICE_KINDS.find((item) => item.kind === kind);
  assert(entry !== undefined, `unknown notification kind ${kind}`);
  return entry.class === "action" ? ["on", "bell_only"] : ["on", "bell_only", "off"];
}

/** Reads the Done retention the owner typed. Input: the text. Output: whole days from
 * `RETENTION_DAYS_MIN` to `RETENTION_DAYS_MAX`, or null for anything else. */
export function retentionDays(typed: string): number | null {
  const text = typed.trim();
  if (!/^[0-9]{1,3}$/.test(text)) return null;
  const days = Number(text);
  return days >= RETENTION_DAYS_MIN && days <= RETENTION_DAYS_MAX ? days : null;
}

/** The text of one badge. Input: the count. Output: the count up to `BADGE_MAX`, then
 * "99+", or null at zero. */
export function badgeText(count: number): string | null {
  assert(Number.isInteger(count) && count >= 0, "a badge counts whole items");
  if (count === 0) return null;
  return count > BADGE_MAX ? `${BADGE_MAX}+` : String(count);
}

/** The page one item opens and the anchor of the part to highlight there. Input: the
 * target. Output: the location hash and the anchor, or null when the page itself is the
 * part. */
export function routeOf(target: NoticeTarget): { hash: string; anchor: string | null } {
  const project = (id: string, tab: ProjectTab, section?: string): string =>
    formatRoute(section === undefined ? { name: "project", id, tab } : { name: "project", id, tab, section });
  switch (target.page) {
    case "questions": {
      const anchor = target.question !== null ? `question-${target.question}` : target.agent !== null ? `agent-${target.agent}` : null;
      return { hash: project(target.project_id, "questions"), anchor };
    }
    case "account_sign_in": return { hash: formatRoute({ name: "settings", tab: "accounts", action: "sign_in", account: target.account }), anchor: null };
    case "project_pool": return { hash: project(target.project_id, "settings", "pool"), anchor: null };
    case "memories": return { hash: `#/projects/${encodeURIComponent(target.project_id)}/context?file=%40memory`, anchor: null };
    case "change_set": return { hash: project(target.project_id, "changes"), anchor: target.change_set ? `change-set-${target.change_set}` : null };
    case "agents": return { hash: project(target.project_id, "agents"), anchor: null };
    case "conversation": return { hash: project(target.project_id, "conversation"), anchor: null };
    case "accounts": return { hash: formatRoute({ name: "settings", tab: "accounts" }), anchor: null };
    case "usage": return { hash: formatRoute({ name: "metrics" }), anchor: null };
    case "updates": return { hash: formatRoute({ name: "settings", tab: "updates" }), anchor: null };
    case "harnesses": return { hash: formatRoute({ name: "settings", tab: "harnesses" }), anchor: null };
    case "home": return { hash: formatRoute({ name: "home" }), anchor: null };
  }
}

/** The query of the panel. Inputs: the filter and the project picked, or null for all.
 * Output: one folded page. */
export function panelQuery(filter: NoticeFilter, projectId: string | null): NoticeQuery {
  return { filter, project_id: projectId, kind: null, fold: true, before: null, after: null, limit: PAGE_MAX };
}

/** The query the bell's counts come from: the smallest page that still carries both
 * counts. Output: the query. */
export function countsQuery(): NoticeQuery {
  return { filter: "unread", project_id: null, kind: null, fold: false, before: null, after: null, limit: 1 };
}

/** The query of one project's Needs you view: its pending items, folded. Input: the
 * project. Output: the query. */
export function projectNeedsQuery(projectId: string): NoticeQuery {
  return { filter: "pending", project_id: projectId, kind: null, fold: true, before: null, after: null, limit: PAGE_MAX };
}

/** The query of the pending account sign-ins, which belong to no project. Output: the
 * query, unfolded so each account is its own item. */
export function signInsQuery(): NoticeQuery {
  return { filter: "pending", project_id: null, kind: "account_sign_in", fold: false, before: null, after: null, limit: PAGE_MAX };
}

/** One project's Needs you list: its own pending items and the pending sign-ins of the
 * accounts in its pool, newest first. Inputs: the project's page, the sign-ins page and the
 * pool's account ids. Output: the merged page; a failed sign-in read leaves the project's
 * own items. */
export function projectNeeds(own: Result<NoticePage> | { kind: "loading" }, signIns: Result<NoticePage> | { kind: "loading" }, accounts: readonly string[]): Result<NoticePage> | { kind: "loading" } {
  if (own.kind !== "ok") return own;
  const extra = signIns.kind === "ok"
    ? signIns.value.items.filter((item) => item.target.page === "account_sign_in" && accounts.includes(item.target.account))
    : [];
  const items = [...extra, ...own.value.items].sort((a, b) => b.id - a.id);
  assert(items.length <= own.value.items.length + extra.length, "the merge only joins two lists");
  return { kind: "ok", value: { ...own.value, items } };
}

/** The change that reaches every item of the given panel rows. Inputs: the rows, the filter
 * they were listed under and the change. Output: one fold change, or null for no rows or
 * more than a page. */
export function foldsAction(rows: readonly Notice[], filter: NoticeFilter, change: FoldChange): NoticeAction | null {
  if (rows.length === 0 || rows.length > PAGE_MAX) return null;
  const folds = rows.map((row) => ({ kind: row.kind, project_id: row.project_id, filter, newest: row.id }));
  return { folds: { change, folds } };
}

/** Reads one page of the store. Input: the query. Output: the page, or a plain error. */
export function loadNotifications(query: NoticeQuery): Promise<Result<NoticePage>> {
  assert(query.limit >= 1 && query.limit <= PAGE_MAX, "a page holds 1 to 50 items");
  return call<NoticePage>("desktop_notifications", { query });
}

/** Reads the notification settings. Output: the settings, or a plain error. */
export function loadNoticeSettings(): Promise<Result<NoticeSettings>> {
  return call<NoticeSettings>("desktop_notification_settings");
}

/** Sends one change to the store. Input: the change. Output: ok once it stands, else the
 * plain refusal. */
export async function sendNotice(action: NoticeAction): Promise<Result<void>> {
  const result = await call<IntentReceipt>("desktop_notification_action", { action, generation: nextGeneration() });
  return result.kind === "ok" ? receiptResult(result.value) : result;
}
