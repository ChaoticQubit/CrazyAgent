import { invoke as native, type Channel, type InvokeArgs } from "@tauri-apps/api/core";
import type { NativeAuthMethod, NativeAuthView } from "./native-auth";

/** Retries a bridge call the native side refused with "busy" because another call held
 * the bridge; such a refusal happens before any action, so repeating it is safe. Each pause
 * is spread at random so calls refused together do not all retry at the same moment again.
 * Inputs: the call and a base pause in milliseconds. Output: the call result or failure. */
export async function retryBusy<T>(call: () => Promise<T>, pause = 250): Promise<T> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await call();
    } catch (failure) {
      const busy = typeof failure === "object" && failure !== null && "code" in failure && failure.code === "busy";
      if (!busy || attempt >= 8) throw failure;
      await new Promise((resolve) => setTimeout(resolve, pause * attempt * (0.5 + Math.random())));
    }
  }
}

export type SettledCall = { ok: true; value: unknown } | { ok: false; failure: unknown };

const unconfirmedWatchers = new Set<() => void>();

/** Whether a bridge reply leaves an owner change unconfirmed: a reply the bridge still marks
 * pending, or a refusal because an earlier change is still pending.
 * Input: the settled call. Output: true when the app should check the kept change. */
export function leavesUnconfirmed(settled: SettledCall): boolean {
  if (settled.ok) {
    const value = settled.value;
    return typeof value === "object" && value !== null && !Array.isArray(value) && "pending" in value && value.pending === true;
  }
  const failure = settled.failure;
  return typeof failure === "object" && failure !== null && "code" in failure && failure.code === "pending_intent";
}

/** Subscribes to bridge replies that leave an owner change unconfirmed, so the app can name
 * the change and check it. Input: the listener. Output: a function that unsubscribes it. */
export function watchUnconfirmed(listener: () => void): () => void {
  unconfirmedWatchers.add(listener);
  return () => { unconfirmedWatchers.delete(listener); };
}

/** Tells every watcher when one settled bridge call left a change unconfirmed.
 * Input: the settled call. Output: none. */
export function reportReply(settled: SettledCall): void {
  if (!leavesUnconfirmed(settled)) return;
  for (const listener of [...unconfirmedWatchers]) listener();
}

/** Calls one native bridge command, retrying only the bridge's pre-action "busy" refusal, and
 * reports a reply that leaves an owner change unconfirmed.
 * Inputs: the command name and its arguments. Output: the command result or its failure. */
export async function bridgeCall<T>(command: string, args?: InvokeArgs): Promise<T> {
  try {
    const value = await retryBusy(() => native<T>(command, args));
    reportReply({ ok: true, value });
    return value;
  } catch (failure) {
    reportReply({ ok: false, failure });
    throw failure;
  }
}

/** Calls one native bridge command through the shared bridge call.
 * Inputs: the command name and its arguments. Output: the command result or its failure. */
function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  return bridgeCall<T>(command, args);
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

export type HarnessStatus = { kind: "ready" } | { kind: "not_installed" } | { kind: "limited"; reason: string };

export type ToolControl = { kind: "switchable" } | { kind: "limited"; reason: string };

export type TeamUse = { kind: "available" } | { kind: "unavailable"; reason: string };

export type AccountKind = "subscription" | "api_key" | "open_ai_compatible_url" | "local";

export interface HarnessEntry {
  id: string;
  name: string;
  status: HarnessStatus;
  status_label: string;
  executable: string | null;
  headless: string;
  tools: ToolControl;
  model_selection: boolean;
  account_kinds: AccountKind[];
  team: TeamUse;
  aliases: string[];
}

/** Lists every harness the engine knows, in owner order, with its status on this machine.
 * Input: none. Output: entries whose status_label reads "Ready", "Not installed" or
 * "Limited control: <reason>". */
