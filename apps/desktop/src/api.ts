import { invoke as native, type InvokeArgs } from "@tauri-apps/api/core";
import { retryBusy } from "./engine-client";
import type { AccountPool, IntentReceipt, ModelAccessDetail, ModelAccessEdit, ModelAccessPage, ProjectPage } from "./engine-client";

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
  account_not_found: "This account no longer exists.",
  native_profile_not_managed: "This account doesn't use a subscription sign-in.",
  native_auth_host_unavailable: "Sign-in isn't available on this Mac right now. Check that the harness is installed.",
  native_auth_cannot_display_safely: "The sign-in page sent something CrazyAgent can't show safely. Try again.",
  native_auth_outcome_unknown_read_same_operation: "We couldn't confirm the sign-in. Check again.",
  native_auth_claude_input_invalid: "Paste the complete code from the Claude page.",
  native_auth_claude_input_outcome_unknown: "We couldn't confirm the code was sent. Check again before sending it a second time.",
};

export const KNOWN_ERROR_CODES = Object.keys(COPY);

/** Turns an engine error code into one plain sentence for the owner.
 * Input: an error code. Output: the copy for it, or the generic fallback. */
export function plainError(code: string): string {
  return COPY[code] ?? FALLBACK;
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

export interface Skill {
  id: string;
  name: string;
  description: string;
  path: string;
  default_on: boolean;
}

export interface McpServer {
  id: string;
  name: string;
  runs: string;
  status: "approved" | "needs_approval" | "turned_off" | "cant_start";
  status_label: string;
  reason: string | null;
  network: boolean;
  secrets: string[];
  default_on: boolean;
}

export interface Memory {
  id: string;
  text: string;
  source: string;
  scope: "app" | { project: string };
  state: "saved" | "suggested";
  saved_at: string | null;
}

/** Reads the app-wide skills. Output: skills with their folder and default switch. */
export function listSkills(): Promise<Result<Skill[]>> {
  return call<Skill[]>("desktop_skills");
}

/** Turns a global skill on or off for new projects. Inputs: skill id and the switch. */
export function setSkillDefault(id: string, on: boolean): Promise<Result<void>> {
  return call<void>("desktop_set_skill_default", { id, on });
}

/** Reads the app-wide MCP servers with their approval status. Output: the servers. */
export function listMcpServers(): Promise<Result<McpServer[]>> {
  return call<McpServer[]>("desktop_mcp_servers");
}

/** Approves one MCP server after the owner reviewed what it runs. Input: server id. */
export function approveMcpServer(id: string): Promise<Result<void>> {
  return call<void>("desktop_approve_mcp_server", { id });
}

/** Turns a global MCP server on or off for new projects. Inputs: server id and the switch. */
export function setMcpDefault(id: string, on: boolean): Promise<Result<void>> {
  return call<void>("desktop_set_mcp_default", { id, on });
}

/** Reads saved and suggested memories across projects. Output: the memories. */
export function listMemories(): Promise<Result<Memory[]>> {
  return call<Memory[]>("desktop_memories");
}

/** Saves a suggested memory; this click is the owner's approval. Input: memory id. */
export function keepMemory(id: string): Promise<Result<void>> {
  return call<void>("desktop_keep_memory", { id });
}

/** Dismisses a suggestion or deletes a saved memory. Input: memory id. */
export function forgetMemory(id: string): Promise<Result<void>> {
  return call<void>("desktop_forget_memory", { id });
}

/** Reads the first page of projects for Home and the sidebar. Output: projects. */
export async function listProjects(): Promise<Result<ProjectPage["items"]>> {
  const result = await call<ProjectPage>("desktop_projects", { next: false });
  return result.kind === "ok" ? { kind: "ok", value: result.value.items } : result;
}
