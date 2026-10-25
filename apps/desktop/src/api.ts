import { invoke as native, type Channel, type InvokeArgs } from "@tauri-apps/api/core";
import { errorMessage, retryBusy } from "./engine-client";
import type { AccountPool, AgentView, AccountProfileView, IntentReceipt, MemoryItem, McpServerSpec, McpServerView, ModelAccessDetail, ModelAccessEdit, ModelAccessPage, PluginScope, ProjectPage, SavedMemoryPage, SkillView, TeamGraph, TerminalMessage } from "./engine-client";

export type { SavedMemory, SavedMemoryPage } from "./engine-client";
export type { McpServerSpec, McpServerView, PluginScope, SkillView } from "./engine-client";
export type { AgentRole, AgentState, AgentView } from "./engine-client";

export type Failure = { kind: "not_ready" } | { kind: "error"; code: string; message: string };
export type Result<T> = { kind: "ok"; value: T } | Failure;

const FALLBACK = "Something went wrong. Try again.";
const UNREACHABLE = "engine_unreachable";
const PAGE_LIMIT = 200;
const ACCOUNT_PAGES_MAX = 100;

const COPY: Record<string, string> = {
  engine_unreachable: "CrazyAgent's engine isn't running.",
  uncertain: "We couldn't confirm your last change. Check it and save again.",
  pending_intent: "We couldn't confirm your last change. Check it and save again.",
  stale_revision: "This changed since you opened it. Review and save again.",
  stale_model_access: "This changed since you opened it. Review and save again.",
  stale_config: "This changed since you opened it. Review and save again.",
  settings_source_changed: "This changed since you opened it. Review and save again.",
  settings_read_only: "Settings can't be changed in this engine. Start it with writable settings.",
  busy: "Another change is still running. Wait for it to finish.",
  rejected: "The engine didn't accept this change.",
  unavailable: "This isn't available right now. Try again.",
  pool_member_limit: "A pool can hold up to 15 accounts.",
  pool_in_use: "Can't delete this pool. A project uses it. Choose another pool for it first.",
  pool_name_bounds: "Use a pool name of 1 to 60 characters on one line.",
  pool_exists: "A pool with this name already exists.",
  pool_not_found: "This pool no longer exists.",
  model_access_duplicate: "This already exists.",
  model_access_missing: "This no longer exists.",
  invalid_model_access: "The engine didn't accept this pool. Check its accounts and save again.",
  owner_profile_name: "Enter your name, up to 80 characters.",
  owner_profile_role: "Keep your role on one line, up to 80 characters.",
  owner_profile_bio: "Keep your bio under 500 characters.",
  owner_profile_stack: "List up to 12 different technologies, each up to 40 characters.",
  owner_profile_bounds: "Your profile is too long. Shorten it and save again.",
  owner_profile_invalid: "Your profile has a value the engine can't save. Check it and save again.",
  account_name_invalid: "Use an account name of up to 60 characters on one line.",
  account_key_required: "Enter an API key.",
  account_key_invalid: "This API key can't be saved. Check it and try again.",
  account_key_not_allowed: "This sign-in method doesn't use an API key.",
  account_url_required: "Enter a full URL that starts with https://, or http://127.0.0.1 for this Mac.",
  account_url_invalid: "Enter a full URL that starts with https://, or http://127.0.0.1 for this Mac.",
  account_local_url_required: "Enter a URL on this Mac, such as http://127.0.0.1:11434.",
  account_url_not_allowed: "This sign-in method doesn't use a URL.",
  account_url_unreachable: "Can't reach this URL. Check the address and that the server is running.",
  account_probe_https_unsupported: "CrazyAgent can only check http URLs on this Mac. The account is saved.",
  account_model_required: "Choose a model.",
  account_model_invalid: "Use a model name without spaces.",
  account_model_not_allowed: "This sign-in method doesn't use a model name.",
  account_models_unavailable: "Couldn't list models at this URL. Type the model name instead.",
  account_reply_invalid: "The server at this URL sent a reply CrazyAgent doesn't understand.",
  account_reply_too_large: "The server at this URL sent a reply that is too large.",
  account_method_unavailable: "This sign-in method isn't available for this provider.",
  account_identity_unavailable: "Couldn't create the account. Try again.",
  account_in_pool: "This account is in a pool. Remove it from the pool first.",
  account_not_found: "This account no longer exists.",
  native_profile_not_managed: "This account doesn't use a subscription sign-in.",
  native_auth_host_unavailable: "Sign-in isn't available on this Mac right now. Check that the harness is installed.",
  native_auth_cannot_display_safely: "The sign-in page sent something CrazyAgent can't show safely. Try again.",
  native_auth_outcome_unknown_read_same_operation: "We couldn't confirm the sign-in. Check again.",
  native_auth_claude_input_invalid: "Paste the complete code from the Claude page.",
  native_auth_claude_input_outcome_unknown: "We couldn't confirm the code was sent. Check again before sending it a second time.",
  picker_failed: "Couldn't open the folder chooser. Type the full path instead.",
  picker_unavailable: "This computer has no folder chooser CrazyAgent can use. Type the full path instead.",
  link_not_allowed: "CrazyAgent only opens web links that start with https:// or http://.",
  open_failed: "This computer couldn't open this. Try again.",
  open_unavailable: "This computer has no app set up to open this.",
  project_folder_missing: "The project folder isn't on this computer anymore.",
  context_item_not_openable: "CrazyAgent opens only documents inside the project folder.",
  rcli_private_key_writer_unavailable: "This engine can't set up accounts for agents. Start it with the CrazyAgent model runner.",
  native_profile_in_use: "An agent is using this account now. Try again after it stops.",
  account_profile_operations_full: "This engine can't set up more accounts for agents.",
  account_profile_outcome_unknown_read_same_request: "We couldn't confirm the account is ready for agents. Save the pool again to check.",
  memory_note_blank: "Write something to remember.",
  memory_note_too_long: "Keep the memory under 2000 characters.",
  memory_note_invalid: "Remove special control characters from the memory.",
  memory_duplicate: "This memory is already saved.",
  memory_full: "Memory is full. Delete an old memory first.",
  memory_missing: "That memory no longer exists.",
  memory_changed: "This memory changed. Reload it and try again.",
  memory_not_kept: "Only memories you kept can be made global.",
  memory_already_global: "This memory is already in global memory.",
  memory_project_missing: "That project no longer exists.",
  memory_search_invalid: "Keep the search on one line, up to 100 characters.",
  memory_page_invalid: "Show between 1 and 20 memories at a time.",
  knowledge_version_conflict: "This memory changed. Reload it and try again.",
  knowledge_source_revoked: "The research this came from was deleted.",
  invalid_knowledge_text: "Write the memory as text, up to 4096 bytes.",
};