export function harnessCatalog(): Promise<HarnessEntry[]> {
  return invoke<HarnessEntry[]>("desktop_harness_catalog");
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

const PLAIN_CODE = /^[a-z0-9_]{1,96}$/;

/** Names a failure's code after its sentence in the style every window uses. A code that is
 * not 1 to 96 lowercase letters, digits or underscores shows as `unknown`, so raw error text
 * never appears. Inputs: the sentence and the code. Output: "<Sentence>. (<code>)", with no
 * second full stop when the sentence already ends with one. */
export function withCode(sentence: string, code: string): string {
  const stop = sentence.endsWith(".") ? "" : ".";
  return `${sentence}${stop} (${PLAIN_CODE.test(code) ? code : "unknown"})`;
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
    pool_member_limit: "A pool can hold up to 15 accounts",
    pool_needs_account: "A pool needs at least one account. Add another account first, or delete the pool.",
    pool_project_limit: "A pool can serve up to 64 projects. Pick another pool for this project.",
    pool_name_bounds: "Use a pool name of 1 to 64 characters, without control characters.",
    owner_profile_name: "Enter your name, up to 100 characters.",
    skill_folder_not_found: "That folder was not found.",
    skill_not_a_folder: "Pick a folder, not a file.",
    skill_missing_skill_md: "This folder has no SKILL.md file.",
    skill_frontmatter_missing: "SKILL.md needs a name and description at the top.",
    skill_name_invalid: "The skill name in SKILL.md must be 1 to 64 lowercase letters, digits or hyphens.",
    skill_description_invalid: "The description in SKILL.md must be 1 to 1024 characters.",
    skill_too_many_files: "A skill folder can hold up to 200 files.",
    skill_too_large: "A skill folder can be up to 8 MB.",
    skill_too_deep: "A skill folder can be up to 8 folders deep.",
    skill_link_outside: "This folder links to files outside it.",
    skill_file_unsupported: "This folder holds something that is not a file or folder.",
    skill_unreadable: "Some files in this folder can't be read.",
    skill_name_taken: "A skill with this name already exists here.",
    skill_limit: "You can add up to 100 skills here.",
    skill_not_found: "This skill was removed.",
    skill_text_too_large: "This SKILL.md is too large to show here.",
    skill_page_invalid: "This skill changed while it was read. Open it again.",
    server_name_invalid: "Use 1 to 64 lowercase letters, digits, - or _ for the name.",
    server_name_taken: "An MCP server with this name already exists here.",
    server_limit: "You can add up to 50 MCP servers here.",
    server_not_found: "This MCP server was removed.",
    server_command_invalid: "Enter the full path to the command, starting with /.",
    server_args_invalid: "Use up to 32 arguments, each on one line.",
    server_url_invalid: "Use an https address. Plain http works only for this Mac, and the address can't hold a password.",
    server_secret_name_invalid: "Use letters, digits, _ or - for the secret name, starting with a letter.",
    server_secret_limit: "A server can have up to 16 secrets.",
    server_secret_duplicate: "Each secret needs its own name.",
    server_secret_not_found: "This secret was removed.",
    server_secret_invalid: "Enter the secret on one line, up to 8192 characters.",
    server_changed: "This server changed. Review it again before approving.",
    server_check_command_only: "Only command servers can be checked from here.",
    server_check_busy: "Other checks are running. Try again in a moment.",
    server_command_not_found: "Can't start: command not found.",
    server_command_not_runnable: "Can't start: this file can't be run.",
    server_cant_start: "The server can't start.",
    server_exited: "The server stopped before answering.",
    server_check_timeout: "The server did not answer within 10 seconds.",
    server_output_too_large: "The server sent too much output.",
    server_refused: "The server refused the request.",
    server_reply_invalid: "The server's answer was not valid MCP.",
    plugin_project_unknown: "This project was not found.",
    plugin_not_found: "This item was removed.",
    plugin_switch_global_only: "Only items from global settings can be switched here.",
    plugin_store_unavailable: "CrazyAgent can't write its plugin folder.",
    plugin_store_unsafe: "CrazyAgent can't write its plugin folder.",
    plugin_secret_store_unavailable: "CrazyAgent can't write its plugin folder.",
    plugins_failed: "The plugins request failed. Refresh before continuing.",
    setup_busy: "Setup is already running.",
    setup_store_unavailable: "CrazyAgent can't write its app data folder.",
    setup_outcome_unknown: "The setup request did not finish. Refresh before continuing.",
    pack_unknown: "This entry is not in the default pack.",
    pack_not_installed: "This entry is not installed yet. Run setup first.",
    pack_name_taken: "A skill or server with this name already exists.",
    agent_not_found: "This agent was not found. Refresh the list.",
    agent_not_running: "This agent is not running. Refresh the list.",
    agent_not_busy: "This agent is not working on anything to interrupt.",
    agent_busy: "This agent is still handling the last request. Try again in a moment.",
    agent_reconnected_read_only: "CrazyAgent reconnected to this agent after a restart. It can be stopped or restarted, not interrupted.",
    agent_restart_needs_key: "CrazyAgent no longer holds this agent's API key since it restarted. Start the agent again from its project.",
    agent_restart_conflict: "This agent is already running again. Refresh the list.",
    agent_restart_limit: "This agent was restarted too many times. Start it again from its project.",
    agent_launch_failed: "The agent could not start. Check its harness and account, then restart it.",
    agent_spec_invalid: "This agent's saved settings are unreadable. Start it again from its project.",
    agent_registry_unavailable: "CrazyAgent can't read its agent list. Restart CrazyAgent.",
    agent_root_unsafe: "CrazyAgent's state folder is not private. Restart CrazyAgent after fixing its permissions.",
    agent_state_unavailable: "CrazyAgent can't write this agent's state folder.",
    agent_thread_unavailable: "CrazyAgent could not start watching this agent. Restart it.",
    agents_outcome_unknown: "The agent request did not finish. Refresh the list before trying again.",
    owner_profile_role: "Keep the role on one line, up to 100 characters.",
    owner_profile_bio: "Keep the bio under 1000 characters.",
    owner_profile_stack: "List up to 12 different technologies, each up to 40 characters.",
    owner_profile_bounds: "The profile is too large. Shorten it and save again.",
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
  return messages[code] ?? withCode("The owner request failed. Refresh before continuing.", code);
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
  kind: "message" | "brief" | "approve_baseline" | "revoke_baseline" | "model_access" | "create_project" | "setup_project" | "research_memory" | "project_documents" | "project_context_delete" | "project_read_paths" | "memory_review" | "memory_add" | "memory_promote" | "memory_delete" | "memory_forget" | "memory_remember" | "memory_selection" | "master" | "owner_profile" | "projects_home" | "move_projects_home" | "add_existing_folder" | "start_onboarding" | "run_setup" | "pack_set" | "pack_remove" | "team" | "project_setup_update" | "project_remove" | "research_cancel" | "research_retry";
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

export type SavedMemory = {
  id: string;
  text: string;
  scope: "global" | "project";
  project_id: string | null;
  origin: "written" | "promoted" | "selected";
  from_project_id: string | null;
  source_path: string | null;
};

export type SavedMemoryPage = {
  items: SavedMemory[];
  next: number | null;
};

export type ApprovedMemory = {
  scope: "global" | "project";
  kind: "note" | "promoted" | "research" | "selection";
  text: string;
};

export type ApprovedMemories = {
  items: ApprovedMemory[];
  complete: boolean;
};

export const MEMORY_NOTE_MAX = 2000;

export function memories(
  project: string | null = null,
  search: string | null = null,
  before: number | null = null,
): Promise<SavedMemoryPage> {
  return invoke<SavedMemoryPage>("desktop_memories", { project, search, before });
}

export function memory(id: string): Promise<SavedMemory | null> {
  return invoke<SavedMemory | null>("desktop_memory", { id });
}

export function approvedMemories(project: string): Promise<ApprovedMemories> {
  return invoke<ApprovedMemories>("desktop_approved_memories", { project });
}

export function addMemory(project: string | null, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_add_memory", { project, text, generation });
}

export function promoteMemory(
  project: string,
  memory: string,
  version: number,
  generation: number,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_promote_memory", { project, memory, version, generation });
}

export function deleteMemory(id: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_delete_memory", { id, generation });
}

export function rememberResearch(
  project: string,
  research: ResearchKind,
  section: string | null,
  generation: number,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_remember_research", { project, research, section, generation });
}

export function saveSelectionToMemory(id: string, path: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_save_selection_to_memory", { id, path, text, generation });
}

export function forgetMemory(project: string, memory: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_forget_memory", { project, memory, generation });
}

export function deleteProjectContext(id: string, path: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_delete_project_context", { id, path, generation });
}

export function projectReadPaths(id: string): Promise<string[]> {
  return invoke<string[]>("desktop_project_read_paths", { id });
}

export function setProjectReadPaths(id: string, paths: string[], generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_set_project_read_paths", { id, paths, generation });
}

export type TeamRole = "master" | "sub";
export type TeamAgentStatus = "starting" | "idle" | "working" | "stopped" | "failed";
export type TeamTaskStatus = "queued" | "needs_pool" | "running" | "done" | "failed" | "cancelled";
export type TeamMessageAuthor = "owner" | "master" | "engine";
export type TeamMessageState = "pending" | "sent" | "answered" | "failed" | "reply" | "note";

export interface TeamHarness {
  harness: string;
  role: TeamRole;
  agents_max: number;
  tools: boolean;
  live: number;
  delegation: "reply" | "tool" | "unavailable" | null;
  delegation_label: string | null;
  runnable: boolean;
}

export interface TeamAgent {
  id: number;
  role: TeamRole;
  label: string;
  harness: string;
  tools: boolean;
  account: string | null;
  worktree: string | null;
  status: TeamAgentStatus;
  status_label: string;
  task: number | null;
  turns: number;
  problem: string | null;
  process_group: number | null;
  created_ms: number;
  heartbeat_ms: number;
  stopped_ms: number | null;
  persona?: string | null;
  handle?: string | null;
}

export interface TeamTask {
  id: number;
  source: "master" | "owner";
  harness: string;
  tools: boolean;
  prompt: string;
  status: TeamTaskStatus;
  status_label: string;
  agent: number | null;
  attempts: number;
  result: string | null;
  problem: string | null;
  delivered: boolean;
  output_bytes: number;
  created_ms: number;
  started_ms: number | null;
  finished_ms: number | null;
}

export interface TeamMessage {
  id: number;
  author: TeamMessageAuthor;
  text: string;
  state: TeamMessageState;
  created_ms: number;
}

export interface TeamStatus {
  project_id: string;
  keep_warm: boolean;
  paused: boolean;
  min_agents: number;
  max_agents: number;
  harnesses: TeamHarness[];
  agents: TeamAgent[];
  tasks: TeamTask[];
  messages: TeamMessage[];
  plan: TeamPlanStep[];
}

export interface TeamPlanStep {
  reply: number;
  step: number;
  harness: string | null;
  prompt: string;
  task: number | null;
}

export function teamStatus(id: string): Promise<TeamStatus> {
  return invoke<TeamStatus>("desktop_team_status", { id });
}

export function teamMessage(id: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_message", { id, text, generation });
}

export function teamDelegate(
  id: string,
  harness: string | null,
  prompt: string,
  generation: number,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_delegate", { id, harness, prompt, generation });
}

export function teamCancel(id: string, task: number | null, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_cancel", { id, task, generation });
}

export function teamKeepWarm(id: string, on: boolean, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_keep_warm", { id, on, generation });
}

export function teamPause(id: string, paused: boolean, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_pause", { id, paused, generation });
}

export function teamHandOff(id: string, reply: number, step: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_hand_off", { id, reply, step, generation });
}

/** Renames a live sub-agent's role handle. Inputs: project id, team agent id, the new handle
 * in kebab case and a generation. Output: the commit receipt; a taken handle is rejected. */
export function teamRename(id: string, agent: number, handle: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_rename", { id, agent, handle, generation });
}

/** Changes a project's name, description, stack, platforms, research, pool and team.
 * Inputs: project id, the new setup, a generation, an optional new name and the setup revision
 * shown (the engine's current one when null). Output: the engine receipt. */
export function updateProjectSetup(
  id: string,
  setup: ProjectSetup,
  generation: number,
  name: string | null = null,
  revision: number | null = null,
): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_update_project_setup", { id, setup, name, revision, generation });
}

/** Removes a project from CrazyAgent; its research stops and its folder stays on disk.
 * Inputs: project id and a generation. Output: the engine receipt. */
export function removeProject(id: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_remove_project", { id, generation });
}

/** Stops one research job; a running agent's whole process group stops.
 * Inputs: project id, job id and a generation. Output: the engine receipt. */
export function cancelResearch(id: string, job: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_cancel_research", { id, job, generation });
}

/** Queues a failed or cancelled research job again.
 * Inputs: project id, job id and a generation. Output: the engine receipt. */
export function retryResearch(id: string, job: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_retry_research", { id, job, generation });
}

/** Reads the last plain lines of one research job's output, with secrets hidden.
 * Inputs: project id and job id. Output: the lines, oldest first. */
export function researchLog(id: string, job: number): Promise<{ lines: string[] }> {
  return invoke<{ lines: string[] }>("desktop_research_log", { id, job });
}

export interface ProjectsHome {
  path: string | null;
  watching: boolean;
}

export type FoundProjectState = "found" | "missing" | "onboarding" | "ready";

export interface FoundProject {
  name: string;
  path: string;
  git: boolean;
  known: boolean;
  state: FoundProjectState;
  project_id?: string | null;
}

export interface FolderSize {
  bytes: number;
  github?: string | null;
  worktrees?: number;
}

export type AddFolderMode = "move" | "copy";

export type OnboardingStep = { kind: "waiting"; until_ms?: number } | { kind: "running" } | { kind: "done" } | { kind: "off" } | { kind: "not_installed" } | { kind: "failed"; reason: string };

export interface MappingFacts {
  languages: string[];
  build_commands: string[];
  test_commands: string[];
  entry_points: string[];
}

export interface OnboardingView {
  project_id: string;
  graph: OnboardingStep;
  mapping: OnboardingStep;
  name: string;
  draft: ProjectSetup;
  facts: MappingFacts;
  missing: string[];
  overview: string | null;
  finished: boolean;
}

export interface GitHubDeviceCode {
  user_code: string;
  verification_uri: string;
  expires_s: number;
}

export type GitHubMethod = "device" | "token" | "cli" | "none";

export interface GitHubStatus {
  signed_in: boolean;
  login: string | null;
  method: GitHubMethod;
  waiting: boolean;
  problem: string | null;
}

export interface GitHubRepo {
  full_name: string;
  private: boolean;
  default_branch: string;
  size_kb: number;
}

export interface GitHubRepoPage {
  repos: GitHubRepo[];
  next: number | null;
}

export interface CloneReceipt {
  full_name: string;
  path: string;
}

/** Starts "Sign in with GitHub". Output: the code to type at the GitHub page and its
 * address; the engine waits for approval in the background, so poll githubStatus. */
export function githubSignIn(): Promise<GitHubDeviceCode> {
  return invoke<GitHubDeviceCode>("desktop_github_sign_in");
}

/** Saves a pasted GitHub token. It goes to the engine once and is never kept by the page.
 * Output: the status after the engine checked the token with GitHub. */
export function githubToken(token: string): Promise<GitHubStatus> {
  return invoke<GitHubStatus>("desktop_github_token", { token });
}

/** Reuses the token of a signed-in GitHub CLI on this Mac. Output: the new status. */
export function githubUseCli(): Promise<GitHubStatus> {
  return invoke<GitHubStatus>("desktop_github_use_cli");
}

/** Reads whether CrazyAgent is signed in to GitHub, as whom, how, and any plain problem.
 * Output: the status. */
export function githubStatus(): Promise<GitHubStatus> {
  return invoke<GitHubStatus>("desktop_github_status");
}

/** Lists one page (from 1) of the repositories the signed-in owner can clone. */
export function githubRepos(page = 1): Promise<GitHubRepoPage> {
  return invoke<GitHubRepoPage>("desktop_github_repos", { page });
}

/** Clones one repository, as "owner/name", into the projects home. Output: where it landed. */
export function cloneRepo(fullName: string): Promise<CloneReceipt> {
  return invoke<CloneReceipt>("desktop_clone_repo", { fullName });
}

export type DependencyName = "tmux" | "uv" | "graphify";

export type DependencyState =
  | { state: "checking" }
  | { state: "missing" }
  | { state: "found"; version: string }
  | { state: "installing" }
  | { state: "installed"; version: string }
  | { state: "failed"; reason: string };

export interface Dependency {
  name: DependencyName;
  label: string;
  state: DependencyState;
}

export type PackEntryKind = "skill" | "mcp_server";
export type PackGroup = "graph" | "workflow" | "design" | "reply_style" | "context" | "optional";

export type PackState = { state: "available" } | { state: "installing" } | { state: "on" } | { state: "off" } | { state: "failed"; reason: string };

export interface PackEntry {
  id: string;
  kind: PackEntryKind;
  group: PackGroup;
  source: string;
  license: string;
  on_by_default: boolean;
  state: PackState;
}

export interface SetupStatus {
  dependencies: Dependency[];
  pack: PackEntry[];
  running: boolean;
}

/** Reads the projects home folder and whether the engine watches it. Output: the home. */
export function projectsHome(): Promise<ProjectsHome> {
  return invoke<ProjectsHome>("desktop_projects_home");
}

/** Picks the projects home folder; nothing on disk moves.
 * Inputs: an absolute folder path and a generation. Output: the engine receipt. */
export function setProjectsHome(path: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_set_projects_home", { path, generation });
}

/** Moves every project folder into a new projects home, after the owner saw the size.
 * Inputs: the new absolute folder and a generation. Output: the engine receipt. */
export function moveProjectsHome(path: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_move_projects_home", { path, generation });
}

/** Lists the folders in the projects home with their state. Output: the found projects. */
export function foundProjects(): Promise<FoundProject[]> {
  return invoke<FoundProject[]>("desktop_found_projects");
}

/** Measures a folder before Copy and names its GitHub repository, if any.
 * Input: an absolute folder path. Output: its size and GitHub owner/name. */
export function folderSize(path: string): Promise<FolderSize> {
  return invoke<FolderSize>("desktop_folder_size", { path });
}

/** Moves or copies an existing folder into the projects home.
 * Inputs: the source folder, move or copy, and a generation. Output: the engine receipt. */
export function addExistingFolder(source: string, mode: AddFolderMode, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_add_existing_folder", { source, mode, generation });
}

/** Starts onboarding a found folder on a pool. Inputs: the folder, the pool id and a
 * generation. Output: the engine receipt; Home then lists the folder as setting up. */
export function startOnboarding(path: string, pool: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_start_onboarding", { path, pool, generation });
}

/** Reads one project's onboarding with the mapping agent's overview. Input: the project id. */
export function onboarding(id: string): Promise<OnboardingView> {
  return invoke<OnboardingView>("desktop_onboarding", { id });
}

/** Signs CrazyAgent out of GitHub and deletes its keychain item. Output: the new status. */
export function githubSignOut(): Promise<GitHubStatus> {
  return invoke<GitHubStatus>("desktop_github_sign_out");
}

/** Reads setup: tools, the default pack and whether setup is running. Output: the status. */
export function setupStatus(): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_setup_status");
}

