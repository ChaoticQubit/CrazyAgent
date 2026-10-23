import { invoke as native, type InvokeArgs } from "@tauri-apps/api/core";
import type { NativeAuthMethod, NativeAuthView } from "./native-auth";

/** Retries a bridge call the native side refused with "busy" because another call held
 * the bridge; such a refusal happens before any action, so repeating it is safe.
 * Inputs: the call and a base pause in milliseconds. Output: the call result or failure. */
export async function retryBusy<T>(call: () => Promise<T>, pause = 250): Promise<T> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await call();
    } catch (failure) {
      const busy = typeof failure === "object" && failure !== null && "code" in failure && failure.code === "busy";
      if (!busy || attempt >= 8) throw failure;
      await new Promise((resolve) => setTimeout(resolve, pause * attempt));
    }
  }
}

/** Calls one native bridge command, retrying only the bridge's pre-action "busy" refusal.
 * Inputs: the command name and its arguments. Output: the command result or its failure. */
function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  return retryBusy(() => native<T>(command, args));
}

export function startNativeAuth(ticket: string, method: NativeAuthMethod): Promise<NativeAuthView> {
  return invoke<NativeAuthView>("desktop_start_native_auth", { ticket, method });
}

export function readNativeAuth(ticket: string): Promise<NativeAuthView> {
  return invoke<NativeAuthView>("desktop_read_native_auth", { ticket });
}

export function cancelNativeAuth(ticket: string): Promise<NativeAuthView> {
  return invoke<NativeAuthView>("desktop_cancel_native_auth", { ticket });
}

export function submitClaudeAuthCode(ticket: string, code: string): Promise<void> {
  return invoke<void>("desktop_submit_claude_auth_code", { ticket, code });
}

export function logoutNativeAuth(ticket: string): Promise<NativeAuthView> {
  return invoke<NativeAuthView>("desktop_logout_native_auth", { ticket });
}

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
    account_setup_unavailable: "Native account setup is not connected in this build. The provider and label remain retained; no account was created or signed in.",
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
    project_spec_bounds: "Use a nonblank name within 256 UTF-8 bytes and 1 to 64 absolute local paths within 4096 bytes each. The complete declaration must fit the native engine frame. Previous values are retained.",
    project_identity_unavailable: "A native project identity could not be generated. The draft is retained; reconnect before trying again.",
    draft_generation_limit: "This draft reached its safe generation limit. Preserve it before restarting.",
    master_open_unavailable: "No account pool is assigned to this project as its master. Assign one in Settings > Pools, then start the master again.",
    master_outcome_pending: "The engine has not confirmed this request yet. Use Reconcile intent at the top of the window, then refresh the master.",
    stale_master: "The master conversation changed. Refresh it before sending again.",
    master_source_conflict: "This master was opened before the engine or settings changed. Close it, start it again, then send.",
    invalid_session: "The master conversation identity is invalid. Refresh the master state.",
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
  items: { id: string; name: string; repository_count: number }[];
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
  kind: "message" | "brief" | "approve_baseline" | "revoke_baseline" | "model_access" | "create_project" | "setup_project" | "research_memory" | "project_documents" | "project_context_delete" | "memory_review" | "master";
  generation: string;
  project_id: string | null;
  baseline_id: string | null;
  model_access: ModelAccessEdit | null;
  project_spec: ProjectSpec | null;
  operation_id: string;
}

export interface IntentReceipt extends PendingIntent {
  outcome: ActionReceipt["outcome"];
  pending: boolean;
  rejection: string | null;
}

export function projects(next = false): Promise<ProjectPage> {
  return invoke<ProjectPage>("desktop_projects", { next });
}

export interface ProjectSpec {
  name: string;
  repositories: string[];
}

export function createProject(spec: ProjectSpec, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_create_project", { spec, generation });
}