export const KNOWN_ERROR_CODES = Object.keys(COPY);

/** Turns an engine error code into one plain sentence for the owner.
 * Input: an error code. Output: the copy for it, or the generic fallback. */
export function plainError(code: string): string {
  if (COPY[code]) return COPY[code];
  return /^(skill|server|plugins?)_[a-z_]+$/.test(code) ? errorMessage({ code }) : FALLBACK;
}

/** Sorts a failed bridge call into "not ready yet" (the command is not in this build) or a
 * plain error. Raw text is never shown. Input: the rejection value. Output: a failure. */
export function classifyFailure(error: unknown): Failure {
  if (typeof error === "string") {
    if (/^Command [a-z_]+ not found$/.test(error) || /^[a-z_]+ not allowed\b[\s\S]*Command not found$/.test(error)) return { kind: "not_ready" };
    return { kind: "error", code: "unknown", message: FALLBACK };
  }
  if (error instanceof TypeError || error instanceof ReferenceError) return { kind: "error", code: UNREACHABLE, message: plainError(UNREACHABLE) };
  const raw = typeof error === "object" && error !== null && "code" in error ? String(error.code) : "unknown";
  const code = ["not_connected", "not_configured", "owner_unavailable", "owner_revoked"].includes(raw) ? UNREACHABLE : raw;
  return { kind: "error", code, message: plainError(code) };
}