/** Starts setup with the ticked pack entries; the ticked MCP servers count as approved.
 * Inputs: the ticked ids and a generation. Output: the status as the run starts. */
export function runSetup(pack: string[], generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_run_setup", { pack, generation });
}

/** Switches one installed pack entry on or off. Inputs: its id, the state and a generation. */
export function setPackEntry(id: string, on: boolean, generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_set_pack_entry", { id, on, generation });
}

/** Removes one installed pack entry and what it added. Inputs: its id and a generation. */
export function removePackEntry(id: string, generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_remove_pack_entry", { id, generation });
}

/** What one node of a project's team graph stands for: the master, one harness with its
 * agent count, or one agent. */
export type GraphNodeKind = "master" | "harness_group" | "agent";
export type GraphEdgeState = "sent" | "running" | "done" | "failed";

/** One node of a project's team graph, as the engine's TeamGraph read returns it. `parent`
 * links an agent to its harness group and a group to the master; `terminal` is the name the
 * terminal commands take, for the master and agents that have one. `questions` counts the
 * questions an agent waits on, once the engine reports them. */
export interface GraphNode {
  id: string;
  kind: GraphNodeKind;
  parent: string | null;
  label: string;
  harness: string;
  status: TeamAgentStatus | null;
  status_label: string | null;
  account: string | null;
  agents: number | null;
  terminal: string | null;
  questions?: number | null;
}

