import { type Channel, type InvokeArgs } from "@tauri-apps/api/core";
import { bridgeCall, errorMessage, withCode } from "./engine-client";
import type { ActionReceipt, BackgroundAtEnd, BranchMode, CatchUpMain, ChildLanding, CoordHarness, MasterIdleStop, MasterLands, CoordSettings, Delivery, PendingIntent, ReuseIdle, SubagentMode, SwitchApply, SwitchMode, TeamUse, ToolSwitch, ToolView } from "./engine-client";
import type { AccountPool, AccountUse, AgentView, AccountProfileView, IntentReceipt, MemoryItem, McpServerSpec, McpServerView, ModelAccessDetail, ModelAccessEdit, ModelAccessPage, PluginScope, ProjectList, AgentMemoryWrites, SavedMemoryPage, SkillDetail, SkillView, HookView, TeamGraph, TerminalMessage, ImportList, ImportedSkill, QuestionsInbox } from "./engine-client";

export type { AgentMemoryWrites, SavedMemory, SavedMemoryPage } from "./engine-client";
export type { HookView, McpServerSpec, McpServerView, PluginScope, SkillDetail, SkillFile, SkillView, ImportCandidate, ImportFrom, ImportList, ImportState, ImportedSkill } from "./engine-client";
export type { CoordHarness, CoordinationView, CoordSettings, Delivery, HarnessCoordination, Overlaps, QuestionsTo, SubagentMode, ToolSwitch, ToolView } from "./engine-client";
export type { AccountUse, AgentRole, AgentState, AgentView, Holder } from "./engine-client";

export type Failure = { kind: "not_ready" } | { kind: "error"; code: string; message: string; cause?: string };
export type Result<T> = { kind: "ok"; value: T } | Failure;

const FALLBACK = "Something went wrong. Try again.";
const UNREACHABLE = "engine_unreachable";
const PAGE_LIMIT = 200;
const ACCOUNT_PAGES_MAX = 100;

const COPY: Record<string, string> = {
  engine_unreachable: "CrazyAgent's engine isn't running.",
  engine_version_mismatch: "CrazyAgent's engine is a different version from this app. Restart the engine.",
  engine_start_failed: "CrazyAgent's engine didn't start. Try again.",
  owner_not_ready: "The engine isn't ready for changes yet. Try again in a moment.",
  owner_read_failed: "Couldn't read this from the engine. Try again.",
  engine_sidecar_missing: "This app is missing its built-in engine. Reinstall CrazyAgent.",
  engine_service_failed: "CrazyAgent couldn't set up its background engine. Try again.",
  engine_restart_unavailable: "This window uses an engine you started yourself. Restart that engine, then try again.",
  uncertain: "We couldn't confirm your last change. Check it and save again.",
  pending_intent: "Another change isn't confirmed yet. Use Check again at the top of the window, then try again.",
  change_may_be_recorded: "This change may still go through, so it can't be dismissed. Use Check again.",
  stale_revision: "This changed since you opened it. Review and save again.",
  stale_model_access: "This changed since you opened it. Review and save again.",
  stale_config: "This changed since you opened it. Review and save again.",
  settings_source_changed: "This changed since you opened it. Review and save again.",
  settings_read_only: "Settings can't be changed in this engine. Start it with writable settings.",
  busy: "Another change is still running. Wait for it to finish.",
  rejected: "The engine didn't accept this change.",
  unavailable: "This isn't available right now. Try again.",
  pool_member_limit: "A pool can hold up to 15 accounts.",
  linux_limited_network_off: "The Linux limited-network sockets setting is 'Do not start limited-network agents', so this master does not start. Pick another Linux limited-network sockets option in Settings.",
  linux_resolver_needs_udp: "This harness cannot look up names over TCP, which the Linux setting 'TCP only' needs. Choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
  linux_harness_unchecked: "This harness starts through a script CrazyAgent cannot check for the Linux setting 'TCP only', so it does not start. Start it from a program, or a script whose first line names a program by its full path, or choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
  pool_needs_account: "A pool needs at least one account. Add another account first, or delete the pool.",
  pool_project_limit: "A pool can serve up to 64 projects. Pick another pool for this project.",
  pool_name_bounds: "Use a pool name of 1 to 60 characters on one line.",
  pool_exists: "A pool with this name already exists.",
  pool_not_found: "This pool no longer exists.",
  project_not_found: "This project no longer exists.",
  account_rank_invalid: "Use a rank from 1 to 999, or leave it empty.",
  account_reserve_invalid: "Use a reserve from 0 to 99 percent.",
  account_schedule_invalid: "Use a rank from 1 to 999 and a reserve from 0 to 99 percent.",
  account_schedule_full: "Too many saved account choices. Clear some you no longer need, then try again.",
  connection_not_found: "This account no longer exists. Refresh and try again.",
  approved_accounts_limit: "Projects can use up to 64 approved accounts. Remove an account you no longer use, then try again.",
  allowed_destinations_limit: "Up to 64 sign-ins and model endpoints can be allowed. Remove a connection you no longer use, then try again.",
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
  native_auth_cancel_pending: "The sign-in is still stopping. Try again in a moment.",
  account_not_found: "This account no longer exists.",
  native_profile_not_managed: "This account doesn't use a subscription sign-in.",
  native_auth_host_unavailable: "Sign-in isn't available on this Mac right now. Check that the harness is installed.",
  native_auth_cannot_display_safely: "The sign-in page sent something CrazyAgent can't show safely. Try again.",
  native_auth_outcome_unknown_read_same_operation: "We couldn't confirm the sign-in. Check again.",
  native_auth_sign_in_running: "A sign-in is still running. Wait for it to finish or cancel it, then try again.",
  native_auth_recovery_required: "A sign-in didn't confirm how it ended, so changes to its account and new sign-ins wait. Use Clear stuck sign-in at the top of the window, then try again.",
  native_auth_in_progress: "A sign-in is still running. Finish or cancel it on its sign-in page first.",
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
  account_profile_outcome_unknown_read_same_request: "We couldn't confirm the account is ready for agents yet. The app checks again on its own, or use Check again at the top of the window.",
  account_profile_recovery_required: "An account's set-up for agents isn't confirmed yet. Use Check again at the top of the window, then try again.",
  memory_note_blank: "Write something to remember.",
  memory_note_too_long: "Keep the memory under 2000 characters.",
  memory_note_invalid: "Remove hidden, direction-changing or control characters from the memory.",
  memory_duplicate: "This memory is already saved.",
  memory_full: "Memory is full. Delete an old memory first.",
  memory_import_folder_invalid: "Choose a folder that exists. A link to a folder can't be imported.",
  memory_import_invalid: "A file in this folder can't be imported.",
  memory_missing: "That memory no longer exists.",
  memory_changed: "This memory changed. Reload it and try again.",
  memory_not_kept: "Only memories you kept can be made global.",
  memory_already_global: "This memory is already in global memory.",
  memory_project_missing: "That project no longer exists.",
  memory_search_invalid: "Keep the search on one line, up to 100 characters.",
  memory_page_invalid: "Show between 1 and 20 memories at a time.",
  knowledge_version_conflict: "This memory changed. Reload it and try again.",
  knowledge_source_revoked: "The research this came from was deleted.",
  invalid_knowledge_text: "Write the memory as plain text, up to 4096 bytes, without hidden or direction-changing characters.",
  read_path_not_absolute: "Type the full path of a folder, starting with /.",
  read_path_missing: "That folder doesn't exist on this computer.",
  read_path_not_folder: "That path is a file. Add a folder.",
  read_path_holds_home: "Agents can't read your whole home folder. Add a folder inside it.",
  read_path_credentials: "That folder holds or sits in a credential location, such as .ssh or .aws, so agents can never read it.",
  read_path_engine_data: "That folder holds CrazyAgent's own data, so agents can never read it.",
  read_path_refused: "That folder can't be shared with agents. Pick another folder.",
  read_path_owner_unknown: "CrazyAgent couldn't find your home folder, so it can't check that path.",
  read_paths_too_many: "A project can share at most 8 extra folders.",
  read_paths_invalid: "Each folder must be one plain path of up to 1024 characters.",
  hidden_paths_too_many: "A project can hide at most 8 folders.",
  hidden_paths_invalid: "Each folder must be one plain path of up to 1024 characters.",
  hidden_path_not_absolute: "Each hidden folder must be one absolute path of up to 1024 characters.",
  hidden_path_missing: "That folder can't be found. Pick a folder that exists.",
  hidden_path_not_folder: "That path isn't a folder. Pick a folder.",
  hidden_path_outside_project: "A hidden folder must sit inside the project. Pick a folder within it.",
  hidden_path_not_plain: "A hidden folder path can't hold quotes, backslashes or control characters.",
  hidden_path_tracked: "Git tracks files in that folder. Hidden folders cover only files git doesn't track; pick a folder git ignores.",
  hidden_path_unchecked: "CrazyAgent couldn't check that folder against git. Try again.",
  agent_access_unavailable: "That agent access level can't run on this computer. Pick another level.",
  agent_policy_invalid: "That agent access choice isn't valid. Open Agent access and save again.",
  agent_policy_unreadable: "CrazyAgent couldn't read this project's agent access. Try again.",
  agent_access_lowered: "This project's agent access was lowered below what the agent started with, so it was stopped. What it changed in its own folder is still in Changes; an agent that ran at Full access may also have changed files outside it.",
  agent_network_narrowed: "This project's agent network was narrowed, so the agent was stopped. Its next start uses the new network.",
  agent_toolchain_lowered: "A toolchain the agent started with is no longer allowed for this project, so the agent was stopped. Its next start goes without it.",
  agent_project_unreadable: "CrazyAgent couldn't read the project folder to check the agent's toolchains, so the agent was stopped. Try again.",
  agent_git_needs_tools: "Agent git needs the Command Line Tools: run xcode-select --install.",
  project_network_unreadable: "CrazyAgent couldn't read this project's network choice, so the check was refused. Try again.",
  temp_folder_shared: "Your temp folder is shared with other users on this computer, so it is never given to agents.",
  temp_folder_needs_macos: "Your temp folder can be given to agents only on macOS in this version.",
  grant_socket_needs_macos: "A socket can be allowed only on macOS in this version.",
  grant_socket_not_plain: "The socket path must be a full path of letters, digits, slashes, dots, dashes and underscores, at most 103 bytes, in a folder that exists.",
  grant_socket_secret: "That socket sits with your keys or credentials, so it is never allowed.",
  grant_socket_engine: "That socket belongs to CrazyAgent itself, so it is never allowed.",
  grant_socket_agent: "That socket hands out your SSH keys or other agents' terminals, so it is never allowed.",
  grant_socket_host: "That socket controls containers or virtual machines (Docker, containerd, Podman, Colima, Lima, OrbStack and the like), which can reach your whole computer, so it is never allowed.",
  grant_socket_keys: "That socket belongs to a key or password agent (gpg-agent, 1Password, SSH or another agent), so it is never allowed.",
  grant_socket_owner: "That socket belongs to the system or another user, or sits in a folder other users can change, so it is never allowed.",
  grant_socket_missing: "There is no socket at that path now. Start the program that makes it, then allow it again.",
  grant_port_reserved: "That port belongs to your sign-in, your local model or CrazyAgent itself (its IDE or model proxy), so it is never allowed.",
  grant_port_dynamic: "Ports from 49152 up are lent by the system to CrazyAgent's IDE and other apps for a while, so they are never allowed. Pick a fixed port below 49152.",
  agent_grants_invalid: "A project allows at most 2 sockets, 4 ports and your temp folder, each once.",
  grant_full: "This project already allows as many extras of that kind as it can.",
  grant_requests_full: "Four access requests of this project are still waiting for the owner.",
  grant_reason_invalid: "Give a reason in plain words.",
  agent_grant_revoked: "An allowed extra this agent held (a socket, a port or your temp folder) was removed, so the agent was stopped. Its next start goes without it.",
};