export type Platform = "website" | "mobile_app" | "desktop_app" | "cli" | "api_backend" | "other";
export type ResearchKind =
  | "market_research"
  | "competitor_research"
  | "design_research"
  | "front_end_design"
  | "tech_stack_research";
export type ResearchStatus = "queued" | "needs_pool" | "running" | "done" | "failed" | "cancelled";

export interface HarnessSlot {
  harness: string;
  agents: number;
  tools: boolean;
}

export interface TeamPlan {
  master: string;
  master_tools: boolean;
  sub_harnesses: HarnessSlot[];
  min_agents: number;
  max_agents: number;
}

export interface ProjectSetup {
  about: string;
  tech_stack: string[];
  platforms: Platform[];
  research: ResearchKind[];
  pool: string | null;
  team: TeamPlan | null;
  documents: string[];
}

export interface ProjectCatalog {
  tech_stack: { id: string; label: string; group: "language" | "framework" | "database" }[];
  platforms: { kind: Platform; label: string }[];
  research: { kind: ResearchKind; label: string }[];
  document_extensions: string[];
  documents_max: number;
  document_bytes_max: number;
  project_document_bytes_max: number;
  default_team: TeamPlan;
}

export interface StoredSetup {
  project_id: string;
  name: string;
  about: string;
  tech_stack: string[];
  platforms: Platform[];
  research: ResearchKind[];
  pool: string | null;
  team: TeamPlan;
  documents: { name: string; bytes: number }[];
  folder: string;
  created_ms: number;
}

export interface ResearchJob {
  id: number;
  project_id: string;
  kind: ResearchKind;
  kind_label: string;
  status: ResearchStatus;
  status_label: string;
  report_path: string;
  harness: string;
  tools: boolean;
  attempts: number;
  claimer: string | null;
  error: string | null;
  cancel_requested: boolean;
  output_bytes: number;
  created_ms: number;
  started_ms: number | null;
  heartbeat_ms: number | null;
  finished_ms: number | null;
}

export interface ProjectContext {
  project_id: string;
  folder: string;
  items: { path: string; kind: "brief" | "document" | "research"; bytes: number }[];
}

export interface ContextChunk {
  path: string;
  bytes: number;
  offset: number;
  text: string | null;
  next_offset: number | null;
}

export function projectCatalog(pool: string | null, research: ResearchKind[]): Promise<ProjectCatalog> {
  return invoke<ProjectCatalog>("desktop_project_catalog", { pool, research });
}

export function setupProject(spec: ProjectSpec, setup: ProjectSetup, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_setup_project", { spec, setup, generation });
}

export function projectSetup(id: string): Promise<StoredSetup | null> {
  return invoke<StoredSetup | null>("desktop_project_setup", { id });
}

export function researchJobs(id: string): Promise<{ project_id: string; jobs: ResearchJob[] }> {
  return invoke<{ project_id: string; jobs: ResearchJob[] }>("desktop_research_jobs", { id });
}

export function projectContext(id: string): Promise<ProjectContext> {
  return invoke<ProjectContext>("desktop_project_context", { id });
}

export function projectContextItem(id: string, path: string, offset = 0): Promise<ContextChunk> {
  return invoke<ContextChunk>("desktop_project_context_item", { id, path, offset });
}

export function saveResearchToMemory(
  id: string,
  research: ResearchKind,
  section: string | null,
  generation: number,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_save_research_to_memory", { id, research, section, generation });
}

export function addProjectDocuments(id: string, documents: string[], generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_add_project_documents", { id, documents, generation });
}

export type MemoryStatus = "Provisional" | "Confirmed" | "Rejected" | "Removed" | "Retracted";

export type MemoryItem = {
  id: string;
  version: number;
  text: string;
  status: MemoryStatus;
  human_reviewed: boolean;
};

export type MemoryDecision =
  | { decision: "Keep" }
  | { decision: "Edit"; text: string }
  | { decision: "Reject" }
  | { decision: "Remove" };

