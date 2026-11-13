import { invoke as native, type Channel, type InvokeArgs } from "@tauri-apps/api/core";
import type { AgentPolicy, PolicyView } from "./api-projects";
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

const unconfirmedWatchers = new Set<(settled: SettledCall) => void>();
const PROFILE_UNCONFIRMED_CODES = new Set(["account_profile_outcome_unknown_read_same_request", "account_profile_recovery_required"]);

function failureCode(failure: unknown): string | null {
  return typeof failure === "object" && failure !== null && "code" in failure && typeof failure.code === "string" ? failure.code : null;
}

/** Whether a bridge reply leaves an account set-up for agents unconfirmed: the set-up or its
 * read timed out, a change was refused until it is checked, or a read of it is still
 * unanswered. Input: the settled call. Output: true when the app should read it again. */
export function leavesProfileUnconfirmed(settled: SettledCall): boolean {
  if (settled.ok) {
    const value = settled.value;
    return typeof value === "object" && value !== null && !Array.isArray(value) && "ticket" in value && "reply" in value &&
      "pending" in value && value.pending === true;
  }
  const code = failureCode(settled.failure);
  return code !== null && PROFILE_UNCONFIRMED_CODES.has(code);
}

/** Whether a bridge reply leaves an owner change unconfirmed: a reply the bridge still marks
 * pending, or a refusal because an earlier change is still pending.
 * Input: the settled call. Output: true when the app should check the kept change. */
export function leavesUnconfirmed(settled: SettledCall): boolean {
  if (settled.ok) {
    const value = settled.value;
    return typeof value === "object" && value !== null && !Array.isArray(value) && "pending" in value && value.pending === true;
  }
  return failureCode(settled.failure) === "pending_intent" || leavesProfileUnconfirmed(settled);
}

/** Subscribes to bridge replies that leave an owner change unconfirmed, so the app can name
 * the change and check it. Input: the listener, which gets the reply. Output: a function
 * that unsubscribes it. */
export function watchUnconfirmed(listener: (settled: SettledCall) => void): () => void {
  unconfirmedWatchers.add(listener);
  return () => { unconfirmedWatchers.delete(listener); };
}

/** Tells every watcher when one settled bridge call left a change unconfirmed.
 * Input: the settled call. Output: none. */