/** One task edge from the master to the agent running it, or to its harness group before
 * that. `reply` is the result or the plain problem once the task ended. */
export interface GraphEdge {
  from: string;
  to: string;
  task_id: number;
  source: "master" | "owner";
  text: string;
  state: GraphEdgeState;
  reply: string | null;
}

/** One project's team as a graph: the master, harness groups, agents and task edges. */
export interface TeamGraph {
  project_id: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

/** A control message on an attached terminal channel: the terminal's size once attached,
 * then a close with a plain reason, or a rejection with a plain code when it never attached. */
export type TerminalFrame =
  | { kind: "attached"; agent_id: string; cols: number; rows: number }
  | { kind: "closed"; reason: string }
  | { kind: "rejected"; code: string };

/** One message on an attached terminal channel: raw screen or output bytes, in order (the
 * current screen first), or a control frame. */
export type TerminalMessage = ArrayBuffer | TerminalFrame;

/** Reads one project's team graph. Input: the project id. Output: nodes and task edges. */
export function teamGraph(id: string): Promise<TeamGraph> {
  return invoke<TeamGraph>("desktop_team_graph", { id });
}

/** Opens a live view of one agent's terminal. The channel gets the attach frame, the screen
 * and then live output until the terminal closes or the view detaches. Inputs: the terminal
 * name and the channel. Output: resolves once the engine accepted the attach. */
export function terminalAttach(agent: string, output: Channel<TerminalMessage>): Promise<void> {
  return invoke<void>("desktop_terminal_attach", { agent, output });
}

/** Types bytes into an attached agent terminal exactly as given. Inputs: the terminal name
 * and 1 to 4096 bytes. Output: resolves once the bridge queued them in order. */
export function terminalInput(agent: string, bytes: number[]): Promise<void> {
  return invoke<void>("desktop_terminal_input", { agent, bytes });
}

/** Sends the owner's explicit prompt to an attached agent terminal as one submission; a
 * sub-agent's master hears a note of it. Inputs: the terminal name and up to 4096 bytes of
 * text. Output: resolves once the bridge queued it in order. */
export function terminalPrompt(agent: string, text: string): Promise<void> {
  return invoke<void>("desktop_terminal_prompt", { agent, text });
}

/** Sets an attached agent terminal's size for its app and every viewer. Inputs: the terminal
 * name, columns and rows, each from 2 to 1000. Output: resolves once queued. */
export function terminalResize(agent: string, cols: number, rows: number): Promise<void> {
  return invoke<void>("desktop_terminal_resize", { agent, cols, rows });
}

/** Closes this window's view of an agent terminal; the agent keeps running. Input: the
 * terminal name. Output: resolves once the view is gone. */
export function terminalDetach(agent: string): Promise<void> {
  return invoke<void>("desktop_terminal_detach", { agent });
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
  name?: string;
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

export type ModelAccessEdit = { kind: "add_account" | "remove_account"; account_ref: string } | { kind: "create_connection" | "replace_connection"; connection: ModelConnection } | { kind: "remove_connection" | "remove_pool"; id: string } | { kind: "create_pool" | "replace_pool"; pool: AccountPool } | { kind: "assign_project_pool"; project: string; pool: string | null };

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

export interface OwnerProfile {
  name: string;
  role: string;
  bio: string;
  preferred_stack: string[];
  working_style: { detail: "terse" | "detailed"; initiative: "ask_first" | "act_first" };
}

export interface PoolMember {
  id: string;
  kind: "chatgpt" | "claude" | "api_key" | "url" | "local" | "other";
  provider: string;
  status: "signed_in" | "not_signed_in" | "set_up";
  status_label: string;
}

export interface PoolSummary {
  id: string;
  name: string;
  summary: string;
  members: PoolMember[];
  project_count: number;
}

/** Reads the owner's first-run profile. Output: null on first run, which shows the first-run page. */
export function ownerProfile(): Promise<OwnerProfile | null> {
  return invoke<OwnerProfile | null>("desktop_owner_profile");
}

/** Saves or updates the owner profile. Inputs: the full profile and a fresh draft generation. */
export function saveOwnerProfile(profile: OwnerProfile, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_save_owner_profile", { profile, generation });
}

/** Reads every pool with plain counts such as "2 ChatGPT, 1 Claude" and member status words. */
export function poolSummaries(): Promise<PoolSummary[]> {
  return invoke<PoolSummary[]>("desktop_pool_summaries");
}

export type PluginScope = { kind: "global" } | { kind: "project"; project_id: string };

export type McpServerSpec = { kind: "stdio"; command: string; args: string[] } | { kind: "http"; url: string };

export type McpServerStatus = "waiting_for_approval" | "approved" | "off" | "cant_start";

export interface SkillView {
  id: string;
  name: string;
  description: string;
  line: string;
  scope: PluginScope;
  scope_label: string;
  enabled: boolean;
  on_here: boolean | null;
  source: string;
  folder: string;
}

export interface SkillFile {
  path: string;
  bytes: number;
}

export interface SkillDetail {
  skill: SkillView;
  text: string;
  files: SkillFile[];
  files_more: number;
}

export interface McpServerView {
  id: string;
  name: string;
  scope: PluginScope;
  scope_label: string;
  spec: McpServerSpec;
  runs: string;
  enabled: boolean;
  status: McpServerStatus;
  status_label: string;
  problem: string | null;
  tools: string[];
  checks: number;
  secrets: { name: string; saved: boolean }[];
  on_here: boolean | null;
}

export interface EffectivePlugins {
  skills: { name: string; folder: string }[];
  servers: { name: string; spec: McpServerSpec; secrets: { name: string; reference: string }[] }[];
}

/** Lists skills for global settings (no project) or for one project's settings. */
export function pluginSkills(projectId?: string): Promise<SkillView[]> {
  return invoke<SkillView[]>("desktop_plugin_skills", { projectId: projectId ?? null });
}

/** Lists MCP servers with plain status words for global settings or one project. */
export function pluginServers(projectId?: string): Promise<McpServerView[]> {
  return invoke<McpServerView[]>("desktop_plugin_servers", { projectId: projectId ?? null });
}

/** Adds a skill from a picked folder that holds SKILL.md; the engine keeps its own copy. */
export function addSkill(scope: PluginScope, folder: string): Promise<SkillView> {
  return invoke<SkillView>("desktop_add_skill", { scope, folder });
}

/** Turns a skill on or off where it was added. */
export function setSkillEnabled(id: string, enabled: boolean): Promise<SkillView> {
  return invoke<SkillView>("desktop_set_skill_enabled", { id, enabled });
}

/** Removes a skill and the engine's copy of its folder. */
export function removeSkill(id: string): Promise<void> {
  return invoke<void>("desktop_remove_skill", { id });
}

/** Reads one skill with its full SKILL.md text and file list, with a project's switch when given. */
export function skillDetail(id: string, projectId?: string): Promise<SkillDetail> {
  return invoke<SkillDetail>("desktop_skill_detail", { id, projectId: projectId ?? null });
}

/** Adds an MCP server that waits for approval. Secret values go to the engine and are never returned. */
export function addMcpServer(scope: PluginScope, name: string, spec: McpServerSpec, secrets: [string, string][]): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_add_mcp_server", { scope, name, spec, secrets });
}

/** Changes how an MCP server runs; it then waits for approval again. */
export function updateMcpServer(id: string, spec: McpServerSpec): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_update_mcp_server", { id, spec });
}