export function projectMemories(id: string, after: string | null = null): Promise<MemoryItem[]> {
  return invoke<MemoryItem[]>("desktop_project_memories", { id, after });
}

export function reviewMemory(
  id: string,
  memory: string,
  version: number,
  decision: MemoryDecision,
  generation: number,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_review_memory", { id, memory, version, decision, generation });
}

export function deleteProjectContext(id: string, path: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_delete_project_context", { id, path, generation });
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
  provider_quota_policy?: "bounded_attempt" | "strict";
}

export interface ManagedAccountPlan {
  provider: "codex" | "claude";
  label: string;
  connection: ModelConnection;
  pool: AccountPool;
}

export function prepareManagedAccount(provider: "codex" | "claude", label: string): Promise<ManagedAccountPlan> {
  return invoke<ManagedAccountPlan>("desktop_prepare_managed_account", { provider, label });
}

export interface ConfiguredAccountPlan extends Omit<ManagedAccountPlan, "provider"> {
  provider: "api_key" | "local";
}

export type ProducerCapability = "available" | { unavailable: { code: string } };

export interface AccountAuthCapabilitiesReply {
  request_id: string;
  database_instance: string;
  engine_epoch: string;
  config_id: string;
  source_hash: string;
  connection: ModelConnection;
  managed: { method: NativeAuthMethod; producer: ProducerCapability }[];
  configured: ProducerCapability;
}

export interface AccountProfileReceipt {
  config_id: string;
  source_hash: string;
  connection: ModelConnection;
  profile_ref: string;
  custody_id: string;
  profile_config_sha256: string;
  profile_generation: number;
}

export interface AccountProfileView {
  ticket: string;
  generation: string;
  reply: {
    request_id: string;
    database_instance: string;
    engine_epoch: string;
    outcome: "not_recorded" | { configured_only: { receipt: AccountProfileReceipt; historical_replay: boolean } };
  };
  pending: boolean;
}

export function prepareConfiguredAccount(provider: "api_key" | "local", label: string, endpoint: string, model: string): Promise<ConfiguredAccountPlan> {
  return invoke<ConfiguredAccountPlan>("desktop_prepare_configured_account", { provider, label, endpoint, model });
}

export function accountAuthCapabilities(ticket: string): Promise<AccountAuthCapabilitiesReply> {
  return invoke<AccountAuthCapabilitiesReply>("desktop_account_auth_capabilities", { ticket });
}

export function configureAccountProfile(ticket: string, secret: string | undefined, generation: number): Promise<AccountProfileView> {
  return invoke<AccountProfileView>("desktop_configure_account_profile", { ticket, secret, generation });
}

export function readAccountProfile(): Promise<AccountProfileView> {
  return invoke<AccountProfileView>("desktop_read_account_profile");
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

export interface MasterTurn {
  id: string;
  ordinal: number;
  phase: "queued" | "blocked" | "prepared" | "delivering" | "running" | "waiting_approval" | "succeeded" | "failed" | "cancelled" | "uncertain";
  reason: string | null;
  reply: { text: string } | null;
}

export interface MasterState {
  project_id: string;
  ticket: string;
  session: { session_id: string; phase: "configured" | "closing" | "closed"; current_turn: string | null } | null;
}

export interface MasterTurns {
  ticket: string;
  page: { turns: MasterTurn[] };
}

export function masterState(ticket: string): Promise<MasterState> {
  return invoke<MasterState>("desktop_master_state", { ticket });
}

export function masterTurns(ticket: string, sessionId: string): Promise<MasterTurns> {
  return invoke<MasterTurns>("desktop_master_turns", { ticket, sessionId });
}

export function openMaster(ticket: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_open_master", { ticket, generation });
}

export function closeMaster(ticket: string, sessionId: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_close_master", { ticket, sessionId, generation });
}

export function submitMaster(ticket: string, sessionId: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_submit_master", { ticket, sessionId, text, generation });
}