export function reportReply(settled: SettledCall): void {
  if (!leavesUnconfirmed(settled)) return;
  for (const listener of [...unconfirmedWatchers]) listener(settled);
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
  stuck_sign_in: string | null;
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

export type TeamUse = { kind: "available" } | { kind: "master_only"; reason: string } | { kind: "unavailable"; reason: string };

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
    linux_limited_network_off: "The Linux limited-network sockets setting is 'Do not start limited-network agents', so this master does not start. Pick another Linux limited-network sockets option in Settings.",
    linux_resolver_needs_udp: "This harness cannot look up names over TCP, which the Linux setting 'TCP only' needs. Choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
    linux_harness_unchecked: "This harness starts through a script CrazyAgent cannot check for the Linux setting 'TCP only', so it does not start. Start it from a program, or a script whose first line names a program by its full path, or choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
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
    skill_description_hidden: "The description in SKILL.md has characters you cannot see, so agents could read something you did not approve.",
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
    skill_import_too_large: "A skill brought in from your skills folders can be up to 2 MB.",
    skill_import_name_changed: "This folder now holds a different skill. Look for your skills again.",
    skill_import_home_unknown: "Your home folder is unknown, so your skills can't be found.",
    server_name_invalid: "Use 1 to 64 lowercase letters, digits, - or _ for the name.",
    server_name_taken: "An MCP server with this name already exists here.",
    server_limit: "You can add up to 50 MCP servers here.",
    server_not_found: "This MCP server was removed.",
    server_command_invalid: "Enter the full path to the command, starting with /.",
    server_args_invalid: "Use up to 32 arguments, each on one line, with at most 4 KB for the command and its arguments together.",
    server_url_invalid: "Use an https address. Plain http works only for this Mac, and the address can't hold a password.",
    server_secret_name_invalid: "Use letters, digits, _ or - for the secret name, starting with a letter.",
    server_secret_limit: "A server can have up to 16 secrets.",
    server_secret_duplicate: "Each secret needs its own name.",
    server_secret_not_found: "This secret was removed.",
    server_secret_invalid: "Enter the secret on one line, up to 8192 characters.",
    server_secret_too_short: "Use a secret of at least 8 characters. A shorter one is part of an agent's launch command, so CrazyAgent cannot keep it hidden.",
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
    hooks_none_found: "This folder has no Claude Code hooks.",
    hooks_invalid: "The hooks file in this folder is not valid hooks JSON.",
    hooks_too_many: "This plugin has more than 64 hooks.",
    hooks_too_large: "The files these hooks run are larger than 16 MB or 2048 files.",
    hook_plugin_name_invalid: "The plugin name must be 1 to 32 letters, digits, ., - or _.",
    hooks_plugin_manifest_missing: "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.",
    hooks_already_added: "These hooks were already added here.",
    hook_limit: "There are already 100 hooks here.",
    tool_unknown: "That tool is not on this server. Check the server again to list its tools.",
    tool_delete_builtin_only: "Only a CrazyAgent tool can be deleted. Turn an MCP server's tool off instead.",
    coord_tool_invalid: "This tool's name has characters a switch can't hold. Only letters, digits, _, - and . work.",
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
    launch_secret_in_command: "The agent could not start: one of this project's MCP secrets is part of its launch command, so it cannot be kept hidden. Replace that secret in Plugins.",
    launch_context_skills_kept: "The agent could not start: an earlier run locked skills into its Codex home, so CrazyAgent could not remove them and Codex would have loaded them.",
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

/** The projects Home, the sidebar, Go to and the name checks list: at most the bridge's
 * PROJECT_LIST_MAX in engine order, and whether the engine holds more. */
export interface ProjectList {
  items: ProjectPage["items"];
  more: boolean;
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
  kind: "message" | "brief" | "approve_baseline" | "revoke_baseline" | "model_access" | "create_project" | "setup_project" | "research_memory" | "project_documents" | "project_context_delete" | "project_read_paths" | "project_hidden_paths" | "project_agent_policy" | "account_schedule" | "memory_review" | "memory_add" | "memory_promote" | "memory_delete" | "memory_approve" | "memory_agent_writes" | "memory_agent_scope" | "memory_move" | "memory_forget" | "memory_remember" | "memory_selection" | "master" | "owner_profile" | "sandbox_settings" | "cleanup_settings" | "quit_settings" | "master_step_settings" | "master_time_settings" | "master_frame_settings" | "learning_mark" | "projects_home" | "move_projects_home" | "add_existing_folder" | "start_onboarding" | "run_setup" | "install_dependency" | "pack_set" | "pack_remove" | "team" | "project_setup_update" | "project_remove" | "project_delete" | "create_demo" | "project_move_home" | "research_cancel" | "research_retry" | "plan_roadmap";
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

/** Reads every project in one bridge call that pages with its own cursor, so overlapping
 * reads never disturb each other. Input: none. Output: the projects, at most the bound. */
export function projectList(): Promise<ProjectList> {
  return invoke<ProjectList>("desktop_project_list");
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
export type JobKind = ResearchKind | "roadmap_planning";

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
  research: JobKind[];
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
  agent_access?: { access: "sandboxed" | "own_branch" | "full"; label: string; unavailable: string | null }[];
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
  kind: JobKind;
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
  items: { path: string; kind: "brief" | "document" | "research" | "plan"; bytes: number }[];
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

/** One decision the project's IDE asked the owner to confirm: the memory at the asked
 * version and whether to approve or reject it. */
export type IdeAsk = MemoryItem & { decision: "approve" | "reject" };

export function ideAsks(id: string): Promise<IdeAsk[]> {
  return invoke<IdeAsk[]>("desktop_ide_asks", { id });
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
  origin: "written" | "promoted" | "selected" | "imported" | "agent" | "requested";
  from_project_id: string | null;
  source_path: string | null;
  agent: string | null;
};

export type AgentMemoryWrites = "save" | "ask";

export type AgentMemoryScope = "project" | "agent";

export type SavedMemoryPage = {
  items: SavedMemory[];
  next: number | null;
  agent_writes: AgentMemoryWrites;
  agent_scope: AgentMemoryScope;
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

export function forgetMemory(project: string, memory: string, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_forget_memory", { project, memory, version, generation });
}

export function deleteProjectContext(id: string, path: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_delete_project_context", { id, path, generation });
}

export function projectReadPaths(id: string): Promise<{ paths: string[]; version: number }> {
  return invoke<{ paths: string[]; version: number }>("desktop_project_read_paths", { id });
}

export function setProjectReadPaths(id: string, paths: string[], version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_set_project_read_paths", { id, version, paths, generation });
}

export function projectAgentPolicy(id: string): Promise<PolicyView> {
  return invoke<PolicyView>("desktop_project_agent_policy", { id });
}

export function setProjectAgentPolicy(id: string, policy: AgentPolicy, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_set_project_agent_policy", { id, policy, generation });
}

export type TeamRole = "master" | "sub" | "teammate";

/** What the screens show in place of receipts for an agent whose launch fires no hooks
 * (DEC-104 bare-hooks), instead of zeros or a blank. */
export const HOOKLESS_WORDS = "unavailable for this account kind";
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
  finished?: boolean;
  wakeable?: boolean;
  wake_note?: string | null;
  parent?: number | null;
  owner_stopped?: boolean;
  teammates?: Teammate[];
  pane?: string | null;
  hookless?: boolean;
  message_note?: string | null;
}

/** One teammate a harness split off in its own pane of the harness's terminal: its pane id,
 * whether its pane closed, whether the owner or the teammate cap stopped it, and when the
 * engine first saw it. */
export interface Teammate {
  pane: string;
  ended: boolean;
  stopped: boolean;
  seen_ms: number;
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
  parent?: number | null;
  wait?: string | null;
}

export interface TeamMessage {
  id: number;
  author: TeamMessageAuthor;
  text: string;
  state: TeamMessageState;
  created_ms: number;
}

/** What the live master does with the conversation when the master harness changes: carry it
 * over to the new harness, or start the new harness fresh. */
export type SwitchMode = "carry" | "fresh";
export type MasterIdleStop = "off" | "fifteen_minutes" | "one_hour";

/** When a saved master harness change reaches the running live master: once it is idle, or at
 * its next start. */
export type SwitchApply = "when_idle" | "next_start";

/** What a task does at its turn end while commands its agent started in the background
 * still run: wait for them, give them a grace time in seconds and stop them, or stop them
 * with the turn and tell the agent first. */
export type BackgroundAtEnd = { mode: "wait" } | { mode: "grace"; secs: number } | { mode: "stop" };

/** What the engine does with processes an agent leaves running after its stop: end them after
 * a short grace, keep them running and let the account go, or hold the account and ask. */
export type LeftoverMode = "end" | "keep" | "ask";

/** One hold of processes an agent left running, as the team view lists it: its engine key,
 * who left it, the processes it names, where it stands, when it began in Unix milliseconds
 * and whether it still keeps an account. */
export type LeftoverHold = {
  key: number;
  who: string;
  names: string;
  state: "held" | "ending" | "kept";
  since_ms: number;
  holds_account: boolean;
};

/** Where a lead's worker lands its change set: on the lead's branch when the lead accepts it,
 * or only when the owner accepts it. */
export type ChildLanding = "lead" | "owner";

/** Where a project's new sub-agents commit: each on its own branch, or a detached HEAD. */
export type BranchMode = "named" | "detached";

/** What a Full-access master does with its own conflict fixes: merge them into main itself,
 * or leave them waiting for the owner's Accept. */
export type MasterLands = "merges" | "waits";
/** Whether a project's agents may merge the landed main, `crazyagent/main`, into their own branch. */
export type CatchUpMain = "on" | "off";
/** Whether an idle agent of the same lead may take a project's next task, or each new task starts a new agent. */
export type ReuseIdle = "on" | "off";
/** How an accepted change set lands on the owner's branch: one squash commit, or a merge commit keeping every agent commit. */
export type LandingMode = "squash" | "merge";

/** A harness kind the coordination settings are kept for, by its catalog id. */
export type CoordHarness = "claude-code" | "codex" | "rcli";
/** How a lead's sub-agents start: app-spawned processes, or the harness's native teammates. */
export type SubagentMode = "app_spawned" | "native";
/** How messages reach an agent: native channels with typing as fallback, native only, or typing only. */
export type Delivery = "auto" | "native_only" | "typing_only";
/** Where a sub-agent's questions for its lead go: to the lead, or straight to the owner. */
export type QuestionsTo = "lead" | "me";
/** The Overlapping edits choices the engine knows, in the order they are offered: warn both,
 * block the edit, nothing, or wait for the file. A new engine choice is added here and in the
 * engine together. */
export const OVERLAP_CHOICES = ["warn", "block", "off", "wait"] as const;
/** Why Claude Code on an API key or URL account gets no hooks, no Block or Wait, as the
 * engine's `coord::settings::BARE_NO_HOOKS` says it (DEC-141 PA-12). */
export const BARE_NO_HOOKS = "Claude Code on an API key or URL account starts without hooks, so it runs none of your hooks and Block and Wait cannot hold its edits.";
/** Why an rcli master gets no hooks, as the engine's `RCLI_NO_HOOKS` says it. */
export const RCLI_NO_HOOKS = "rcli has no hooks, so it runs none of yours and Block and Wait cannot hold its edits.";
/** Which owner hooks a Codex agent runs, as the engine's `CODEX_HOOK_EVENTS_ONLY` says it. */
export const CODEX_HOOK_EVENTS_ONLY = "Codex agents run your hooks on the events Codex has.";
/** What happens when two agents claim the same files: one of `OVERLAP_CHOICES`. */
export type Overlaps = (typeof OVERLAP_CHOICES)[number];
/** When workflow nudges remind an agent: never, with the engine's note per flag after ten
 * minutes of work, or with one hook reminder on its first edit before any code-graph query. */
export type Nudges = "off" | "ten_minutes" | "first_edit";

/** A team role whose skills the owner chooses: the master, a lead or a worker. */
export type SkillRole = "master" | "lead" | "worker";

/** Which approved skills each team role gets: null is All approved skills, a list is Chosen
 * skills, and an empty list gives that role no skills. */
export interface RoleSkills {
  master: string[] | null;
  lead: string[] | null;
  worker: string[] | null;
}

/** How every agent terminal of a project draws, whatever harness runs in it (DEC-141): Classic
 * on the normal screen, so the app's view scrolls with the wheel and a plain drag selects, or
 * Fullscreen, the harness's own view, which takes the mouse. */
export type TerminalDisplay = "classic" | "fullscreen";

/** Whether the first turn of each task of a team agent carries the Task-start reminder: the
 * context-mode tools to load and the approved skills to check for the task. */
export type TaskStart = "on" | "off";

/** How many heavy commands of a project run at once through its heavy lane (F28): 1, 2, 3 or
 * 4 at a time, or No limit. */
export type HeavyLane = "one" | "two" | "three" | "four" | "no_limit";

/** A named thing a project's heavy commands share, such as a local model or a database, and
 * how many commands may use it at once, 1 to 8. */
export interface SharedResource {
  name: string;
  slots: number;
}

/** A project's coordination settings: question routing, claim overlaps, workflow nudges, the
 * tool names the adoption counts include, the skills each team role gets, the terminal
 * display of every harness, the Task-start reminder, the heavy lane size and the shared
 * resources. */
export interface CoordSettings {
  questions_to: QuestionsTo;
  overlaps: Overlaps;
  nudges: Nudges;
  counted_tools: string[];
  role_skills: RoleSkills;
  terminal_display: TerminalDisplay;
  task_start: TaskStart;
  heavy_lane: HeavyLane;
  shared_resources: SharedResource[];
}

/** One heavy command in a project's lane: the agent that runs it, its label (never its
 * arguments), the shared resources it uses and when it started or began to wait. */
export interface LaneEntry {
  agent: number;
  label: string;
  uses: string[];
  since_ms: number;
}

/** A project's heavy lane at one moment: the commands that run and those that wait, each in
 * turn order. */
export interface LaneView {
  running: LaneEntry[];
  waiting: LaneEntry[];
}

/** One harness kind's sub-agent mode and delivery, whether it has native teammates at all, and
 * the terminal displays its terminals can draw; an engine from before DEC-141 sends none. */
export interface HarnessCoordination {
  harness: CoordHarness;
  mode: SubagentMode;
  delivery: Delivery;
  native: boolean;
  displays?: TerminalDisplay[];
}

/** A project's coordination settings with each harness kind's mode and delivery and its
 * heavy lane; an engine from before F28 sends no lane. */
export interface CoordinationView {
  settings: CoordSettings;
  harnesses: HarnessCoordination[];
  lane?: LaneView;
}

export interface SessionSwitch {
  harness: string;
  mode: SwitchMode;
  note: string;
}

export interface TeamStatus {
  project_id: string;
  keep_warm: boolean;
  turn_limit_secs?: number | null;
  session_limit_secs?: number | null;
  idle_stop_secs?: number | null;
  on_harness_switch?: SwitchMode;
  apply_harness_switch?: SwitchApply;
  sub_agent_depth?: number;
  child_landing?: ChildLanding;
  show_finished?: boolean;
  agent_branches?: BranchMode;
  master_branch?: string | null;
  land_on?: string | null;
  background_at_end?: BackgroundAtEnd;
  leftovers?: LeftoverMode;
  leftover_holds?: LeftoverHold[];
  ask_before_merge?: boolean;
  master_lands?: MasterLands | null;
  access_unreadable?: boolean;
  reask_empty_reply?: boolean;
  done_quiet_secs?: number | null;
  master_idle_stop?: MasterIdleStop;
  master_live_terminal?: boolean;
  catch_up_main?: CatchUpMain;
  reuse_idle?: ReuseIdle;
  landing_mode?: LandingMode;
  new_folder_files_max?: number;
  coordination?: CoordinationView | null;
  message_settings?: MessageSettings;
  partial?: boolean;
  live_switch?: SessionSwitch | null;
  paused: boolean;
  max_harnesses?: number | null;
  max_teammates?: number | null;
  master_rounds?: number | null;
  rounds_used?: number;
  rounds_kept_going?: boolean;
  master_run_secs?: number | null;
  master_deadline_ms?: number | null;
  token_budget?: number | null;
  budget_used?: number | null;
  budget_reached?: boolean;
  spread_accounts?: boolean;
  terminals_per_window?: 4 | 6 | 9;
  live_master?: boolean | null;
  live_problem?: string | null;
  live_idle_stopped?: boolean;
  settings_version?: number;
  min_agents: number;
  max_agents: number;
  harnesses: TeamHarness[];
  agents: TeamAgent[];
  tasks: TeamTask[];
  messages: TeamMessage[];
  plan: TeamPlanStep[];
  events: TeamEventsView;
}

/** How a finished or failed task's result shows in a Messages timeline: its first line until
 * the owner opens it, or the whole result. */
export type ResultsShown = "first_line" | "full";

/** A project's Messages settings, stored whole: whether the master's messages and answers to
 * its sub-agents are kept, how many kept messages the project holds, whether progress events
 * and the owner's own tasks and prompts show, how results show and the page size. */
export interface MessageSettings {
  keep_master_messages: boolean;
  kept_per_project: number;
  progress_shown: boolean;
  results: ResultsShown;
  owner_items_shown: boolean;
  page_size: number;
}

/** Who a Messages entry comes from, as the engine stored it, never as its text says. */
export type MessageSpeaker = "master" | "owner" | "agent" | "engine";

/** What one Messages entry is. */
export type MessageKind = "task" | "wake" | "message" | "answer" | "prompt" | "question" | "progress" | "change_set_ready" | "finished" | "failed";

/** Which store a Messages entry comes from; with its time and id it orders the timeline. */
export type MessageSource = "task" | "message" | "event";

/** Where a Messages page ends; the next page holds the entries before it. */
export interface MessageCursor {
  at_ms: number;
  source: MessageSource;
  id: number;
}

/** One entry of a sub-agent's Messages timeline. `text` is null for a master's message the
 * project kept no text of; `result` is a finished task's result or a failed task's problem. */
export interface AgentMessage extends MessageCursor {
  from: MessageSpeaker;
  kind: MessageKind;
  text: string | null;
  task: number | null;
  state: TeamEventState | null;
  result: string | null;
  result_more: boolean;
}

/** One page of a sub-agent's Messages timeline, newest first, with whether older entries
 * remain and the settings it was read under. */
export interface AgentMessages {
  project_id: string;
  agent: number;
  entries: AgentMessage[];
  older: boolean;
  settings: MessageSettings;
}

export type TeamEventKind = "question" | "finished" | "failed" | "change_set_ready" | "progress";
export type TeamEventState = "queued" | "delivered" | "muted" | "carried" | "dropped" | "settled";
export type WakeRuleLevel = "shipped" | "app" | "project" | "agent";

export interface TeamEvent {
  id: number;
  agent: number;
  label: string;
  task: number | null;
  kind: TeamEventKind;
  text: string;
  state: TeamEventState;
  created_ms: number;
  delivered_ms: number | null;
}

export interface WakeRule {
  question: boolean;
  finished: boolean;
  failed: boolean;
  change_set_ready: boolean;
  progress: boolean;
  hourly_cap: number;
}

export type WakeRuleTarget =
  | { level: "app" }
  | { level: "project"; project_id: string }
  | { level: "agent"; project_id: string; agent: number };

export interface TeamNotification {
  id: number;
  project_id: string;
  kind: "escalation" | "wake_paused" | "queue_full" | "budget_stopped" | "urgent_question" | "question" | "waiting_on_you";
  text: string;
  created_ms: number;
  seen_ms: number | null;
}

/** How urgent a question to the owner is (Q14b): high pauses the asker, normal makes it wait
 * for the answer and low lets it carry on with the assumption it stated. */
export type QuestionSeverity = "high" | "normal" | "low";

/** One open question an agent put to the owner: who asked, how urgent it is, whom the asker
 * meant it for and that party's name (Me for the owner), the question, what the asker assumes
 * until answered, when it was asked and the asker's terminal while it runs. */
export interface OwnerQuestion {
  id: number;
  agent: number;
  label: string;
  severity: QuestionSeverity;
  asked_for: "me" | "master" | "lead";
  for_label: string;
  text: string;
  assumption: string | null;
  created_ms: number;
  terminal: string | null;
}

/** One running agent that waits on the owner in its own terminal (Q14): a permission prompt
 * or a question asked there, which only that terminal can answer. */
export interface WaitingAgent {
  agent: number;
  label: string;
  waiting: "question" | "permission";
  since_ms: number;
  terminal: string | null;
}

/** One project's Questions inbox: open questions to the owner, most urgent and then oldest
 * first, how many more are open than it lists, and the agents waiting in their terminal. */
export interface QuestionsInbox {
  project_id: string;
  questions: OwnerQuestion[];
  more: number;
  waiting: WaitingAgent[];
}

export interface TeamEventsView {
  recent: TeamEvent[];
  waiting: { agent: number; text: string }[];
  notifications: TeamNotification[];
  paused: boolean;
  questions: { agent: number; count: number }[];
  wake: {
    app: WakeRule | null;
    app_version: number;
    project: WakeRule | null;
    project_version: number;
    agents: { agent: number; rule: WakeRule; version: number }[];
    cleared: { agent: number; version: number }[];
    effective: WakeRule;
    level: WakeRuleLevel;
  };
}

export interface AppTeamEvents {
  rule: WakeRule | null;
  rule_version: number;
  effective: WakeRule;
  notifications: TeamNotification[];
  unseen: number;
  projects: { project_id: string; unseen: number; questions?: number }[];
  working?: { project_id: string; agents: number }[];
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

/** Turns keeping the team's minimum sub-agents running on or off. Inputs: the project id, the
 * switch, the team settings version the screen read and a generation. Output: the engine
 * receipt; a save from an older read is rejected as `stale_revision`. */
export function teamKeepWarm(id: string, on: boolean, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_keep_warm", { id, on, version, generation });
}

/** Sets the team's task, session and idle stop time limits. Inputs: the project id, the three
 * limits in seconds or null for no limit, the team settings version the screen read and a
 * generation. Output: the engine receipt. */
export function teamTimeLimits(id: string, turnSecs: number | null, sessionSecs: number | null, idleSecs: number | null, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_time_limits", { id, turnSecs, sessionSecs, idleSecs, version, generation });
}

/** Sets what the live master does with the conversation when the master harness changes.
 * Inputs: the project id, the mode, the team settings version the screen read and a
 * generation. Output: the engine receipt. */
export function teamOnHarnessSwitch(id: string, mode: SwitchMode, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_on_harness_switch", { id, mode, version, generation });
}

/** Sets when a saved master harness change reaches the running live master. Inputs: the project
 * id, the choice, the team settings version the screen read and a generation. Output: the
 * engine receipt; a save from an older read is rejected as `stale_revision`. */
export function teamApplyHarnessSwitch(id: string, mode: SwitchApply, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_apply_harness_switch", { id, mode, version, generation });
}

/** Sets how deep sub-agents may start workers of their own. Inputs: the project id, the depth
 * 1, 2 or 3 and a generation. Output: the engine receipt. */
export function teamSubAgentDepth(id: string, depth: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_sub_agent_depth", { id, depth, generation });
}

/** Sets where a lead's worker lands its change set. Inputs: the project id, the mode and a
 * generation. Output: the engine receipt. */
export function teamChildLanding(id: string, mode: ChildLanding, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_child_landing", { id, mode, generation });
}

/** Sets where the project's finished agents show. Inputs: the project id, true for Show (the
 * agents list and team graph) or false for Hide (the Finished fold-out only), the team
 * settings version the screen read and a generation. */
export function teamFinishedAgents(id: string, show: boolean, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_finished_agents", { id, show, version, generation });
}

/** Turns the project's live master on or off. Inputs: the project id, the switch, the team
 * settings version the screen read and a generation. Output: the commit receipt; a master
 * with no terminal app is rejected. */
export function teamLiveMaster(id: string, on: boolean, version: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_live_master", { id, on, version, generation });
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

/** Wakes a finished sub-agent by its team id; the team runtime brings the same agent back in
 * its own worktree and session. Inputs: project id, team agent id and a generation. Output: the
 * commit receipt; an agent that cannot wake is rejected with a plain code. */
export function teamWakeAgent(id: string, agent: number, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_team_wake_agent", { id, agent, generation });
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

/** Moves or copies a project kept where it is into the projects home; the engine waits for its
 * team to stop first. Inputs: the project id, move or copy, the soft refusals the owner chose to
 * go past and a generation. Output: the engine receipt. */
export function moveProjectHome(id: string, mode: AddFolderMode, overrides: AddOverride[], generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_move_project_home", { id, mode, overrides, generation });
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

/** Queues the project's roadmap and release process planning job now.
 * Inputs: project id and a generation. Output: the engine receipt. */
export function planRoadmap(id: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_plan_roadmap", { id, generation });
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

/** What the native bridge read about a picked folder: its bytes, or null with the size code when
 * it could not be measured, how many of its files are only in iCloud, its GitHub repository, its linked worktrees and their folders, whether
 * the engine lets it move and be copied without an override and which git link a Move breaks,
 * whether it has git, and whether it shares a disk with the projects folder (null when that is
 * unknown). */
export interface FolderSize {
  bytes: number | null;
  size_problem?: string | null;
  placeholders?: number;
  github?: string | null;
  worktrees?: number;
  worktree_paths?: string[];
  movable?: boolean;
  git_link?: GitLink;
  copyable?: boolean;
  git?: boolean;
  same_volume?: boolean | null;
}

/** Which git link outside a folder a Move breaks, as the engine reads it. */
export type GitLink = "none" | "worktrees" | "linked_worktree" | "submodule" | "unreadable";

export type AddFolderMode = "move" | "copy";

/** A soft refusal of one add that the owner was shown and chose to go past for that add only. */
export type AddOverride = "worktrees" | "linked_checkout" | "too_large";

export type OnboardingStep = { kind: "waiting"; until_ms?: number } | { kind: "running" } | { kind: "done" } | { kind: "off" } | { kind: "not_installed" } | { kind: "failed"; reason: string };

/** One kind of research the project's own documents already hold, read from their text:
 * the kind and the files below the project folder that hold it. A setup mapped by an engine
 * up to 2.2.0 may hold a roadmap planning group, whose files are roadmap files. */
export interface FoundResearch {
  kind: JobKind;
  files: string[];
}

export interface MappingFacts {
  languages: string[];
  build_commands: string[];
  test_commands: string[];
  entry_points: string[];
  research?: FoundResearch[];
  documents?: string[];
  roadmap?: string[];
}

/** One instruction file of a project (CLAUDE.md, .claude/CLAUDE.md, AGENTS.md or
 * CLAUDE.local.md): its path inside the project with `/`, its size, whether git tracks it (so
 * moving it shows as a deletion), the plain code of why it stays in place, null when it is
 * or was imported, and whether its move still waits for a commit (DEC-106). */
export interface ImportFile {
  path: string;
  bytes: number;
  tracked: boolean;
  refused: string | null;
  uncommitted?: boolean;
}

/** The local commit that recorded an instruction import's move: its id, the branch it is on
 * and the moved paths whose deletion it holds; never pushed (DEC-106 addendum). */
export interface ImportCommit {
  commit: string;
  branch: string;
  files: string[];
}

/** The moved instruction files whose move the checkout has not committed, the branch it has
 * checked out, null for a detached HEAD or when no move waits (DEC-106 addendum), and that
 * branch whether or not a move waits, for the project head, null for a detached HEAD. */
export interface UncommittedImport {
  files: string[];
  branch: string | null;
  checked_out: string | null;
}

/** The try a running mapping step is on: its number of `tries`, when it started in ms since the
 * epoch and the plain reason the try before it failed. */
export interface MappingTry {
  number: number;
  tries: number;
  started_ms: number;
  earlier: string | null;
}

export interface OnboardingView {
  project_id: string;
  graph: OnboardingStep;
  mapping: OnboardingStep;
  mapping_try?: MappingTry;
  name: string;
  draft: ProjectSetup;
  facts: MappingFacts;
  missing: string[];
  overview: string | null;
  folder_overview: string | null;
  finished: boolean;
  imported?: ImportFile[];
  proposed?: ProjectSetup | null;
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
  refused: string | null;
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

export type DependencyName = "tmux" | "uv" | "graphify" | "node";

export type DependencyState =
  | { state: "checking" }
  | { state: "missing" }
  | { state: "found"; version: string }
  | { state: "installing" }
  | { state: "installed"; version: string }
  | { state: "bundled"; version: string }
  | { state: "failed"; reason: string }
  | { state: "unsupported" };

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
  hooks_refused?: string | null;
}

export interface SetupStatus {
  dependencies: Dependency[];
  pack: PackEntry[];
  running: boolean;
  notes?: string[];
  version: number;
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

/** How far the running iCloud download has come: the files only in iCloud it found so far and how
 * many of them are on this Mac now. */
export interface IcloudProgress {
  total: number;
  done: number;
}

/** Downloads every file of a folder that is only in iCloud and waits until all are on this Mac;
 * the folder itself is not moved. Input: an absolute folder path. Output: how many it downloaded. */
export function icloudDownload(path: string): Promise<number> {
  return invoke<number>("desktop_icloud_download", { path });
}

/** Reads the running iCloud download's progress. Output: the counts, or null when none runs. */
export function icloudProgress(): Promise<IcloudProgress | null> {
  return invoke<IcloudProgress | null>("desktop_icloud_progress");
}

/** Stops the running iCloud download. Output: whether one was running. */
export function icloudCancel(): Promise<boolean> {
  return invoke<boolean>("desktop_icloud_cancel");
}

/** Measures a folder before Copy and names its GitHub repository, if any.
 * Input: an absolute folder path. Output: its size and GitHub owner/name. */
export function folderSize(path: string): Promise<FolderSize> {
  return invoke<FolderSize>("desktop_folder_size", { path });
}

/** Moves or copies an existing folder into the projects home.
 * Inputs: the source folder, move or copy, the soft refusals the owner chose to go past and a
 * generation. Output: the engine receipt. */
export function addExistingFolder(source: string, mode: AddFolderMode, overrides: AddOverride[], generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_add_existing_folder", { source, mode, overrides, generation });
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

/** Installs one missing tool alone, such as Node. Inputs: its name and a generation.
 * Output: the status as the install starts. */
export function installDependency(name: DependencyName, generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_install_dependency", { name, generation });
}

/** Switches one installed pack entry on or off. Inputs: its id, the state, the pack
 * version shown and a generation. */
export function setPackEntry(id: string, on: boolean, version: number, generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_set_pack_entry", { id, on, version, generation });
}

/** Removes one installed pack entry and what it added. Inputs: its id, the pack version
 * shown and a generation. */
export function removePackEntry(id: string, version: number, generation: number): Promise<SetupStatus> {
  return invoke<SetupStatus>("desktop_remove_pack_entry", { id, version, generation });
}

/** What one node of the who-leads-whom tree stands for: the master, a harness lead the master
 * started, a sub-agent a lead started, or a native teammate a lead's harness split off. */
export type GraphNodeKind = "master" | "lead" | "sub" | "teammate";
export type GraphEdgeState = "sent" | "running" | "done" | "failed";
export type Presence = "live" | "idle" | "away" | "stopped";
export type Waiting = "question" | "permission";
export type Receipt = "sent" | "delivered" | "typed" | "read" | "replied" | "failed" | "expired";

/** One node of a project's team graph, as the engine's TeamGraph read returns it. `parent`
 * names the node that leads it; `terminal` is the name the terminal commands take, for the
 * nodes that have one. `questions` counts the questions it queued for the master, `agent` is
 * its own team agent row and `pane` a native teammate's pane, for Stop. `presence`, `waiting`
 * on the owner in its terminal, `waits_for` (the file its start waits for under Wait),
 * `owner_questions` (open For: Me questions) and `no_worktree` come from the coordination layer; `hookless` marks an agent whose launch fires no hooks, so
 * it has no receipts; `message_note` is the engine's sentence when the Delivery setting leaves
 * the agent no way to get messages, and `started_ms` when its earliest running task started,
 * null while it runs none. Older engines leave the optional fields out. */
export interface GraphNode {
  id: string;
  kind: GraphNodeKind;
  parent: string | null;
  label: string;
  harness: string;
  status: TeamAgentStatus | null;
  status_label: string | null;
  account: string | null;
  branch?: string | null;
  terminal: string | null;
  questions?: number | null;
  agent?: number | null;
  pane?: string | null;
  presence?: Presence | null;
  waiting?: Waiting | null;
  waits_for?: string | null;
  owner_questions?: number;
  no_worktree?: boolean;
  hookless?: boolean;
  message_note?: string | null;
  started_ms?: number | null;
}

/** Unread messages from one party to another: how many and the newest one's receipt. Each end
 * is a node id, "owner" or "engine". */
export interface MessageEdge {
  from: string;
  to: string;
  unread: number;
  receipt: Receipt;
}

/** Active claims of two agents that may touch the same thing: how many claim pairs, the first
 * plain reason, and whether every pair has a stale side. */
export interface OverlapEdge {
  from: string;
  to: string;
  count: number;
  reason: string;
  stale: boolean;
}

/** One assignment edge from the node leading an agent to the agent running the task, or from
 * the master to the master while no agent took it. `to` stays the agent's own node id while
 * that agent is not drawn, so its task counts for no other card; `agent_label` names it.
 * `reply` is the result or the plain problem once the task ended. */
export interface GraphEdge {
  from: string;
  to: string;
  task_id: number;
  source: "master" | "owner";
  text: string;
  state: GraphEdgeState;
  reply: string | null;
  lead?: string | null;
  agent_label?: string | null;
}

/** How the upper node of one tree pair relates to the lower one. */
export type Relation = "leads" | "delegated_to" | "teammate_of";

/** What one tree pair is doing now: a handoff names its task, cut to 160 bytes, and a review
 * the agent whose change set it reviews by its node name, cut to 96 bytes. */
export type LiveAction =
  | { action: "handing_off"; task: string }
  | { action: "messaging" }
  | { action: "waiting_on"; owner: boolean }
  | { action: "reviewing"; agent: string };

/** The one relation edge of a tree pair, from the node that leads to the node it leads,
 * with the pair's live action while one holds. */
export interface RelationEdge {
  from: string;
  to: string;
  relation: Relation;
  live: LiveAction | null;
}

/** One project's team as a tree of who leads whom, its assignment edges, the unread
 * message and claim overlap edges of the coordination layer, and one relation edge per
 * tree pair. */
export interface TeamGraph {
  project_id: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  messages?: MessageEdge[];
  overlaps?: OverlapEdge[];
  relations?: RelationEdge[];
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

/** The project whose IDE tab a TUI "open in IDE" asked to bring forward, and the reveal
 * that asked. */
export interface IdeFocus {
  project: string;
  request: string;
}

/** Takes, once, the IDE tab a TUI "open in IDE" asked the app to show, and raises the
 * window when there is one. Output: the project and reveal, or null when none is waiting. */
export function ideFocus(): Promise<IdeFocus | null> {
  return invoke<IdeFocus | null>("desktop_ide_focus");
}

/** Opens a live view of one agent's terminal. The channel gets the attach frame, the screen
 * and then live output until the terminal closes or the view detaches. Inputs: the terminal
 * name and the channel. Output: the view's serial once the engine accepted the attach. */
export function terminalAttach(agent: string, output: Channel<TerminalMessage>, pane: string | null = null): Promise<number> {
  return invoke<number>("desktop_terminal_attach", { agent, pane, output });
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

/** Closes this window's view of an agent terminal; the agent keeps running and a newer view
 * of it stays open. Inputs: the terminal name and the serial its attach returned. Output:
 * resolves once the view is gone. */
export function terminalDetach(agent: string, serial: number): Promise<void> {
  return invoke<void>("desktop_terminal_detach", { agent, serial });
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
  version: number;
  source: string;
  folder: string;
  refused: string | null;
}

export interface SkillFile {
  path: string;
  bytes: number;
}

/** One hook as Settings lists it: its plugin, Claude Code event and matcher, command, where
 * it applies, its On flags, the skill or MCP server it follows and the version a project
 * switch names. */
export interface HookView {
  id: string;
  name: string;
  plugin: string;
  event: string;
  matcher: string | null;
  command: string;
  scope: PluginScope;
  scope_label: string;
  enabled: boolean;
  on_here: boolean | null;
  follows: string | null;
  source: string;
  version: number;
}

export interface SkillDetail {
  skill: SkillView;
  text: string;
  files: SkillFile[];
  files_more: number;
}

export type ImportFrom = "claude" | "codex";

export type ImportState =
  | { kind: "new" }
  | { kind: "added"; id: string }
  | { kind: "differs"; id: string }
  | { kind: "unusable"; code: string };

export interface ImportCandidate {
  from: ImportFrom;
  folder: string;
  name: string;
  line: string;
  state: ImportState;
}

export interface ImportList {
  found: ImportCandidate[];
  more: number;
}

export interface ImportedSkill {
  skill: SkillView;
  skipped: string[];
  skipped_total: number;
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
  version: number;
}

/** A tool's switch: on, off, or deleted (hidden; built-in tools only). */
export type ToolSwitch = "on" | "off" | "hidden";

/** One tool with its switches: the CrazyAgent built-in tools, or one MCP server's tools.
 * `global` is the every-project switch; `here` is the project's own switch, null when the
 * project follows the global one or in global settings. */
export interface ToolView {
  server: string;
  server_id: string | null;
  tool: string;
  description: string;
  builtin: boolean;
  global: ToolSwitch;
  here: ToolSwitch | null;
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

/** Lists hooks for global settings (no project) or for one project's settings. */
export function pluginHooks(projectId?: string): Promise<HookView[]> {
  return invoke<HookView[]>("desktop_plugin_hooks", { projectId: projectId ?? null });
}

/** Adds the hooks a picked plugin folder defines; the engine keeps its own copy of their files. */
export function addHooks(scope: PluginScope, folder: string): Promise<HookView[]> {
  return invoke<HookView[]>("desktop_add_hooks", { scope, folder });
}

/** Turns a hook on or off where it was added. */
export function setHookEnabled(id: string, enabled: boolean): Promise<HookView> {
  return invoke<HookView>("desktop_set_hook_enabled", { id, enabled });
}

/** Turns a skill on or off where it was added, from the version the list showed. */
export function setSkillEnabled(id: string, enabled: boolean, version: number): Promise<SkillView> {
  return invoke<SkillView>("desktop_set_skill_enabled", { id, enabled, version });
}

/** Removes a skill and the engine's copy of its folder. */
export function removeSkill(id: string, version: number): Promise<void> {
  return invoke<void>("desktop_remove_skill", { id, version });
}

/** Reads one skill with its full SKILL.md text and file list, with a project's switch when given. */
export function skillDetail(id: string, projectId?: string): Promise<SkillDetail> {
  return invoke<SkillDetail>("desktop_skill_detail", { id, projectId: projectId ?? null });
}

/** Looks for skills in the owner's Claude Code and Codex skills folders, only when asked. */
export function skillImportScan(): Promise<ImportList> {
  return invoke<ImportList>("desktop_skill_import_scan", {});
}

/** Imports one found skill, turned on for every project; `replace` is the id of an added skill whose files differ. */
export function importSkill(folder: string, replace: string | null): Promise<ImportedSkill> {
  return invoke<ImportedSkill>("desktop_import_skill", { folder, replace });
}

/** Adds an MCP server that waits for approval. Secret values go to the engine and are never returned. */
export function addMcpServer(scope: PluginScope, name: string, spec: McpServerSpec, secrets: [string, string][]): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_add_mcp_server", { scope, name, spec, secrets });
}

/** Changes how an MCP server runs; it then waits for approval again. */
export function updateMcpServer(id: string, spec: McpServerSpec, version: number): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_update_mcp_server", { id, spec, version });
}

/** Approves an MCP server exactly as shown; a server that changed since is refused. */
export function approveMcpServer(id: string, spec: McpServerSpec): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_approve_mcp_server", { id, spec });
}

/** Turns an MCP server on or off where it was added, from the version the list showed. */
export function setMcpServerEnabled(id: string, enabled: boolean, version: number): Promise<McpServerView> {
  return invoke<McpServerView>("desktop_set_mcp_server_enabled", { id, enabled, version });
}

/** Removes an MCP server and its saved secrets. */
export function removeMcpServer(id: string, version: number): Promise<void> {
  return invoke<void>("desktop_remove_mcp_server", { id, version });
}

/** Switches a global skill or MCP server on or off for one project, from the version the list showed. */
export function switchPlugin(projectId: string, id: string, on: boolean, version: number): Promise<void> {
  return invoke<void>("desktop_switch_plugin", { projectId, id, on, version });
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
 * conversation's turn, ended by closing that conversation, a research job, ended by
 * cancelling it, or processes a Full access agent left running, named by `report`, which
 * signing the account out lets go of while they still run. */
export type Holder =
  | { kind: "master"; project: string; session: string }
  | { kind: "research"; project: string; job: number; title: string }
  | { kind: "leftover"; report: string };

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
  attempt?: { connection: { id: string; account_ref: string }; selected_at_ms?: number } | null;
  accounts?: MasterAccount[];
  detail?: string | null;
  cause?: string | null;
  tasks?: MasterTask[];
  changes?: string | null;
  engine_request?: MasterEngineRequest | null;
  input_cursor?: string;
  activity?: MasterActivityLine[];
}

/** One line of what a running master turn is doing, as the engine words it: a fixed phrase
 * with at most one project-relative path, and the engine's clock in milliseconds when it
 * last saw it. The engine sends the lines oldest first, only while the turn is outstanding. */
export interface MasterActivityLine {
  at_ms: number;
  text: string;
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
  session: { session_id: string; phase: MasterPhase; current_turn: string | null } | null;
}

/** Where a master session stands: live ("configured"), parked and resumable, or closed with its project. */
export type MasterPhase = "configured" | "parked" | "closing" | "closed";

/** One master session in the Sessions list. `created` and `last_activity` are engine revisions;
 * the `_ms` times are absent for sessions older than the marks. `agents` names its own sub-agents. */
export interface MasterSessionSummary {
  session_id: string;
  phase: MasterPhase;
  created: string;
  last_activity: string;
  started_ms: number | null;
  active_ms: number | null;
  turns: number;
  first_line: string | null;
  last_line: string | null;
  agents: string[];
  results_waiting: number;
  mode?: MasterMode | null;
  harness?: string | null;
}

/** Which kind of master ran a message or a session: the one the engine runs one turn at a
 * time, or the one that runs live in its terminal. */
export type MasterMode = "one_turn" | "live";

/** One stored message of a master conversation. `seq` orders a session's messages from 1.
 * `speaker` "engine" is a request the engine typed itself and "switch" the divider where
 * another master took the conversation over, whose `text` is the whole divider line.
 * `harness` is a live master's catalog id or alias, or a one-turn master's backend (`rcli`,
 * `codex`, `claude`). `text` is null for a terminal turn whose typed words were not read.
 * `source` tells where the row comes from (see `SessionMessageSource`). */
export interface SessionMessage {
  seq: number;
  speaker: "owner" | "engine" | "master" | "switch";
  mode: MasterMode;
  harness: string | null;
  text: string | null;
  at_ms: number;
  source: SessionMessageSource;
}

/** Where a stored message comes from: a turn the engine ran ("turn"), a turn in the live
 * terminal ("terminal"), a master switch ("switch") or the divider an upgrade placed
 * ("backfill"). "record" is text read from the harness's own record and "counter" a row that
 * stands for a terminal turn an older engine only counted: the engine did not see those
 * words typed, so a view marks them. */
export type SessionMessageSource = "turn" | "terminal" | "record" | "counter" | "switch" | "backfill";

/** One page of a session's stored messages, oldest first. `next` is the `before` of the page
 * of older messages, null once the page holds the session's first message; `count` is every
 * stored message of the session. */
export interface MasterMessagePage {
  project_id: string;
  session_id: string;
  messages: SessionMessage[];
  next: number | null;
  count: number;
}

export interface MasterMessages {
  ticket: string;
  revision: string;
  page: MasterMessagePage;
}

/** Reads one page of the stored messages of any session of the selected project, in either
 * master mode: the newest when `before` is null, else those with a sequence number below it.
 * Inputs: the selection ticket, the session id, the cursor and the page size, 1 to 64. */
export function masterMessages(ticket: string, sessionId: string, before: number | null, limit: number): Promise<MasterMessages> {
  return invoke<MasterMessages>("desktop_master_messages", { ticket, sessionId, before, limit });
}

export interface MasterSessions {
  ticket: string;
  revision: string;
  sessions: MasterSessionSummary[];
}

/** Lists the selected project's master sessions, the live one first, then newest activity first. */
export function masterSessions(ticket: string): Promise<MasterSessions> {
  return invoke<MasterSessions>("desktop_master_sessions", { ticket });
}

/** One owner message behind a master turn, keyed by the turn's input cursor. */
export interface OwnerLine {
  input_cursor: string;
  text: string;
}

/** The owner messages read for some turns of a session. */
export interface MasterInputs {
  ticket: string;
  revision: string;
  entries: OwnerLine[];
}

/** Reads the owner messages behind at most 32 turns of a session of the selected project. */
export function masterInputs(ticket: string, sessionId: string, cursors: string[]): Promise<MasterInputs> {
  return invoke<MasterInputs>("desktop_master_inputs", { ticket, sessionId, cursors });
}

/** Reads one page of any session of the selected project, live or parked. */
export function sessionTurns(ticket: string, sessionId: string, cursor: unknown = null): Promise<MasterTurns> {
  return invoke<MasterTurns>("desktop_master_session_turns", { ticket, sessionId, cursor });
}

/** Parks the live session and opens a fresh one in one engine action; a running turn needs `cancelRunning`. */
export function newMasterSession(ticket: string, cancelRunning: boolean, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_new_master_session", { ticket, cancelRunning, generation });
}

/** Makes a parked session live again, parking the live one; a running turn needs `cancelRunning`. */
export function resumeMasterSession(ticket: string, sessionId: string, cancelRunning: boolean, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_resume_master_session", { ticket, sessionId, cancelRunning, generation });
}

/** Deletes a session that is not live: its stored turns and its transcript in the master's private home. */
export function deleteMasterSession(ticket: string, sessionId: string, generation: number): Promise<IntentReceipt> {
  return invoke<IntentReceipt>("desktop_delete_master_session", { ticket, sessionId, generation });
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