export const KNOWN_ERROR_CODES = Object.keys(COPY);

/** The sentence this window has for an engine or bridge code. Input: the code. Output: the
 * sentence, or undefined when there is none. */
export function knownError(code: string): string | undefined {
  if (Object.hasOwn(COPY, code)) return COPY[code];
  return /^(skill|server|plugins?|hooks?)_[a-z_]+$/.test(code) ? errorMessage({ code }) : undefined;
}

/** The owner's line for a failure, never opaque: its own sentence, else the generic sentence;
 * the engine cause behind it is named after the sentence, and the generic sentence names the code
 * when there is no cause. Inputs: the sentence if known, the code and the engine cause. Output:
 * one plain line. */
export function namedFailure(sentence: string | undefined, code: string, cause?: string): string {
  const named = cause === undefined || cause === code ? undefined : cause;
  if (sentence === undefined) return withCode(FALLBACK, named ?? code);
  return named === undefined ? sentence : withCode(sentence, named);
}

/** Turns an engine error code into one plain sentence for the owner.
 * Inputs: an error code and the engine cause behind it, if any. Output: the copy for it, or
 * the generic fallback naming the code. */
export function plainError(code: string, cause?: string): string {
  return namedFailure(knownError(code), code, cause);
}

/** Sorts a failed bridge call into "not ready yet" (the command is not in this build) or a
 * plain error. Raw text is never shown. Input: the rejection value. Output: a failure. */
export function classifyFailure(error: unknown): Failure {
  if (typeof error === "string") {
    if (/^Command [a-z_]+ not found$/.test(error) || /^[a-z_]+ not allowed\b[\s\S]*Command not found$/.test(error)) return { kind: "not_ready" };
    return { kind: "error", code: "unknown", message: plainError("unknown") };
  }
  if (error instanceof TypeError || error instanceof ReferenceError) return { kind: "error", code: UNREACHABLE, message: plainError(UNREACHABLE) };
  const raw = typeof error === "object" && error !== null && "code" in error ? String(error.code) : "unknown";
  const code = ["not_connected", "not_configured", "owner_unavailable", "owner_revoked"].includes(raw) ? UNREACHABLE : raw;
  const cause = typeof error === "object" && error !== null && "cause" in error && typeof error.cause === "string" ? error.cause : undefined;
  return cause === undefined ? { kind: "error", code, message: plainError(code) } : { kind: "error", code, cause, message: plainError(code, cause) };
}

/** Calls one Tauri command, retrying only the bridge's pre-action "busy" refusal.
 * Inputs: the command and its arguments. Output: the value, not ready, or a plain error. */