/** Calls one Tauri command, retrying only the bridge's pre-action "busy" refusal.
 * Inputs: the command and its arguments. Output: the value, not ready, or a plain error. */
export async function call<T>(command: string, args?: InvokeArgs): Promise<Result<T>> {
  try {
    return { kind: "ok", value: await retryBusy(() => native<T>(command, args)) };
  } catch (error) {
    return classifyFailure(error);
  }
}

/** Reads an engine receipt for an owner change: saved only when applied and settled.
 * Input: the receipt. Output: ok, the rejection as plain copy, or "uncertain". */
export function receiptResult(receipt: IntentReceipt): Result<void> {
  if (receipt.pending) return { kind: "error", code: "uncertain", message: plainError("uncertain") };
  if (receipt.outcome === "applied") return { kind: "ok", value: undefined };
  const code = receipt.rejection ?? (receipt.outcome === "rejected" ? "rejected" : "unavailable");
  return { kind: "error", code, message: plainError(code) };
}

let generation = 0;

/** Issues the next draft generation for an owner change; the bridge accepts 1 to 2^53 - 1.
 * Output: a positive, increasing safe integer. */
export function nextGeneration(): number {
  assert(generation < Number.MAX_SAFE_INTEGER, "draft generation exhausted");
  generation += 1;
  return generation;
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function change(command: string, args: InvokeArgs): Promise<Result<void>> {
  const result = await call<IntentReceipt>(command, args);
  return result.kind === "ok" ? receiptResult(result.value) : result;
}

export interface ConnectionStatus {
  connected: boolean;
  pending: boolean;
}

/** Connects this window to the engine. Output: whether it is connected and whether an earlier
 * change is still unconfirmed. */
export function connect(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_connect");
}

/** Reads the current engine connection. Output: connection and unconfirmed-change flags. */
export function status(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_status");
}

/** Asks the engine to settle an unconfirmed change. Output: ok once it is settled. */
export function checkUnconfirmed(): Promise<Result<void>> {
  return change("desktop_reconcile_intent", {});
}

export interface OwnerProfile {
  name: string;
  role: string;
  bio: string;
  preferred_stack: string[];
  working_style: { detail: "terse" | "detailed"; initiative: "ask_first" | "act_first" };
}

/** Reads the owner profile. Output: null on first run, which shows the Welcome page. */
export function getProfile(): Promise<Result<OwnerProfile | null>> {
  return call<OwnerProfile | null>("desktop_owner_profile");
}

/** Saves or updates the owner profile. Input: the full profile. Output: ok once saved. */
export function saveProfile(profile: OwnerProfile): Promise<Result<void>> {
  return change("desktop_save_owner_profile", { profile, generation: nextGeneration() });
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

export interface AccountCheck {
  account: AccountView;
  models: string[];
}

/** Reads the provider catalog behind the Add account dialog. Output: providers in display
 * order, each with its sign-in methods. */
export function accountCatalog(): Promise<Result<ProviderEntry[]>> {
  return call<ProviderEntry[]>("desktop_account_catalog");
}

/** Reads every account, following the engine's pages. Output: all accounts in engine order. */
export async function listAccounts(): Promise<Result<AccountView[]>> {
  const accounts: AccountView[] = [];
  let after: string | null = null;
  for (let page = 0; page < ACCOUNT_PAGES_MAX; page += 1) {
    const result: Result<{ accounts: AccountView[]; next: string | null }> = await call("desktop_list_accounts", { after, limit: PAGE_LIMIT });
    if (result.kind !== "ok") return result;
    accounts.push(...result.value.accounts);
    if (!result.value.next) return { kind: "ok", value: accounts };
    after = result.value.next;
  }
  return { kind: "error", code: "account_page_limit", message: "There are more accounts than this window can show." };
}

/** Adds one account. The key, when given, is sent once and never kept here.
 * Inputs: the provider and method choice and an optional key. Output: the new account. */
export function addAccount(draft: AccountDraft, key?: string): Promise<Result<AccountView>> {
  return call<AccountView>("desktop_add_account", { draft, key: key || null });
}

/** Renames one account. Inputs: account id and new name. Output: the renamed account. */
export function renameAccount(id: string, name: string): Promise<Result<AccountView>> {
  return call<AccountView>("desktop_rename_account", { id, name });
}

/** Removes one account. Input: account id. Output: ok once removed. */
export function removeAccount(id: string): Promise<Result<void>> {
  return call<void>("desktop_remove_account", { id });
}

/** Checks one URL or local account against its server. Input: account id. Output: the
 * account with its fresh status and the models the server lists. */
export function checkAccount(id: string): Promise<Result<AccountCheck>> {
  return call<AccountCheck>("desktop_check_account", { id });
}

/** Lists models served by a local model server. Input: its base URL. Output: model names. */
export function localModels(url: string): Promise<Result<string[]>> {
  return call<string[]>("desktop_local_models", { url });
}

/** Reads one saved account's sign-in record, which subscription sign-in starts from.
 * Input: account id. Output: the record with its edit ticket. */
export function signInRecord(id: string): Promise<Result<ModelAccessDetail>> {
  return call<ModelAccessDetail>("desktop_inspect_model_access", { collection: "connections", id });
}

const PROFILE_NOT_NEEDED = new Set(["account_profile_harness_unsupported", "account_profile_auth_unsupported"]);

/** Tells whether a refusal to set an account up for agents only means it needs no harness
 * profile (for example a harness that reads the key itself). Input: the error code. */
export function profileNotNeeded(code: string): boolean {
  return PROFILE_NOT_NEEDED.has(code);
}

/** Sets a saved-key, URL or local account up for agents once it is in a pool. No key is
 * sent: the engine uses the key it already holds. Input: account id. Output: ok when the
 * engine configured it or it needs no profile, otherwise a plain error. */
export async function readyForAgents(id: string): Promise<Result<void>> {
  const detail = await call<ModelAccessDetail>("desktop_inspect_model_access", { collection: "connections", id });
  if (detail.kind !== "ok") return detail;
  const configured = await call<AccountProfileView>("desktop_configure_account_profile", { ticket: detail.value.ticket, secret: null, generation: nextGeneration() });
  if (configured.kind === "error" && profileNotNeeded(configured.code)) return { kind: "ok", value: undefined };
  if (configured.kind !== "ok") return configured;
  if (configured.value.pending || configured.value.reply.outcome === "not_recorded") {
    return { kind: "error", code: "account_profile_outcome_unknown_read_same_request", message: plainError("account_profile_outcome_unknown_read_same_request") };
  }
  return { kind: "ok", value: undefined };
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

export const POOL_MEMBER_MAX = 15;

/** Reads every pool with its plain summary and member statuses. Output: the pools. */
export function poolSummaries(): Promise<Result<PoolSummary[]>> {
  return call<PoolSummary[]>("desktop_pool_summaries");
}

function poolId(): string {
  const bytes = new Uint8Array(10);
  crypto.getRandomValues(bytes);
  return "pool-" + Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Validates a pool before saving. Inputs: name and member ids. Output: the trimmed name
 * and members, or a plain error. */
export function poolInput(name: string, members: string[]): Result<{ name: string; members: string[] }> {
  const trimmed = name.trim();
  if (!trimmed) return { kind: "error", code: "pool_name_required", message: "Enter a pool name." };
  if ([...trimmed].length > 60 || /[\u0000-\u001f\u007f-\u009f]/u.test(trimmed)) return { kind: "error", code: "pool_name_bounds", message: plainError("pool_name_bounds") };
  if (new Set(members).size !== members.length) return { kind: "error", code: "pool_duplicate_member", message: "Each account can be in a pool once." };
  if (members.length > POOL_MEMBER_MAX) return { kind: "error", code: "pool_member_limit", message: plainError("pool_member_limit") };
  return { kind: "ok", value: { name: trimmed, members } };
}

/** Creates a pool, or renames and replaces the members of an existing one while keeping its
 * project assignments. Inputs: pool id (absent for a new pool), name, member ids.
 * Output: ok once the engine saved it. */
export async function savePool(id: string | undefined, name: string, members: string[]): Promise<Result<void>> {
  const input = poolInput(name, members);
  if (input.kind !== "ok") return input;
  let ticket: string;
  let edit: ModelAccessEdit;
  if (id === undefined) {
    const page = await call<ModelAccessPage>("desktop_model_access_page", { collection: "pools", next: false });
    if (page.kind !== "ok") return page;
    ticket = page.value.ticket;
    edit = { kind: "create_pool", pool: { id: poolId(), name: input.value.name, members: input.value.members, projects: [], roles: [], fallback_allowed: false } as AccountPool };
  } else {
    const detail = await call<ModelAccessDetail>("desktop_inspect_model_access", { collection: "pools", id });
    if (detail.kind !== "ok") return detail;
    if (detail.value.record.kind !== "pool") return { kind: "error", code: "pool_not_found", message: plainError("pool_not_found") };
    ticket = detail.value.ticket;
    edit = { kind: "replace_pool", pool: { ...detail.value.record.pool, name: input.value.name, members: input.value.members } as AccountPool };
  }
  return change("desktop_edit_model_access", { ticket, edit, generation: nextGeneration() });
}

/** Deletes a pool; the engine refuses while a project uses it. Input: pool id.
 * Output: ok once deleted. */
export async function deletePool(id: string): Promise<Result<void>> {
  const detail = await call<ModelAccessDetail>("desktop_inspect_model_access", { collection: "pools", id });
  if (detail.kind !== "ok") return detail;
  return change("desktop_edit_model_access", { ticket: detail.value.ticket, edit: { kind: "remove_pool", id }, generation: nextGeneration() });
}

export type HarnessAccountKind = "subscription" | "api_key" | "open_ai_compatible_url" | "local";

export interface HarnessEntry {
  id: string;
  name: string;
  status: { kind: "ready" } | { kind: "not_installed" } | { kind: "limited"; reason: string };
  status_label: string;
  executable: string | null;
  headless: string;
  tools: { kind: "switchable" } | { kind: "limited"; reason: string };
  model_selection: boolean;
  account_kinds: HarnessAccountKind[];
}

/** Reads the harness catalog with what is installed on this Mac. Output: harnesses in
 * catalog priority order. */
export function harnessCatalog(): Promise<Result<HarnessEntry[]>> {
  return call<HarnessEntry[]>("desktop_harness_catalog");
}

/** Reads skills for global settings (no project) or one project's settings, where global
 * items carry the project's own switch. Output: skills in engine order. */
export function listSkills(projectId?: string): Promise<Result<SkillView[]>> {
  return call<SkillView[]>("desktop_plugin_skills", { projectId: projectId ?? null });
}

/** Adds a skill from a folder that holds SKILL.md; the engine keeps its own copy.
 * Inputs: where it applies and the folder's full path. Output: the new skill. */
export function addSkill(scope: PluginScope, folder: string): Promise<Result<SkillView>> {
  return call<SkillView>("desktop_add_skill", { scope, folder });
}

/** Turns a skill on or off where it was added. Inputs: skill id and the switch. */
export function setSkillEnabled(id: string, enabled: boolean): Promise<Result<SkillView>> {
  return call<SkillView>("desktop_set_skill_enabled", { id, enabled });
}

/** Removes a skill and the engine's copy of its folder; the owner's folder stays. */
export function removeSkill(id: string): Promise<Result<void>> {
  return call<void>("desktop_remove_skill", { id });
}

/** Reads MCP servers for global settings (no project) or one project's settings.
 * Output: servers with how they run, status, saved secret names and tools. */
export function listMcpServers(projectId?: string): Promise<Result<McpServerView[]>> {
  return call<McpServerView[]>("desktop_plugin_servers", { projectId: projectId ?? null });
}

/** Adds an MCP server that waits for approval. Secret values go to the engine once, in its
 * own private frame, and never come back. Inputs: scope, name, spec, secrets by name. */
export function addMcpServer(scope: PluginScope, name: string, spec: McpServerSpec, secrets: [string, string][]): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_add_mcp_server", { scope, name, spec, secrets });
}

/** Approves an MCP server exactly as the owner saw it; the engine refuses one that changed.
 * Inputs: server id and the shown spec. Output: the approved server. */
export function approveMcpServer(id: string, spec: McpServerSpec): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_approve_mcp_server", { id, spec });
}

/** Turns an MCP server on or off where it was added. Inputs: server id and the switch. */
export function setMcpServerEnabled(id: string, enabled: boolean): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_set_mcp_server_enabled", { id, enabled });
}

/** Removes an MCP server and its saved secrets. Input: server id. */
export function removeMcpServer(id: string): Promise<Result<void>> {
  return call<void>("desktop_remove_mcp_server", { id });
}

/** Saves or replaces one secret for an MCP server; the value is sent once and not kept here.
 * Inputs: server id, secret name and value. Output: the server with its secret names. */
export function setMcpSecret(id: string, name: string, value: string): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_set_mcp_secret", { id, name, value });
}