/** Approves an MCP server exactly as shown; a server that changed since is refused. */
export function approveMcpServer(id: string, spec: McpServerSpec): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_approve_mcp_server", { id, spec });
}

/** Turns an MCP server on or off where it was added. */
export function setMcpServerEnabled(id: string, enabled: boolean): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_set_mcp_server_enabled", { id, enabled });
}

/** Removes an MCP server and its saved secrets. */
export function removeMcpServer(id: string): Promise<void> {
  return invoke<void>("desktop_remove_mcp_server", { id });
}

/** Switches a global skill or MCP server on or off for one project. */
export function switchPlugin(projectId: string, id: string, on: boolean): Promise<void> {
  return invoke<void>("desktop_switch_plugin", { projectId, id, on });
}

/** Saves or replaces one MCP server secret. */
export function setMcpSecret(id: string, name: string, value: string): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_set_mcp_secret", { id, name, value });
}

/** Deletes one MCP server secret. */
export function removeMcpSecret(id: string, name: string): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_remove_mcp_secret", { id, name });
}

/** Starts a command MCP server, lists its tools and stops it. */
export function checkMcpServer(id: string): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_check_mcp_server", { id });
}

/** Which part of a project's team an agent plays. */
export type AgentRole = "master" | "sub_agent" | "research";