export async function call<T>(command: string, args?: InvokeArgs): Promise<Result<T>> {
  try {
    return { kind: "ok", value: await bridgeCall<T>(command, args) };
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
  stuck_sign_in: string | null;
}

/** Connects this window to the engine. Output: whether it is connected and whether an earlier
 * change is still unconfirmed. */
export function connect(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_connect");
}

/** The default engine's version beside this app's, and the plain message asking for a restart
 * when they differ. `engine` is null when no engine answers. */
export interface EngineVersions {
  engine: string | null;
  app: string;
  message: string | null;
}

/** Reads the default engine's version and this app's. Output: both versions and, when they
 * differ, the message to show, or a plain error. */
export function engineVersions(): Promise<Result<EngineVersions>> {
  return call<EngineVersions>("desktop_engine_versions");
}

/** Restarts the default engine on this app's own version through its login service, then
 * reconnects this window. Output: the new connection, or a plain error. */
export function restartEngine(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_restart_engine");
}

/** Reads the current engine connection. Output: connection and unconfirmed-change flags. */
export function status(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_status");
}

/** Reads the change the bridge still keeps unconfirmed. Output: the change, or null. */
export function keptChange(): Promise<Result<PendingIntent | null>> {
  return call<PendingIntent | null>("desktop_pending_intent");
}

/** Asks the engine whether the kept change was recorded, by its operation id and hash,
 * reconnecting first when a lost reply let go of the engine.
 * Output: the check's receipt, or why it couldn't run. */
export async function checkUnconfirmed(): Promise<Result<IntentReceipt>> {
  const current = await status();
  if (current.kind === "ok" && !current.value.connected) {
    const connected = await connect();
    if (connected.kind !== "ok") return connected;
  }
  return call<IntentReceipt>("desktop_reconcile_intent");
}

/** Sends the exact kept change again; the engine applies it once even if the first send
 * landed. Output: the receipt, or why it couldn't run. */
export function retryUnconfirmed(): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_retry_intent");
}

/** Drops the kept change once the engine said it never recorded it, so it never goes
 * through. Output: the connection, or why it can't be dropped while it may still land. */
export function dropUnconfirmed(): Promise<Result<ConnectionStatus>> {
  return call<ConnectionStatus>("desktop_drop_intent");
}

/** Pauses before each automatic check of an unconfirmed change, in milliseconds. */
export const UNCONFIRMED_CHECKS_MS: readonly number[] = [2000, 4000, 8000, 16000, 30000];

const MODEL_ACCESS_CHANGE: Record<ModelAccessEdit["kind"], string> = {
  add_account: "adding an account",
  remove_account: "deleting an account",
  create_connection: "adding a connection",
  replace_connection: "changing a connection",
  remove_connection: "removing a connection",
  create_pool: "creating a pool",
  replace_pool: "changing a pool",
  remove_pool: "deleting a pool",
  assign_project_pool: "changing a project's pool",
};

const CHANGE: Record<PendingIntent["kind"], string> = {
  message: "sending your message",
  brief: "changing the project brief",
  approve_baseline: "approving a baseline",
  revoke_baseline: "revoking a baseline",
  model_access: "changing accounts and pools",
  create_project: "creating the project",
  setup_project: "saving the project setup",
  research_memory: "saving research to memory",
  project_documents: "adding project documents",
  project_context_delete: "removing a project file",
  project_read_paths: "saving extra folders agents can read",
  project_hidden_paths: "saving folders agents can't open",
  project_agent_policy: "saving agent access",
  account_schedule: "saving the account order",
  memory_review: "reviewing a memory",
  memory_add: "adding a memory",
  memory_promote: "promoting a memory",
  memory_delete: "deleting a memory",
  memory_approve: "approving a memory",
  memory_agent_writes: "saving agent memory writes",
  memory_forget: "forgetting a memory",
  memory_remember: "saving a memory",
  memory_selection: "changing the memory selection",
  master: "changing the master conversation",
  owner_profile: "saving your profile",
  sandbox_settings: "changing the sandbox setting",
  cleanup_settings: "changing when finished agents' worktrees go",
  quit_settings: "changing what quitting does to team agents",
  master_step_settings: "changing the master step limit",
  master_time_settings: "changing the master time limit",
  projects_home: "changing the projects folder",
  move_projects_home: "moving your projects",
  add_existing_folder: "adding a folder",
  project_move_home: "moving the project into your projects folder",
  start_onboarding: "starting project setup",
  run_setup: "running project setup",
  pack_set: "changing the default pack",
  pack_remove: "removing a pack",
  team: "changing the team",
  project_setup_update: "updating the project setup",
  project_remove: "removing the project",
  project_delete: "deleting the project",
  research_cancel: "cancelling the research",
  research_retry: "retrying the research",
  plan_roadmap: "planning the roadmap",
};

/** Names a kept change in plain words. Input: the change. Output: a lower-case phrase, the
 * broader phrase for an access edit the window has no name for, and "your change" for a kind
 * it does not know, so the banner never fails to render. */
export function changeLabel(intent: PendingIntent): string {
  const access = intent.kind === "model_access" && intent.model_access ? MODEL_ACCESS_CHANGE[intent.model_access.kind] : undefined;
  return access ?? CHANGE[intent.kind] ?? "your change";
}

function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** The banner line for a change that is still unconfirmed. Inputs: the change and the last
 * check's outcome, or null before any check. Output: the line naming the change. */
export function unconfirmedLine(intent: PendingIntent, outcome: ActionReceipt["outcome"] | null): string {
  const label = changeLabel(intent);
  if (outcome === "not_recorded_at_barrier") return `${capital(label)} never reached the engine. Retry sends it again, or Dismiss drops it.`;
  return `We couldn't confirm ${label}. Other changes wait until it's checked.`;
}

/** The toast after a check or retry settles a kept change. Input: the receipt.
 * Output: whether it went through, or null while the change is still unconfirmed. */