/** Deletes one saved MCP server secret. Inputs: server id and secret name. */
export function removeMcpSecret(id: string, name: string): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_remove_mcp_secret", { id, name });
}

/** Starts a command MCP server, lists its tools and stops it. Input: server id.
 * Output: the server with its tools or the plain reason it can't start. */
export function checkMcpServer(id: string): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_check_mcp_server", { id });
}

/** Switches a global skill or MCP server on or off for one project.
 * Inputs: project id, item id and the switch. */
export function switchPlugin(projectId: string, id: string, on: boolean): Promise<Result<void>> {
  return call<void>("desktop_switch_plugin", { projectId, id, on });
}

/** Lists launched agents, newest first. Input: a project id, or none for every project.
 * Output: each agent's plain status and the controls usable now. */
export function agents(projectId?: string): Promise<Result<AgentView[]>> {
  return call<AgentView[]>("desktop_agents", { projectId: projectId ?? null });
}

/** Stops one agent and everything it started; other agents keep running. Input: agent id. */
export function stopAgent(id: string): Promise<Result<AgentView>> {
  return call<AgentView>("desktop_stop_agent", { id });
}

/** Interrupts what one agent is doing now; the agent keeps running. Input: agent id. */
export function interruptAgent(id: string): Promise<Result<void>> {
  return call<void>("desktop_interrupt_agent", { id });
}