/** Where an agent is; "closed" means it stopped when CrazyAgent closed. */
export type AgentState = "running" | "stopped" | "exited" | "failed" | "closed";

/** One launched agent, its plain status and the controls usable now. */
export interface AgentView {
  id: string;
  project: string;
  role: AgentRole;
  label: string;
  harness: string;
  state: AgentState;
  status: string;
  reattached: boolean;
  busy: boolean;
  attempt: number;
  can_stop: boolean;
  can_interrupt: boolean;
  can_restart: boolean;
  exit_code: number | null;
  reason: string | null;
  last_text: string | null;
  started_ms: number;
  ended_ms: number | null;
}

/** Something other than an agent that holds an account's sign-in while it runs: a master
 * conversation's turn, ended by closing that conversation, or a research job, ended by
 * cancelling it. */
export type Holder =
  | { kind: "master"; project: string; session: string }
  | { kind: "research"; project: string; job: number; title: string };

/** Everything that holds one account's sign-in now: its running agents and other holders. */
export interface AccountUse {
  agents: AgentView[];
  holders: Holder[];
}

/** Lists launched agents, newest first, for one project or for every project. */
export function agents(projectId?: string): Promise<AgentView[]> {
  return invoke<AgentView[]>("desktop_agents", { projectId: projectId ?? null });
}

