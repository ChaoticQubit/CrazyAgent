import { invoke } from "@tauri-apps/api/core";

export interface ConnectionStatus {
  connected: boolean;
  pending: boolean;
}

export interface BaselineRow {
  id: string;
  project_id: string;
  approved: boolean;
  revoked: boolean;
  current: boolean;
}

export interface BaselinePage {
  items: BaselineRow[];
  has_more: boolean;
  fresh: boolean;
}

export interface BaselineDetail extends BaselineRow {
  ticket: string;
  origin: string;
  requirements: string;
  checks: {
    id: string;
    kind: string;
    minimum_risk: string;
    content: string;
    encoding: "utf8" | "hex";
  }[];
  rules: { path_prefix: string; minimum_risk: string }[];
  retry_limit: number;
  fresh: boolean;
}

export interface ActionReceipt {
  baseline_id: string;
  outcome: "applied" | "rejected" | "unavailable" | "unknown" | "not_recorded_at_barrier" | "uncertain";
  pending: boolean;
}

export function connect(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("desktop_connect");
}

export function status(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("desktop_status");
}

export function baselines(next = false): Promise<BaselinePage> {
  return invoke<BaselinePage>("desktop_baselines", { next });
}

export function inspectBaseline(id: string): Promise<BaselineDetail> {
  return invoke<BaselineDetail>("desktop_inspect_baseline", { id });
}

export function approveBaseline(ticket: string): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_approve_baseline", { ticket });
}

export function revokeBaseline(ticket: string): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_revoke_baseline", { ticket });
}

export function reconcile(): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_reconcile");
}

export function retry(): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_retry");
}

export function errorMessage(error: unknown): string {
  const code = typeof error === "object" && error !== null && "code" in error
    ? String(error.code) : "unknown";
  const messages: Record<string, string> = {
    settings_source_conflict: "Global or project settings changed outside this editor. The engine host must reconcile the configured sources; then refresh and review retained values.",
    settings_source_replaced: "The configured settings file was replaced. Reconnect after the engine host verifies its source grant.",
    settings_publication_pending: "The engine has an unresolved durable settings publication. Reconcile the original intent after the engine recovers it.",
    account_exists: "That account reference already exists. Refresh and inspect it.",
    connection_exists: "That connection identity already exists. Inspect it before replacing it.",
    pool_exists: "That pool identity already exists. Inspect it before replacing it.",
    account_not_found: "That account reference no longer exists. Refresh the metadata.",
    connection_not_found: "That connection no longer exists. Refresh and inspect it again.",
    pool_not_found: "That pool no longer exists. Refresh and inspect it again.",
    model_access_record_not_found: "That metadata identity was not found. Refresh the collection and choose a current record.",
    owner_revoked: "Desktop owner authority was revoked. The engine host must restore it before reconnecting.",
    stale_revision: "The engine revision changed. Refresh and review retained values before saving.",
    stale_model_access: "This metadata form no longer matches its owner snapshot. Refresh and review the retained values before saving.",
    settings_read_only: "Model metadata is read-only. The engine host must grant writable settings sources before edits can be published.",
    settings_source_changed: "The settings source changed outside this editor. Refresh and review retained values before saving again.",
    settings_project_changed: "Project settings changed. Refresh the effective configuration and review retained values.",
    settings_pending: "The engine has an unresolved settings publication. Reconcile the original intent before another edit.",
    stale_config: "The effective configuration changed. Refresh and review retained values before saving.",
    invalid_model_access: "The engine rejected the metadata. Check references, endpoint/protocol/auth compatibility, unique pool members and project/role scope.",
    model_access_duplicate: "That metadata identity already exists. Refresh and inspect it before replacing it.",
    model_access_missing: "That metadata identity no longer exists. Refresh before choosing another edit.",
    model_access_referenced: "This record is still referenced by a connection or pool. Update those records before removal.",
    model_access_edit_bounds: "The metadata edit exceeds the engine byte limit. Reduce the entered values.",
    invalid_project: "The selected project identity is invalid.",
    draft_generation_limit: "This draft reached its safe generation limit. Preserve it before restarting.",
    conversation_text_bounds: "Text must be nonempty, contain no NUL, and fit within 8192 UTF-8 bytes. The draft was not submitted.",
    not_configured: "No engine connection was configured for this desktop session.",
    not_connected: "Connect to the engine before reading owner state.",
    busy: "Another owner request is still running. Wait for it to finish.",
    owner_unavailable: "The configured engine could not be authenticated. Check the engine and reconnect.",
    owner_reconnect_failed: "Reconnection was refused. Any pending owner intent remains retained.",
    owner_read_failed: "The owner read failed. Displayed data is stale; reconnect or refresh.",
    resync_required: "The engine requires a fresh snapshot. Refresh the first page.",
    no_next_page: "There is no next page. Refresh to read current state.",
    projection_limit: "This response exceeds the desktop display limit.",
    invalid_baseline: "The selected baseline identity is invalid.",
    stale_selection: "This inspection is no longer current. Inspect the baseline again before deciding.",
    selection_limit: "This native session reached its inspection limit. Restart the desktop.",
    decision_not_applicable: "That decision is unavailable for the inspected baseline state.",
    pending_intent: "An exact owner intent is still pending. Reconcile it before another decision.",
    no_pending_intent: "There is no retained intent in this native process.",
    owner_not_ready: "A fresh connected inspection is required before submitting a decision.",
    invalid_baseline_data: "The engine returned a baseline that could not be validated.",
    unexpected_reply: "The engine returned an unexpected response. Refresh before continuing.",
  };
  return messages[code] ?? "The owner request failed. Refresh before continuing.";
}