/** Restarts one agent with its first task. Input: agent id. Output: the running agent. */
export function restartAgent(id: string): Promise<Result<AgentView>> {
  return call<AgentView>("desktop_restart_agent", { id });
}

/** Opens the macOS folder chooser. Output: the chosen folder's full path, or null when the
 * owner closed the chooser. */
export function pickFolder(): Promise<Result<string | null>> {
  return call<string | null>("desktop_pick_folder");
}

const SUGGESTION_PROJECTS_MAX = 50;

export interface Suggestion {
  project_id: string;
  project_name: string;
  id: string;
  version: number;
  text: string;
}

/** Reads one page of saved memory, newest first: app-wide (no project) or one project's
 * own notes. Inputs: project id or null, search text, and the previous page's next value.
 * Output: the page. */
export function savedMemories(project: string | null, search: string, before: number | null = null): Promise<Result<SavedMemoryPage>> {
  return call<SavedMemoryPage>("desktop_memories", { project, search: search.trim() || null, before });
}

/** Saves a memory the owner wrote, app-wide or for one project. Inputs: project id or null
 * and the text. Output: ok once saved. */
export function addMemory(project: string | null, text: string): Promise<Result<void>> {
  return change("desktop_add_memory", { project, text, generation: nextGeneration() });
}