/** Stops one agent and everything it started; other agents keep running. */
export function stopAgent(id: string): Promise<AgentView> {
  return invoke<AgentView>("desktop_stop_agent", { id });
}

/** Interrupts what one agent is doing now; the agent keeps running. */
export function interruptAgent(id: string): Promise<void> {
  return invoke<void>("desktop_interrupt_agent", { id });
}

/** Restarts one agent with its first task. */
export function restartAgent(id: string): Promise<AgentView> {
  return invoke<AgentView>("desktop_restart_agent", { id });
}

/** Reads what agents launched for a project receive: skill folders and approved MCP servers. */
export function projectPlugins(projectId: string): Promise<EffectivePlugins> {
  return invoke<EffectivePlugins>("desktop_project_plugins", { projectId });
}

export interface MasterAccount {
  account_ref: string;
  connection: string;
  harness: string;
  reason: string | null;
}

export interface MasterTurn {
  id: string;
  ordinal: number;
  phase: "queued" | "blocked" | "prepared" | "delivering" | "running" | "waiting_approval" | "succeeded" | "failed" | "cancelled" | "uncertain";
  reason: string | null;
  reply: { text: string } | null;
  attempt?: { connection: { account_ref: string } } | null;
  accounts?: MasterAccount[];
  detail?: string | null;
  cause?: string | null;
  tasks?: MasterTask[];
  changes?: string | null;
  engine_request?: MasterEngineRequest | null;
}