export function settledLine(receipt: IntentReceipt): string | null {
  if (receipt.pending) return null;
  const label = capital(changeLabel(receipt));
  if (receipt.outcome === "applied") return `${label} went through.`;
  const code = receipt.rejection ?? (receipt.outcome === "rejected" ? "rejected" : "unavailable");
  return `${label} didn't go through. ${plainError(code)}`;
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

/** How a limited-network agent's sockets are held on Linux, the engine's `LinuxSockets`. */
export type LinuxSockets = "tcp_only" | "refuse" | "tcp_ports";

/** The owner's app-wide sandbox settings, the engine's `SandboxSettings`. */
export interface SandboxSettings {
  linux_sockets: LinuxSockets;
}

/** Reads the owner's sandbox settings. Output: the stored settings, or the defaults before the first save. */
export function getSandboxSettings(): Promise<Result<SandboxSettings>> {
  return call<SandboxSettings>("desktop_sandbox_settings");
}

/** Saves the owner's sandbox settings. Input: the full settings. Output: ok once saved. */
export function saveSandboxSettings(settings: SandboxSettings): Promise<Result<void>> {
  return change("desktop_set_sandbox_settings", { settings, generation: nextGeneration() });
}

/** When the engine removes a finished agent's worktree on its own, the engine's `CleanupMode`. */
export type CleanupMode = "after_days" | "once_landed" | "manual";

/** The owner's finished agents setting, the engine's `CleanupSettings`: the mode and the days
 * the first mode waits, 1 to 365, kept while another mode is picked. */
export interface CleanupSettings {
  mode: CleanupMode;
  days: number;
}

/** Reads when finished agents' worktrees go. Output: the stored setting, or the default before the first save. */
export function getCleanupSettings(): Promise<Result<CleanupSettings>> {
  return call<CleanupSettings>("desktop_cleanup_settings");
}

/** Saves when finished agents' worktrees go. Input: the full setting. Output: ok once saved. */
export function saveCleanupSettings(settings: CleanupSettings): Promise<Result<void>> {
  return change("desktop_set_cleanup_settings", { settings, generation: nextGeneration() });
}

/** What quitting CrazyAgent does to running team agents, the engine's `OnQuit`. */
export type OnQuit = "keep_running" | "stop";

/** How long agents left running wait for CrazyAgent to come back, the engine's `LeftRunning`. */
export type LeftRunning = "after_hours" | "never";

/** The owner's agents on quit setting, the engine's `QuitSettings`: what quitting does to
 * running team agents, and how long the ones left running wait, with the hours, 1 to 168,
 * kept while Never is picked. */
export interface QuitSettings {
  on_quit: OnQuit;
  left_running: LeftRunning;
  hours: number;
}

/** Reads what quitting does to team agents. Output: the stored setting, or the default before the first save. */
export function getQuitSettings(): Promise<Result<QuitSettings>> {
  return call<QuitSettings>("desktop_quit_settings");
}

/** Saves what quitting does to team agents. Input: the full setting. Output: ok once saved. */
export function saveQuitSettings(settings: QuitSettings): Promise<Result<void>> {
  return change("desktop_set_quit_settings", { settings, generation: nextGeneration() });
}

/** The most steps a master takes in one turn, the engine's `StepLimit`. */
export type StepLimit = "no_limit" | "steps50" | "steps100" | "steps200";

/** The owner's master step limit setting, the engine's `MasterStepSettings`. */
export interface MasterStepSettings {
  limit: StepLimit;
}

/** Reads the master step limit. Output: the stored setting, or No limit before the first save. */
export function getMasterStepSettings(): Promise<Result<MasterStepSettings>> {
  return call<MasterStepSettings>("desktop_master_step_settings");
}

/** Saves the master step limit. Input: the full setting. Output: ok once saved. */
export function saveMasterStepSettings(settings: MasterStepSettings): Promise<Result<void>> {
  return change("desktop_set_master_step_settings", { settings, generation: nextGeneration() });
}

/** How long a master works on one turn before it stops, the engine's `TimeLimit`. */
export type TimeLimit = "no_limit" | "minutes10" | "minutes30" | "minutes60";

/** The owner's master time limit setting, the engine's `MasterTimeSettings`. */
export interface MasterTimeSettings {
  limit: TimeLimit;
}

/** Reads the master time limit. Output: the stored setting, or No limit before the first save. */
export function getMasterTimeSettings(): Promise<Result<MasterTimeSettings>> {
  return call<MasterTimeSettings>("desktop_master_time_settings");
}

/** Saves the master time limit. Input: the full setting. Output: ok once saved. */
export function saveMasterTimeSettings(settings: MasterTimeSettings): Promise<Result<void>> {
  return change("desktop_set_master_time_settings", { settings, generation: nextGeneration() });
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

/** The words each account goes by wherever a view shows it outside its own row in Accounts:
 * the name the owner gave it, with the account id after it when another account has the
 * same name, so two accounts never read the same. Input: the accounts. Output: the words by
 * account id. */
export function accountLabels(accounts: readonly { id: string; name: string }[]): Map<string, string> {
  const uses = new Map<string, number>();
  for (const account of accounts) uses.set(account.name, (uses.get(account.name) ?? 0) + 1);
  const labels = new Map<string, string>();
  for (const account of accounts) labels.set(account.id, uses.get(account.name) === 1 ? account.name : `${account.name} (${account.id})`);
  if (labels.size > accounts.length) throw new Error(`more account labels than accounts: ${labels.size} for ${accounts.length}`);
  return labels;
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

/** Where an account set-up for agents that the bridge kept unconfirmed stands after it read
 * the same request again: configured, never recorded (nothing was set up), still unanswered,
 * no set-up kept, or the read failed. */
export type ProfileCheck = "configured" | "not_recorded" | "pending" | "none" | "failed";

/** Reads where a kept account set-up stands from one read of it. Input: the read.
 * Output: the state. */
export function profileCheck(read: Result<AccountProfileView>): ProfileCheck {
  if (read.kind !== "ok") return read.kind === "error" && read.code === "account_profile_operation_missing" ? "none" : "failed";
  if (read.value.pending) return "pending";
  return read.value.reply.outcome === "not_recorded" ? "not_recorded" : "configured";
}

/** Reads the account set-up the bridge still keeps unconfirmed, the same request again, so
 * no key is sent twice. Output: where it stands. */
export async function checkAccountProfile(): Promise<ProfileCheck> {
  return profileCheck(await call<AccountProfileView>("desktop_read_account_profile"));
}

/** The banner line while an account set-up for agents is unconfirmed. */
export const PROFILE_UNCONFIRMED_LINE = "We couldn't confirm an account's set-up for agents. Account, pool and project changes wait until it's checked.";

/** The toast once an unconfirmed account set-up settles. Input: the check. Output: the line,
 * or null while it is unconfirmed or when nothing was kept. */
export function profileSettledLine(check: ProfileCheck): string | null {
  if (check === "configured") return "The account is set up for agents.";
  if (check === "not_recorded") return "The account set-up for agents never reached the engine. Save the pool again to set it up.";
  return null;
}

/** The banner line while a sign-in's outcome is unconfirmed. */
export const STUCK_SIGN_IN_LINE = "A sign-in didn't confirm how it ended. Changes to that account and new sign-ins wait until you clear it.";

/** The toast when settings changed after a stuck sign-in started: only the engine can end it. */
export const STALE_SIGN_IN_LINE = "Settings changed after this sign-in started, so only the engine can end it, within 10 minutes of its start. Clear it again after that.";

/** Asks the engine about the stuck sign-in and, while it still runs, asks it to stop. The app
 * forgets the sign-in only once the engine answers for it. Output: true when cleared, false
 * while the engine is still stopping it, or a plain error. */
export function clearStuckSignIn(): Promise<Result<boolean>> {
  return call<boolean>("desktop_clear_native_auth");
}

/** The toast after Clear stuck sign-in. Input: the clear result. Output: the line. */
export function stuckClearedLine(result: Result<boolean>): string {
  if (result.kind === "ok") return result.value ? "Stuck sign-in cleared." : plainError("native_auth_cancel_pending");
  if (result.kind === "error") return result.code === "native_profile_source_conflict" ? STALE_SIGN_IN_LINE : result.message;
  return plainError("unknown");
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

export type AccountStrategy = "use_it_or_lose_it" | "manual_rank" | "balanced";
export type AccountPin = "never_use" | "use_last";
/** What a live master does once its pool's account order puts another account first (DEC-139). */
export type LiveMasterAccount = "keep" | "move_when_idle";

export interface AccountPlan {
  rank: number | null;
  reserve_percent: number;
  pin: AccountPin | null;
}

export interface AccountSchedule {
  strategy: AccountStrategy;
  live_master: LiveMasterAccount;
  pools: { id: string; strategy: AccountStrategy }[];
  projects: { id: string; strategy: AccountStrategy }[];
  accounts: { connection: string; plan: AccountPlan }[];
  version: number;
}

export type ScheduleChange =
  | { kind: "app_strategy"; strategy: AccountStrategy }
  | { kind: "live_master"; choice: LiveMasterAccount }
  | { kind: "pool_strategy"; pool: string; strategy: AccountStrategy | null }
  | { kind: "project_strategy"; project: string; strategy: AccountStrategy | null }
  | { kind: "account"; connection: string; plan: AccountPlan };

/** Reads how pools choose accounts: the app strategy, pool strategies, project overrides
 * and each account's rank, reserve and pin. Output: the schedule. */
export function accountSchedule(): Promise<Result<AccountSchedule>> {
  return call<AccountSchedule>("desktop_account_schedule");
}

/** Why a ready account stands where it does in its pool's ranking. */
export type OrderWhy =
  | { kind: "use_last" } | { kind: "limit_reported" } | { kind: "no_reading" }
  | { kind: "resets_soonest"; resets_ms: number } | { kind: "resets_later"; resets_ms: number }
  | { kind: "least_used" } | { kind: "more_used" } | { kind: "more_five_hour_used" }
  | { kind: "ranked"; rank: number } | { kind: "unranked" }
  | { kind: "tied_rank"; rank: number | null } | { kind: "tied_order" };

export type AccountStanding =
  | { kind: "next"; weekly_resets_ms: number | null; weekly_left_percent: number | null; use_last: boolean; why: OrderWhy }
  | { kind: "not_signed_in" }
  | { kind: "sign_in_unconfirmed" }
  | { kind: "paused"; until_ms: number }
  | { kind: "never_use" }
  | { kind: "five_hour_cap"; until_ms: number | null }
  | { kind: "reserve_reached"; reserve_percent: number; until_ms: number | null };

export interface ProjectHold {
  project: string;
  name: string;
  until_ms: number;
}

export interface RankedAccount {
  connection: string;
  standing: AccountStanding;
  held_for?: ProjectHold[];
}

export interface PoolRanking {
  pool: string;
  strategy: AccountStrategy;
  accounts: RankedAccount[];
  own_order_projects: number;
}

/** Reads each pool's account ranking: the order work would try its accounts now, then each
 * held account with why. Output: one ranking per pool. */
export function accountRanking(): Promise<Result<PoolRanking[]>> {
  return call<PoolRanking[]>("desktop_account_ranking");
}

/** Saves one account choice change. Input: the change. Output: ok once the engine applied
 * it, or a plain error. */
export function setAccountSchedule(scheduleChange: ScheduleChange, version: number): Promise<Result<void>> {
  return change("desktop_set_account_schedule", { change: scheduleChange, version, generation: nextGeneration() });
}

/** Makes the id of a new pool, so the page can select the pool it creates. Output: the id. */
export function newPoolId(): string {
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

/** A pool's name and member ids, as one read or one save saw them. */
export interface PoolBase { name: string; members: string[] }

/** Applies what the owner changed in a pool since they read it onto the pool as it is now, so
 * a change another client made meanwhile is not undone: members the owner removed leave,
 * members the owner added join after the current ones, and the name changes only when the
 * owner renamed it. Inputs: the pool now, the pool as the owner read it, and what the owner
 * saved. Output: the merged name and members, or a plain error such as the 15-account limit. */
export function mergePool(now: PoolBase, opened: PoolBase, saved: PoolBase): Result<PoolBase> {
  const removed = new Set(opened.members.filter((id) => !saved.members.includes(id)));
  const kept = now.members.filter((id) => !removed.has(id));
  const added = saved.members.filter((id) => !opened.members.includes(id) && !kept.includes(id));
  const renamed = saved.name.trim() !== opened.name.trim() || !now.name.trim();
  return poolInput(renamed ? saved.name : now.name, [...kept, ...added]);
}

/** Creates a pool with its first accounts. Inputs: its new id, name and member ids. Output: ok
 * once the engine saved it. */
export async function createPool(id: string, name: string, members: string[]): Promise<Result<void>> {
  const input = poolInput(name, members);
  if (input.kind !== "ok") return input;
  const page = await call<ModelAccessPage>("desktop_model_access_page", { collection: "pools", next: false });
  if (page.kind !== "ok") return page;
  const edit: ModelAccessEdit = { kind: "create_pool", pool: { id, name: input.value.name, members: input.value.members, projects: [], roles: [], fallback_allowed: false } as AccountPool };
  return change("desktop_edit_model_access", { ticket: page.value.ticket, edit, generation: nextGeneration() });
}

/** Saves the owner's changes to a pool while keeping its project assignments and any change made
 * elsewhere since the owner read it (see `mergePool`). Inputs: pool id, name, member ids, and its
 * name and members as the owner read them. Output: ok once the engine saved it. */
export async function savePool(id: string, name: string, members: string[], opened: PoolBase): Promise<Result<void>> {
  const input = poolInput(name, members);
  if (input.kind !== "ok") return input;
  const detail = await call<ModelAccessDetail>("desktop_inspect_model_access", { collection: "pools", id });
  if (detail.kind !== "ok") return detail;
  if (detail.value.record.kind !== "pool") return { kind: "error", code: "pool_not_found", message: plainError("pool_not_found") };
  const now = detail.value.record.pool;
  const merged = mergePool({ name: now.name ?? "", members: now.members }, opened, input.value);
  if (merged.kind !== "ok") return merged;
  const edit: ModelAccessEdit = { kind: "replace_pool", pool: { ...now, name: merged.value.name, members: merged.value.members } as AccountPool };
  return change("desktop_edit_model_access", { ticket: detail.value.ticket, edit, generation: nextGeneration() });
}

/** Deletes a pool and takes it off every project that uses it, in one engine change.
 * Input: pool id. Output: ok once deleted; those projects then have no pool. */
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
  team: TeamUse;
  aliases: string[];
}

/** Reads the harness catalog with what is installed on this Mac, from the engine's last look
 * unless `check` asks it to look again now. Input: whether to look again. Output: harnesses in
 * catalog priority order. */
export function harnessCatalog(check = false): Promise<Result<HarnessEntry[]>> {
  return call<HarnessEntry[]>("desktop_harness_catalog", { check });
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

/** Turns a skill on or off where it was added. Inputs: skill id, the switch and the version
 * the list showed; a skill changed since is refused with `stale_revision`. */
export function setSkillEnabled(id: string, enabled: boolean, version: number): Promise<Result<SkillView>> {
  return call<SkillView>("desktop_set_skill_enabled", { id, enabled, version });
}

/** Reads hooks for global settings (no project) or one project's settings, where global
 * hooks carry the project's own switch. Output: hooks in engine order. */
export function listHooks(projectId?: string): Promise<Result<HookView[]>> {
  return call<HookView[]>("desktop_plugin_hooks", { projectId: projectId ?? null });
}

/** Adds the command hooks a plugin folder defines, each turned on; nothing in the folder runs.
 * Inputs: where they apply and the folder's full path. Output: the added hooks. */
export function addHooks(scope: PluginScope, folder: string): Promise<Result<HookView[]>> {
  return call<HookView[]>("desktop_add_hooks", { scope, folder });
}

/** Turns a hook on or off where it was added. Inputs: hook id and the switch. */
export function setHookEnabled(id: string, enabled: boolean): Promise<Result<HookView>> {
  return call<HookView>("desktop_set_hook_enabled", { id, enabled });
}

/** Removes a skill and the engine's copy of its folder; the owner's folder stays. */
export function removeSkill(id: string, version: number): Promise<Result<void>> {
  return call<void>("desktop_remove_skill", { id, version });
}

/** Reads one skill for its detail view: the skill with the project's switch when a project
 * is given, its full SKILL.md text and its files. Inputs: skill id and optional project id.
 * Output: the detail, or the engine's refusal such as a removed skill. */
export function skillDetail(id: string, projectId?: string): Promise<Result<SkillDetail>> {
  return call<SkillDetail>("desktop_skill_detail", { id, projectId: projectId ?? null });
}

/** Looks for skills in the owner's `~/.claude/skills` and `~/.codex/skills`, read only when
 * the owner asks. Output: each skill found with its state against the app's skills. */
export function skillImportScan(): Promise<Result<ImportList>> {
  return call<ImportList>("desktop_skill_import_scan", {});
}

/** Imports one ticked skill into the app's skills, turned on for every project, leaving out
 * dotfiles and credential-like files. Inputs: the found folder and, for a skill whose files
 * differ, the added skill's id to replace. Output: the skill and the paths left out. */
export function importSkill(folder: string, replace: string | null): Promise<Result<ImportedSkill>> {
  return call<ImportedSkill>("desktop_import_skill", { folder, replace });
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

/** Turns an MCP server on or off where it was added. Inputs: server id, the switch and the
 * version the list showed; a server changed since is refused with `stale_revision`. */
export function setMcpServerEnabled(id: string, enabled: boolean, version: number): Promise<Result<McpServerView>> {
  return call<McpServerView>("desktop_set_mcp_server_enabled", { id, enabled, version });
}

/** Removes an MCP server and its saved secrets. Input: server id. */
export function removeMcpServer(id: string, version: number): Promise<Result<void>> {
  return call<void>("desktop_remove_mcp_server", { id, version });
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
 * Inputs: project id, item id, the switch and the item's version the list showed. */
export function switchPlugin(projectId: string, id: string, on: boolean, version: number): Promise<Result<void>> {
  return call<void>("desktop_switch_plugin", { projectId, id, on, version });
}

/** Reads tool switches: the CrazyAgent built-in tools (no server id) or one MCP server's
 * checked tools, for global settings (no project) or one project. Output: each tool with its
 * every-project switch and the project's own switch. */
export function toolSwitches(projectId: string | undefined, serverId: string | null): Promise<Result<ToolView[]>> {
  return call<ToolView[]>("desktop_tool_switches", { projectId: projectId ?? null, serverId });
}

/** Sets one tool's switch for every project (no project) or for one project; hidden deletes a
 * built-in tool until Restore defaults. Inputs: project, server id (null for built-in), the tool
 * and the switch. Output: the server's tools after the change. */
export function setToolSwitch(projectId: string | undefined, serverId: string | null, tool: string, value: ToolSwitch): Promise<Result<ToolView[]>> {
  return call<ToolView[]>("desktop_set_tool_switch", { projectId: projectId ?? null, serverId, tool, value });
}

/** Clears every tool switch of one server in that scope, bringing deleted built-in tools back.
 * Inputs: project (none for every project) and server id (null for built-in). Output: the tools. */
export function restoreToolSwitches(projectId: string | undefined, serverId: string | null): Promise<Result<ToolView[]>> {
  return call<ToolView[]>("desktop_restore_tool_switches", { projectId: projectId ?? null, serverId });
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

/** Reads everything that uses one account's sign-in, so a refused sign-out can name it.
 * Input: the account id. Output: its running agents and other holders, newest first. */
export function accountUse(accountId: string): Promise<Result<AccountUse>> {
  return call<AccountUse>("desktop_account_use", { accountId });
}

/** Ends everything that uses one account's sign-in and waits for it to end, so the account
 * can be signed out: agents and research jobs stop, master conversations end.
 * Input: the account id. Output: what still uses it, nothing once all have ended. */
export function releaseAccount(accountId: string): Promise<Result<AccountUse>> {
  return call<AccountUse>("desktop_release_account", { accountId, generation: nextGeneration() });
}

/** What the native folder chooser is for: a skill folder, an existing project folder to add,
 * the folder that holds the owner's projects, a folder of memory files to import, or a plugin
 * folder whose hooks are added. Each shows its own fixed prompt. */
export type FolderPurpose = "skill" | "project" | "projects_home" | "memory" | "plugin";

/** Opens the native folder chooser for one purpose. Input: what the folder is for. Output: the
 * chosen folder's full path, or null when the owner closed the chooser. */
export function pickFolder(purpose: FolderPurpose): Promise<Result<string | null>> {
  return call<string | null>("desktop_pick_folder", { purpose });
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

/** How an import of a memory folder ended: memories saved and markdown files not saved. */
export interface MemoryImportCounts {
  imported: number;
  skipped: number;
}

/** Imports the markdown memory files of a folder the owner chose into one project's memory,
 * one memory per file. Inputs: the project id and the folder's full path. Output: the counts,
 * or the plain error of a change that was refused or is still unconfirmed. */
export async function importMemory(project: string, folder: string): Promise<Result<MemoryImportCounts>> {
  const result = await call<MemoryImportCounts & { stopped: IntentReceipt | null }>("desktop_import_memory", { project, folder, generation: nextGeneration() });
  if (result.kind !== "ok") return result;
  if (result.value.stopped) {
    const stopped = receiptResult(result.value.stopped);
    if (stopped.kind !== "ok") return stopped;
  }
  return { kind: "ok", value: { imported: result.value.imported, skipped: result.value.skipped } };
}

/** Deletes one saved memory for good. Input: memory id. Output: ok once deleted. */
export function deleteMemory(id: string): Promise<Result<void>> {
  return change("desktop_delete_memory", { id, generation: nextGeneration() });
}

/** Approves one memory an agent asked to save, so agents get it from their next launch.
 * Input: memory id. Output: ok once approved. */
export function approveMemory(id: string): Promise<Result<void>> {
  return change("desktop_approve_memory", { id, generation: nextGeneration() });
}

/** Saves the owner's Agent memory writes setting: agents save directly, or ask first.
 * Input: the choice. Output: ok once saved. */
export function setAgentMemoryWrites(writes: AgentMemoryWrites): Promise<Result<void>> {
  return change("desktop_set_agent_memory_writes", { writes, generation: nextGeneration() });
}

/** Picks the research memories in a project nobody reviewed yet. Inputs: the project and its
 * memory list. Output: the suggestions, in engine order. */
export function suggestionsFrom(project: { id: string; name: string }, items: MemoryItem[]): Suggestion[] {
  return items.filter((item) => item.status === "Provisional" && !item.human_reviewed)
    .map((item) => ({ project_id: project.id, project_name: project.name, id: item.id, version: item.version, text: item.text }));
}

export const MEMORY_PAGES_MAX = 20;

/** Reads a project's memories, research saved for approval included, batch after batch up to
 * MEMORY_PAGES_MAX batches, for every screen that lists or counts them; one engine batch holds
 * at most 8 entries of any status. Input: the project id. Output: the memories in id order and
 * whether more remain unread. */
export async function loadAllProjectMemories(id: string): Promise<Result<{ items: MemoryItem[]; more: boolean }>> {
  const items: MemoryItem[] = [];
  let after: string | null = null;
  for (let page = 0; page < MEMORY_PAGES_MAX; page += 1) {
    const batch: Result<MemoryItem[]> = await call<MemoryItem[]>("desktop_project_memories", { id, after });
    if (batch.kind !== "ok") return batch;
    if (batch.value.length === 0) return { kind: "ok", value: { items, more: false } };
    items.push(...batch.value);
    after = batch.value[batch.value.length - 1].id;
  }
  return { kind: "ok", value: { items, more: true } };
}

/** Reads unreviewed research memories across projects (every memory batch of each, up to 50
 * projects). Input: the projects. Output: the suggestions. */
export async function memorySuggestions(projects: { id: string; name: string }[]): Promise<Result<Suggestion[]>> {
  const found: Suggestion[] = [];
  for (const project of projects.slice(0, SUGGESTION_PROJECTS_MAX)) {
    const memories = await loadAllProjectMemories(project.id);
    if (memories.kind !== "ok") return memories;
    found.push(...suggestionsFrom(project, memories.value.items));
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

/** The most projects Home, the sidebar and Go to list, the bridge's PROJECT_LIST_MAX. */
export const PROJECT_LIST_MAX = 200;

/** Reads the projects for Home, the sidebar and Go to in one bridge call that walks every
 * page with its own cursor, up to PROJECT_LIST_MAX. Input: none. Output: the projects in
 * engine order and whether more exist, or the read's error. */
export function listProjects(): Promise<Result<ProjectList>> {
  return call<ProjectList>("desktop_project_list");
}

/** Reads one project's team graph: master, harness groups, agents and task edges.
 * Input: the project id. Output: the graph, not ready, or a plain error. */
export function loadTeamGraph(id: string): Promise<Result<TeamGraph>> {
  return call<TeamGraph>("desktop_team_graph", { id });
}

/** Reads one project's Questions inbox: the open questions agents put to the owner, most
 * urgent first, and the agents waiting on the owner in their own terminal.
 * Input: the project id. Output: the inbox, not ready, or a plain error. */
export function loadQuestions(id: string): Promise<Result<QuestionsInbox>> {
  return call<QuestionsInbox>("desktop_coord_questions", { id });
}

/** Answers one open question an agent put to the owner; it reaches the asker as the owner's
 * reply. Inputs: the project id, the question's message id and the answer. Output: the engine
 * receipt. */
export function answerQuestion(id: string, message: number, text: string): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_coord_answer", { id, message, text, generation: nextGeneration() });
}

/** Opens a live view of one agent's terminal on a channel: the attach frame, the screen,
 * then live output. With a pane, the view is that one pane of the terminal's window, such as
 * a native teammate's. A wall window names its project, whose graph must show the terminal.
 * Inputs: the terminal name, the channel, the pane id and the project id, if any.
 * Output: the view's serial once attached, which its detach names. */
export function attachTerminal(agent: string, output: Channel<TerminalMessage>, pane: string | null = null, project: string | null = null): Promise<Result<number>> {
  return call<number>("desktop_terminal_attach", { agent, pane, output, id: project });
}

/** Types one chunk of bytes into an attached agent terminal. Inputs: the terminal name, 1 to
 * 4096 bytes and whether the view made them from the mouse wheel, which the engine never
 * counts as the owner typing. Output: ok once queued in order. */
export function typeIntoTerminal(agent: string, bytes: number[], wheel: boolean = false): Promise<Result<void>> {
  return call<void>("desktop_terminal_input", { agent, bytes, wheel });
}

/** Sends the owner's explicit prompt to an attached agent terminal: the engine types it as one
 * submission and tells a sub-agent's master. Inputs: the terminal name and the prompt, up to
 * 4096 bytes. Output: ok once queued in order with typed keys. */
export function promptTerminal(agent: string, text: string): Promise<Result<void>> {
  return call<void>("desktop_terminal_prompt", { agent, text });
}

/** Sets an attached agent terminal's size. Inputs: the terminal name, columns and rows.
 * Output: ok once queued. */
export function resizeTerminal(agent: string, cols: number, rows: number): Promise<Result<void>> {
  return call<void>("desktop_terminal_resize", { agent, cols, rows });
}

/** Closes this window's view of an agent terminal; the agent keeps running. A newer view
 * of the same agent stays open. Inputs: the terminal name and the serial its attach
 * returned. Output: ok once closed. */
export function detachTerminal(agent: string, serial: number): Promise<Result<void>> {
  return call<void>("desktop_terminal_detach", { agent, serial });
}

/** Turns keeping the team's minimum sub-agents running on or off. Inputs: the project id,
 * the switch and the team settings version the screen read. Output: the engine receipt, not
 * ready, or a plain error; a save from an older read is rejected as `stale_revision`. */
export function keepTeamWarm(id: string, on: boolean, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_keep_warm", { id, on, version, generation: nextGeneration() });
}

/** Sets how long one task turn and one agent session of the team may run, and how long a
 * sub-agent may sit idle; null is no limit. Inputs: the project id, the three limits in
 * seconds and the team settings version the screen read. Output: the engine receipt, not
 * ready, or a plain error; a save from an older read is rejected as `stale_revision`. */
export function setTeamTimeLimits(id: string, turnSecs: number | null, sessionSecs: number | null, idleSecs: number | null, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_time_limits", { id, turnSecs, sessionSecs, idleSecs, version, generation: nextGeneration() });
}

/** Sets what the live master does with the conversation when the master harness changes.
 * Inputs: the project id, the mode and the team settings version the screen read. Output:
 * the engine receipt, not ready, or a plain error; a save from an older read is rejected as
 * `stale_revision`. */
export function setOnHarnessSwitch(id: string, mode: SwitchMode, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_on_harness_switch", { id, mode, version, generation: nextGeneration() });
}

/** Sets when a saved master harness change reaches the running live master: once it is idle, or
 * at its next start. Inputs: the project id, the choice and the team settings version the
 * screen read. Output: the engine receipt, not ready, or a plain error; a save from an older
 * read is rejected as `stale_revision`. */
export function setApplyHarnessSwitch(id: string, mode: SwitchApply, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_apply_harness_switch", { id, mode, version, generation: nextGeneration() });
}

/** Sets how deep sub-agents may start workers of their own: 1, 2 or 3. Inputs: the project id
 * and the depth. Output: the engine receipt, not ready, or a plain error. */
export function setSubAgentDepth(id: string, depth: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_sub_agent_depth", { id, depth, generation: nextGeneration() });
}

/** Sets where a lead's worker lands its change set: when the lead accepts it, or only when the
 * owner does. Inputs: the project id and the mode. Output: the engine receipt, not ready, or a
 * plain error. */
export function setChildLanding(id: string, mode: ChildLanding): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_child_landing", { id, mode, generation: nextGeneration() });
}

/** Sets where the team's finished agents show: in the agents list and graph (Show) or only in
 * the Finished fold-out (Hide). Inputs: the project id, true for Show and the team settings
 * version the screen read. Output: the engine receipt, not ready, or a plain error; a save
 * from an older read is rejected as `stale_revision`. */
export function showFinishedAgents(id: string, show: boolean, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_finished_agents", { id, show, version, generation: nextGeneration() });
}

/** Sets where the project's new sub-agents commit: each on its own branch crazyagent/agent-<id>
 * (named) or on a detached HEAD (detached). Inputs: the project id and the mode. Output: the
 * engine receipt, not ready, or a plain error. */
export function setAgentBranches(id: string, mode: BranchMode): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_agent_branches", { id, mode, generation: nextGeneration() });
}

/** Stops every agent of the project, the master, each harness and each teammate, until the
 * owner starts the team again, or starts it again. Inputs: the project id and true to stop.
 * Output: the engine receipt, not ready, or a plain error. */
export function setTeamStopped(id: string, stopped: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_pause", { id, paused: stopped, generation: nextGeneration() });
}

/** Sets Max harnesses per project: how many harnesses run at once, null for No limit. Inputs:
 * the project id and the cap. Output: the engine receipt, not ready, or a plain error. */
export function setMaxHarnesses(id: string, max: number | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_max_harnesses", { id, max, generation: nextGeneration() });
}

/** Sets Max teammates per harness, null for No limit. Inputs: the project id and the cap.
 * Output: the engine receipt, not ready, or a plain error. */
export function setMaxTeammates(id: string, max: number | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_max_teammates", { id, max, generation: nextGeneration() });
}

/** Sets the project's Token budget, null for Off: once its agents use that many tokens from now,
 * the engine stops the whole team. Inputs: the project id and the tokens. Output: the engine
 * receipt, not ready, or a plain error. */
export function setTokenBudget(id: string, tokens: number | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_token_budget", { id, tokens, generation: nextGeneration() });
}

/** Turns the project's Spread harnesses across accounts on or off: on, a new or woken agent
 * prefers a pool account no running agent of the project uses, else the account strategy
 * picks. Inputs: the project id and the switch. Output: the engine receipt, not ready, or a
 * plain error. */
export function setSpreadAccounts(id: string, on: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_spread_accounts", { id, on, generation: nextGeneration() });
}

/** Sets the branch the project's master works on and merges finished harnesses into, or
 * clears it back to the repository's default branch. Inputs: the project id and the branch,
 * null for the default. Output: the engine receipt, not ready, or a plain error. */
export function setMasterBranch(id: string, branch: string | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_master_branch", { id, branch, generation: nextGeneration() });
}

/** Sets the branch the project's team work lands on: the master's merges, an Accept with no
 * branch picked and what each change set counts. Inputs: the project id and the branch, null
 * for the master's branch. Output: the engine receipt, not ready, or a plain error. */
export function setLandOn(id: string, branch: string | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_land_on", { id, branch, generation: nextGeneration() });
}

/** Sets what the project's tasks do at their turn end while commands their agents started in
 * the background still run. Inputs: the project id and the choice. Output: the engine
 * receipt, not ready, or a plain error; the engine refuses a grace time outside 10 to 3600 s. */
export function setBackgroundAtEnd(id: string, choice: BackgroundAtEnd): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_background", { id, choice, generation: nextGeneration() });
}

/** Turns the project's "Ask me before merging into the master's branch" setting on or off;
 * on, the master's merge of a finished harness waits for the owner's Accept in Changes.
 * Inputs: the project id and the switch. Output: the engine receipt, not ready, or a plain
 * error. */
export function setAskBeforeMerge(id: string, on: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_ask_before_merge", { id, on, generation: nextGeneration() });
}

/** Sets what a Full-access master does with its own conflict fixes: merge them into main
 * itself, or leave them waiting for the owner's Accept in Changes. Inputs: the project id and
 * the choice. Output: the engine receipt, not ready, or a plain error. */
export function setMasterLands(id: string, mode: MasterLands): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_master_lands", { id, mode, generation: nextGeneration() });
}

/** Turns the project's "Re-ask a sub-agent's empty reply" setting on or off; on, a
 * sub-agent whose task ends with no reply text is asked once more for its reply. Inputs:
 * the project id and the switch. Output: the engine receipt, not ready, or a plain error. */
export function setReaskEmptyReply(id: string, on: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_reask_empty_reply", { id, on, generation: nextGeneration() });
}

/** Sets how many terminals Open all terminals puts in one terminal wall window. Inputs: the
 * project id and the choice, 4, 6 or 9. Output: the engine receipt, not ready, or a plain
 * error. */
export function setTerminalsPerWindow(id: string, perWindow: 4 | 6 | 9): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_terminals_per_window", { id, perWindow, generation: nextGeneration() });
}

/** Reads how many terminals one terminal wall window holds; the wall windows may call it.
 * Input: the project id. Output: 4, 6 or 9, not ready, or a plain error. */
export function loadTerminalsPerWindow(id: string): Promise<Result<4 | 6 | 9>> {
  return call<4 | 6 | 9>("desktop_terminals_per_window", { id });
}

/** Opens or updates the project's terminal wall windows: count windows titled with the
 * project's name, closing any past the count. With focus they come to the front; without it
 * nothing opens unless the project has one open already, and none closes. Inputs: the project id, its name,
 * the window count and whether to focus them. Output: nothing, not ready, or a plain error. */
export function openTerminalWindows(id: string, name: string, count: number, focus: boolean): Promise<Result<void>> {
  return call<void>("desktop_open_terminal_windows", { id, name, count, focus });
}

/** Asks for the wall windows a wall window's growing team needs, from that wall window: the
 * missing pages open beside it and none closes. Inputs: the project id and the window
 * count. Output: nothing, not ready, or a plain error. */
export function extendTerminalWindows(id: string, count: number): Promise<Result<void>> {
  return call<void>("desktop_extend_terminal_windows", { id, count });
}

/** Brings forward, from a terminal wall window, the main window on its project's Agents tab
 * or another wall window of its own project, as the wall's Back to Agents and Window switch
 * do. Inputs: the project id and the target, "main" or a page. Output: nothing, not ready,
 * or a plain error. */
export function focusTerminalWindow(id: string, target: "main" | number): Promise<Result<void>> {
  return call<void>("desktop_focus_terminal_window", { id, target: String(target) });
}

/** Closes a removed project's terminal wall windows, for the main window or a wall window of
 * that project. Input: the project id. Output: nothing, not ready, or a plain error. */
export function closeProjectWalls(id: string): Promise<Result<void>> {
  return call<void>("desktop_close_project_walls", { id });
}

/** Sets the project's "Catch up with main" setting for agents started from now on: on lets each
 * agent merge the landed main, `crazyagent/main`, into its own branch and tells it to; off makes the
 * git the app writes for an agent refuse that merge, except a master at Full access, and is a guide,
 * not a lock: landed work can still come in another way.
 * Inputs: the project id and the mode. Output: the engine receipt, not ready, or a plain error. */
export function setCatchUpMain(id: string, mode: CatchUpMain): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_catch_up_main", { id, mode, generation: nextGeneration() });
}

/** Saves a project's "Stop the master when idle after" choice (DEC-117 Q4). Inputs: the
 * project id and the choice. Output: the engine receipt, not ready, or a plain error. */
export function setMasterIdleStop(id: string, choice: MasterIdleStop): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_master_idle_stop", { id, choice, generation: nextGeneration() });
}

/** Saves a project's "Live terminal" setting (DEC-117 Q4). Inputs: the project id and whether
 * it is on. Output: the engine receipt, not ready, or a plain error. */
export function setMasterLiveTerminal(id: string, on: boolean): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_master_live_terminal", { id, on, generation: nextGeneration() });
}

/** Sets the project's "Reuse idle agents" setting: off starts a new agent for each new task, on
 * the current branch; on lets an idle agent of the same lead take it, on the history it already has.
 * Inputs: the project id and the mode. Output: the engine receipt, not ready, or a plain error. */
export function setReuseIdle(id: string, mode: ReuseIdle): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_reuse_idle", { id, mode, generation: nextGeneration() });
}

/** Sets the most files one new top-level folder, a folder the owner's branch does not have,
 * may gain in a project's change set before the engine refuses the set. Inputs: the project id
 * and the limit. Output: the engine receipt, not ready, or a plain error. */
export function setNewFolderLimit(id: string, files: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_new_folder_limit", { id, files, generation: nextGeneration() });
}

/** Saves a project's coordination settings: where sub-agent questions go, what overlapping
 * claims do, workflow nudges and the counted tools. Inputs: the project id and the settings.
 * Output: the engine receipt, not ready, or a plain error. */
export function setCoordSettings(id: string, settings: CoordSettings): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_coordination", { id, settings, generation: nextGeneration() });
}