/** Deletes one saved memory for good. Input: memory id. Output: ok once deleted. */
export function deleteMemory(id: string): Promise<Result<void>> {
  return change("desktop_delete_memory", { id, generation: nextGeneration() });
}

/** Picks the research memories in a project nobody reviewed yet. Inputs: the project and its
 * memory list. Output: the suggestions, in engine order. */
export function suggestionsFrom(project: { id: string; name: string }, items: MemoryItem[]): Suggestion[] {
  return items.filter((item) => item.status === "Provisional" && !item.human_reviewed)
    .map((item) => ({ project_id: project.id, project_name: project.name, id: item.id, version: item.version, text: item.text }));
}

/** Reads unreviewed research memories across projects (first page of each, up to 50
 * projects). Input: the projects. Output: the suggestions. */
export async function memorySuggestions(projects: { id: string; name: string }[]): Promise<Result<Suggestion[]>> {
  const found: Suggestion[] = [];
  for (const project of projects.slice(0, SUGGESTION_PROJECTS_MAX)) {
    const items = await call<MemoryItem[]>("desktop_project_memories", { id: project.id, after: null });
    if (items.kind !== "ok") return items;
    found.push(...suggestionsFrom(project, items.value));
  }
  return { kind: "ok", value: found };
}

export const MEMORY_TEXT_BYTES_MAX = 4096;