/** What the engine asked the master in a turn Ask agent to update started: the change set,
 * the plain line shown in the chat and the text the master receives. */
export interface MasterEngineRequest {
  change_set: string;
  line: string;
  text: string;
}

/** One task a master turn handed to the project's team, with the engine's note once it ended. */
export interface MasterTask {
  task: number;
  note: string | null;
}

export interface MasterState {
  project_id: string;
  ticket: string;
  session: { session_id: string; phase: "configured" | "closing" | "closed"; current_turn: string | null } | null;
}

export interface MasterTurns {
  ticket: string;
  page: { turns: MasterTurn[]; next: unknown };
}

export function masterState(ticket: string): Promise<MasterState> {
  return invoke<MasterState>("desktop_master_state", { ticket });
}

export function masterTurns(ticket: string, sessionId: string, cursor: unknown = null): Promise<MasterTurns> {
  return invoke<MasterTurns>("desktop_master_turns", { ticket, sessionId, cursor });
}

const MASTER_TURNS_MAX = 1024;
const MASTER_READ_TRIES = 3;

/** Reads every turn of one master session, oldest first, by following the engine's pages.
 * Inputs: the selection ticket and the session id. Output: the turns with the ordinals the
 * engine returned; a read the engine keeps changing under it fails with resync_required. */
export async function allMasterTurns(ticket: string, sessionId: string): Promise<MasterTurn[]> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      const turns: MasterTurn[] = [];
      let cursor: unknown = null;
      for (let pages = 0; pages <= MASTER_TURNS_MAX; pages += 1) {
        const { page } = await masterTurns(ticket, sessionId, cursor);
        turns.push(...page.turns);
        if (page.next === null) return turns;
        cursor = page.next;
      }
      throw { code: "projection_limit" };
    } catch (failure) {
      const resync = typeof failure === "object" && failure !== null && "code" in failure && failure.code === "resync_required";
      if (!resync || attempt >= MASTER_READ_TRIES) throw failure;
    }
  }
}

export function openMaster(ticket: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_open_master", { ticket, generation });
}

export function closeMaster(ticket: string, sessionId: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_close_master", { ticket, sessionId, generation });
}

export function cancelMasterTurn(ticket: string, session: string, turn: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_cancel_master_turn", { ticket, session, turn, generation });
}

export function submitMaster(ticket: string, sessionId: string, text: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_submit_master", { ticket, sessionId, text, generation });
}

export type AccountProvider = "chatgpt" | "claude" | "openrouter" | "openai_api" | "anthropic_api" | "openai_compatible" | "local";

export type SignInMethod = "subscription" | "api_key" | "url";

export type AccountField = "hidden" | "optional" | "required";

export type AccountStatus = "signed_in" | "not_signed_in" | "key_saved" | "unreachable" | "signing_in";

export interface SignInMethodEntry {
  method: SignInMethod;
  label: string;
  url: AccountField;
  url_preset: string | null;
  key: AccountField;
  model: AccountField;
}

export interface ProviderEntry {
  provider: AccountProvider;
  name: string;
  methods: SignInMethodEntry[];
}

export interface AccountDraft {
  provider: AccountProvider;
  method: SignInMethod;
  name: string;
  url?: string | null;
  model?: string | null;
}

export interface AccountView {
  id: string;
  name: string;
  provider: AccountProvider;
  provider_name: string;
  method: SignInMethod;
  method_label: string;
  url: string | null;
  model: string | null;
  status: AccountStatus;
  status_label: string;
}

export interface AccountPage {
  accounts: AccountView[];
  next: string | null;
}

export interface AccountCheck {
  account: AccountView;
  models: string[];
}

/** Reads the provider catalog the add-account menu renders: providers in display order,
 * each with its plain sign-in methods. Output: the catalog from the engine. */
export function accountCatalog(): Promise<ProviderEntry[]> {
  return invoke<ProviderEntry[]>("desktop_account_catalog");
}

/** Reads one page of accounts with plain statuses. Inputs: the previous page's `next`
 * (or null) and a page size of 1 to 200. Output: accounts and the next cursor. */
export function listAccounts(after: string | null = null, limit = 200): Promise<AccountPage> {
  return invoke<AccountPage>("desktop_list_accounts", { after, limit });
}

/** Adds one account from a catalog choice. The key, when given, goes to the engine once
 * and is never kept by the page. Output: the new account. */
export function addAccount(draft: AccountDraft, key?: string): Promise<AccountView> {
  return invoke<AccountView>("desktop_add_account", { draft, key: key || null });
}

export function renameAccount(id: string, name: string): Promise<AccountView> {
  return invoke<AccountView>("desktop_rename_account", { id, name });
}

export function removeAccount(id: string): Promise<void> {
  return invoke<void>("desktop_remove_account", { id });
}

export function checkAccount(id: string): Promise<AccountCheck> {
  return invoke<AccountCheck>("desktop_check_account", { id });
}

export function localModels(url: string): Promise<string[]> {
  return invoke<string[]>("desktop_local_models", { url });
}