/** Sets how one harness kind's sub-agents start in a project: app-spawned, or native teammates
 * where that kind has them. Inputs: the project id, the harness kind and the mode. Output: the
 * engine receipt, not ready, or a plain error. */
export function setHarnessMode(id: string, harness: CoordHarness, mode: SubagentMode): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_harness_mode", { id, harness, mode, generation: nextGeneration() });
}

/** Sets how messages reach one harness kind's agents in a project. Inputs: the project id, the
 * harness kind and the delivery. Output: the engine receipt, not ready, or a plain error. */
export function setHarnessDelivery(id: string, harness: CoordHarness, delivery: Delivery): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_harness_delivery", { id, harness, delivery, generation: nextGeneration() });
}

/** Turns the project's live master on or off; on, the engine keeps the master's own app running
 * in a terminal. Inputs: the project id, the switch and the team settings version the screen
 * read. Output: the engine receipt, not ready, or a plain error; a save from an older read is
 * rejected as `stale_revision`. */
export function setLiveMaster(id: string, on: boolean, version: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_live_master", { id, on, version, generation: nextGeneration() });
}

/** Hands one task straight to a sub-agent. Inputs: the project id, a harness of the team plan
 * or null for any, and the task. Output: the engine receipt, not ready, or a plain error. */
export function delegateTeamTask(id: string, harness: string | null, prompt: string): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_delegate", { id, harness, prompt, generation: nextGeneration() });
}