async function keepGlobally(suggestion: Suggestion, decision: { decision: "Keep" } | { decision: "Edit"; text: string }): Promise<Result<void>> {
  const kept = await change("desktop_review_memory", { id: suggestion.project_id, memory: suggestion.id, version: suggestion.version, decision, generation: nextGeneration() });
  if (kept.kind !== "ok") return kept;
  assert(suggestion.version < Number.MAX_SAFE_INTEGER, "memory version exhausted");
  return change("desktop_promote_memory", { project: suggestion.project_id, memory: suggestion.id, version: suggestion.version + 1, generation: nextGeneration() });
}

/** Saves a research suggestion to app-wide memory with one owner click: the engine first
 * keeps it in its project, then makes it global. Input: the suggestion. Output: ok once both
 * are saved, or the plain reason the second step failed. */
export function saveSuggestion(suggestion: Suggestion): Promise<Result<void>> {
  return keepGlobally(suggestion, { decision: "Keep" });
}

/** Checks a memory the owner rewrote, with the engine's rules: not blank, no NUL, at most 4096
 * UTF-8 bytes. Input: the text. Output: the trimmed text, or the plain reason it can't be saved. */
export function memoryEdit(text: string): { kind: "ok"; text: string } | { kind: "invalid"; reason: string } {
  const trimmed = text.trim();
  if (!trimmed) return { kind: "invalid", reason: "Write something to remember." };
  if (trimmed.includes("\0")) return { kind: "invalid", reason: "Remove the empty characters from the memory." };
  const bytes = new TextEncoder().encode(trimmed).length;
  if (bytes > MEMORY_TEXT_BYTES_MAX) return { kind: "invalid", reason: `Too long: ${bytes} of ${MEMORY_TEXT_BYTES_MAX} bytes.` };
  return { kind: "ok", text: trimmed };
}