export interface ProjectPage {
  items: { id: string; name: string }[];
  has_more: boolean;
  fresh: boolean;
}

export interface ConversationRow {
  kind: "message" | "brief";
  version: string | null;
  text: string;
  event: string;
  revision: string;
  origin: string;
}

export interface ProjectDetail {
  project_id: string;
  ticket: string;
  brief: ConversationRow | null;
  fresh: boolean;
  revision: string;
}

export interface HistoryPage {
  project_id: string;
  entries: ConversationRow[];
  has_more: boolean;
  fresh: boolean;
  revision: string;
}

export interface PendingIntent {
  kind: "message" | "brief" | "approve_baseline" | "revoke_baseline" | "model_access";
  generation: string;
  project_id: string | null;
  baseline_id: string | null;
  model_access: ModelAccessEdit | null;
}

export interface IntentReceipt extends PendingIntent {
  outcome: ActionReceipt["outcome"];
  pending: boolean;
  rejection: string | null;
}

export function projects(next = false): Promise<ProjectPage> {
  return invoke<ProjectPage>("desktop_projects", { next });
}

export function openProject(id: string): Promise<ProjectDetail> {
  return invoke<ProjectDetail>("desktop_open_project", { id });
}

export function history(next = false): Promise<HistoryPage> {
  return invoke<HistoryPage>("desktop_history", { next });
}

export function sendMessage(ticket: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_send_message", { ticket, text, generation });
}

export function replaceBrief(ticket: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_replace_brief", { ticket, text, generation });
}

export function pendingIntent(): Promise<PendingIntent | null> {
  return invoke<PendingIntent | null>("desktop_pending_intent");
}

export function reconcileIntent(): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_reconcile_intent");
}

export function retryIntent(): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_retry_intent");
}

export type ModelAccessCollection = "accounts" | "connections" | "pools";

export type ModelAuthentication = { method: "none" } | { method: "api_key" | "oauth"; credential_ref: string } | { method: "native_managed"; profile_ref: string };

export interface ModelConnection {
  id: string;
  provider: string;
  harness: string;
  protocol: string;
  account_ref: string;
  endpoint: { kind: "http"; url: string } | { kind: "native" };
  model: { id: string; author: string };
  auth: ModelAuthentication;
}

export interface AccountPool {
  id: string;
  members: string[];
  projects: string[];
  roles: string[];
  fallback_allowed: boolean;
}

export type ModelAccessRecord = { kind: "account"; account_ref: string } | { kind: "connection"; connection: ModelConnection } | { kind: "pool"; pool: AccountPool };

export type ModelAccessEdit = { kind: "add_account" | "remove_account"; account_ref: string } | { kind: "create_connection" | "replace_connection"; connection: ModelConnection } | { kind: "remove_connection" | "remove_pool"; id: string } | { kind: "create_pool" | "replace_pool"; pool: AccountPool };

export interface ModelAccessBasis {
  ticket: string;
  collection: ModelAccessCollection;
  revision: string;
  config_id: string;
  source_hash: string;
  writable: boolean;
  fresh: boolean;
}

export interface ModelAccessPage extends ModelAccessBasis {
  items: ModelAccessRecord[];
  has_more: boolean;
}

export interface ModelAccessDetail extends ModelAccessBasis {
  record: ModelAccessRecord;
}

export function modelAccessPage(collection: ModelAccessCollection, next = false): Promise<ModelAccessPage> {
  return invoke<ModelAccessPage>("desktop_model_access_page", { collection, next });
}

export function inspectModelAccess(collection: ModelAccessCollection, id: string): Promise<ModelAccessDetail> {
  return invoke<ModelAccessDetail>("desktop_inspect_model_access", { collection, id });
}

export function editModelAccess(ticket: string, edit: ModelAccessEdit, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_edit_model_access", { ticket, edit, generation });
}