/** Saves a research suggestion in the owner's own words: the engine keeps the edited text in
 * its project, then makes it app-wide. Inputs: the suggestion and the new text. Output: ok once
 * both are saved, the plain reason the text is refused, or why a step failed. */
export function saveEditedSuggestion(suggestion: Suggestion, text: string): Promise<Result<void>> {
  const edit = memoryEdit(text);
  if (edit.kind === "invalid") return Promise.resolve({ kind: "error", code: "invalid_knowledge_text", message: edit.reason });
  return keepGlobally(suggestion, { decision: "Edit", text: edit.text });
}

/** Keeps a project's research memory in the owner's own words, the TUI's "Edit first": the
 * engine saves the edited text in the project and bumps its version. Inputs: project id,
 * memory id, its version, the new text and a generation. Output: the receipt, or the plain
 * reason the text is refused before any engine call. */
export function editProjectMemory(id: string, memory: string, version: number, text: string, generation: number): Promise<Result<IntentReceipt>> {
  const edit = memoryEdit(text);
  if (edit.kind === "invalid") return Promise.resolve({ kind: "error", code: "invalid_knowledge_text", message: edit.reason });
  return call<IntentReceipt>("desktop_review_memory", { id, memory, version, decision: { decision: "Edit", text: edit.text }, generation });
}

/** Dismisses a research suggestion; it stays out of memory. Input: the suggestion. */
export function dismissSuggestion(suggestion: Suggestion): Promise<Result<void>> {
  return change("desktop_review_memory", { id: suggestion.project_id, memory: suggestion.id, version: suggestion.version, decision: { decision: "Reject" }, generation: nextGeneration() });
}

/** Reads the first page of projects for Home and the sidebar. Output: projects. */
export async function listProjects(): Promise<Result<ProjectPage["items"]>> {
  const result = await call<ProjectPage>("desktop_projects", { next: false });
  return result.kind === "ok" ? { kind: "ok", value: result.value.items } : result;
}

/** Reads one project's team graph: master, harness groups, agents and task edges.
 * Input: the project id. Output: the graph, not ready, or a plain error. */
export function loadTeamGraph(id: string): Promise<Result<TeamGraph>> {
  return call<TeamGraph>("desktop_team_graph", { id });
}

/** Opens a live view of one agent's terminal on a channel: the attach frame, the screen,
 * then live output. Inputs: the terminal name and the channel. Output: ok once attached. */
export function attachTerminal(agent: string, output: Channel<TerminalMessage>): Promise<Result<void>> {
  return call<void>("desktop_terminal_attach", { agent, output });
}

/** Types one chunk of bytes into an attached agent terminal. Inputs: the terminal name and
 * 1 to 4096 bytes. Output: ok once queued in order. */
export function typeIntoTerminal(agent: string, bytes: number[]): Promise<Result<void>> {
  return call<void>("desktop_terminal_input", { agent, bytes });
}

/** Sets an attached agent terminal's size. Inputs: the terminal name, columns and rows.
 * Output: ok once queued. */
export function resizeTerminal(agent: string, cols: number, rows: number): Promise<Result<void>> {
  return call<void>("desktop_terminal_resize", { agent, cols, rows });
}

/** Closes this window's view of an agent terminal; the agent keeps running.
 * Input: the terminal name. Output: ok once closed. */
export function detachTerminal(agent: string): Promise<Result<void>> {
  return call<void>("desktop_terminal_detach", { agent });
}

/** Turns keeping the team's minimum sub-agents running on or off. Inputs: the project id
 * and the switch. Output: the engine receipt, not ready, or a plain error. */
export function keepTeamWarm(id: string, on: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_keep_warm", { id, on, generation: nextGeneration() });
}

/** Hands one task straight to a sub-agent. Inputs: the project id, a harness of the team plan
 * or null for any, and the task. Output: the engine receipt, not ready, or a plain error. */
export function delegateTeamTask(id: string, harness: string | null, prompt: string): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_delegate", { id, harness, prompt, generation: nextGeneration() });
}
