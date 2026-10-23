import type { ModelAccessDetail } from "./engine-client";
import { displayText } from "./screens/conversation";

export type NativeAuthMethod = "codex_browser" | "codex_device_code" | "codex_existing" | "claude_official";
export type NativeAuthCommand = NativeAuthMethod | "read" | "cancel" | "logout";
export type NativeAuthAction = { kind: "sign_in"; method: NativeAuthMethod } | { kind: "logout" };
export type NativeAuthPhase = "starting" | "awaiting_owner" | "checking_account" |
  "authenticated_observed" | "unauthenticated_observed" | "cancelling" |
  "unavailable" | "uncertain";
type StatusField = { state: "missing" } | { state: "null" } | { state: "value"; text: string };
type Challenge = { kind: "codex_browser"; auth_url: string } |
  { kind: "codex_device_code"; user_code: string; verification_url: string } |
  { kind: "claude_official"; auth_url: string; accepts_code: boolean; prompt_id: "1" | null };
type Account = { kind: "codex_api_key" } |
  { kind: "codex_chatgpt"; email: string | null; plan_type: string } |
  { kind: "codex_amazon_bedrock"; uses_codex_managed_credentials: boolean } |
  { kind: "claude"; logged_in: boolean; auth_method: string; api_provider: string;
    config_directory: string; email: StatusField; org_id: StatusField; org_name: StatusField;
    subscription_type: StatusField };

export interface NativeAuthView {
  ticket: string;
  request_id: string;
  operation_id: string;
  database_instance: string;
  engine_epoch: string;
  connection: string;
  config_id: string;
  source_hash: string;
  generation: string;
  action: NativeAuthAction;
  phase: NativeAuthPhase;
  code: string | null;
  challenge: Challenge | null;
  account: Account | null;
  display_error?: string | null;
}

interface Context {
  ticket: string;
  connection: string;
  config_id: string;
  source_hash: string;
  navigation: number;
  harness: string;
}

type Operation = Omit<NativeAuthView,
  "ticket" | "request_id" | "challenge" | "account" | "display_error">;

export interface NativeAuthState {
  open: boolean;
  context: Context;
  valid: boolean;
  in_flight?: { action: NativeAuthCommand; request: number };
  operation?: Operation;
  presentation?: NativeAuthView;
  confirming_logout: boolean;
  error?: string;
  invalidated?: boolean;
  rebound?: boolean;
  recover?: boolean;
  recoverAction?: NativeAuthAction;
  recoveryContext?: Context;
  claude_input?: { operation_id: string; generation: string; prompt_id: "1";
    request: number; status: "sending" | "accepted" | "uncertain" };
}

type Event = { kind: "open"; detail: ModelAccessDetail; navigation: number } |
  { kind: "begin"; action: NativeAuthCommand; request: number } |
  { kind: "reply"; navigation: number; request: number; reply: NativeAuthView } |
  { kind: "failure"; navigation: number; request: number; code: string } |
  { kind: "claude_input_begin"; request: number } |
  { kind: "claude_input_accepted" | "claude_input_failure"; navigation: number; request: number } |
  { kind: "close" | "invalidate" | "confirm_logout" | "back" };

type Target = Pick<Context, "connection" | "config_id" | "source_hash">;

function sameTarget(left: Target, right: Target): boolean {
  return left.connection === right.connection && left.config_id === right.config_id &&
    left.source_hash === right.source_hash;
}

function open(state: NativeAuthState | undefined, detail: ModelAccessDetail,
  navigation: number): NativeAuthState {
  const connection = detail.record.kind === "connection" ? detail.record.connection : undefined;
  const context = { ticket: detail.ticket, config_id: detail.config_id,
    source_hash: detail.source_hash, navigation, connection: connection?.id ?? "",
    harness: connection?.harness ?? "" };
  const valid = detail.fresh && detail.collection === "connections" &&
    connection?.protocol === "native" && connection.endpoint.kind === "native" &&
    connection.auth.method === "native_managed" && ["codex", "claude"].includes(connection.harness);
  const interrupted = Boolean(state?.in_flight);
  return { open: true, context, valid: Boolean(valid), confirming_logout: false,
    operation: state?.operation,
    claude_input: state?.claude_input,
    recover: state?.recover || interrupted,
    recoverAction: state?.recoverAction ?? (state?.in_flight && actionFor(state.in_flight.action)),
    recoveryContext: state?.recoveryContext ?? (interrupted ? state?.context : undefined),
    rebound: Boolean(state?.invalidated && sameTarget(state.context, context)),
    error: !detail.fresh ? "stale_model_access" : valid ? undefined :
      "native_profile_not_managed" };
}

export function nativeAuthActive(state: NativeAuthState | undefined): boolean {
  return Boolean(state?.recover || state?.in_flight || state?.operation &&
    ["starting", "awaiting_owner", "checking_account", "cancelling", "uncertain"]
      .includes(state.operation.phase));
}

export function nativeAuthAllowed(state: NativeAuthState, action: NativeAuthCommand): boolean {
  if (!state.open || !state.valid || state.in_flight || state.claude_input?.status === "sending") return false;
  if (action === "read" || action === "cancel") {
    const target = state.operation ?? state.recoveryContext;
    return Boolean(target && sameTarget(target, state.context));
  }
  if (nativeAuthActive(state)) return false;
  if (action === "logout") return state.confirming_logout;
  return state.context.harness === "codex" ?
    action === "codex_browser" || action === "codex_device_code" || action === "codex_existing" :
    state.context.harness === "claude" && action === "claude_official";
}

function actionFor(command: NativeAuthCommand): NativeAuthAction | undefined {
  if (command === "logout") return { kind: "logout" };
  if (command === "read" || command === "cancel") return undefined;
  return { kind: "sign_in", method: command };
}

function sameAction(left: NativeAuthAction, right: NativeAuthAction): boolean {
  return left.kind === right.kind && (left.kind === "logout" ||
    right.kind === "sign_in" && left.method === right.method);
}

function matchesReply(state: NativeAuthState, reply: NativeAuthView): boolean {
  const target = state.context;
  if (reply.ticket !== target.ticket || reply.connection !== target.connection ||
    reply.config_id !== target.config_id || reply.source_hash !== target.source_hash ||
    !/^[1-9][0-9]{0,19}$/.test(reply.generation)) return false;
  const command = state.in_flight?.action;
  const action = command && actionFor(command);
  if (action) return sameAction(reply.action, action) && reply.request_id === reply.operation_id;
  const original = state.operation;
  if (!original) {
    return Boolean(state.recoverAction && sameAction(reply.action, state.recoverAction));
  }
  const epoch = reply.engine_epoch === original.engine_epoch ||
    state.rebound && !reply.challenge && !reply.account;
  return reply.operation_id === original.operation_id &&
    reply.database_instance === original.database_instance && Boolean(epoch) &&
    reply.generation === original.generation && sameAction(reply.action, original.action);
}

export function nativeAuthText(value: string): string {
  return displayText(value).replace(/\n/g, "\\n").replace(/\t/g, "\\t");
}

export function safeChallenge(challenge: Challenge): boolean {
  if (challenge.kind === "claude_official") {
    const prefix = "https://claude.com/cai/oauth/authorize?";
    return typeof challenge.auth_url === "string" && challenge.auth_url.length <= 8192 &&
      challenge.auth_url.startsWith(prefix) && challenge.auth_url.length > prefix.length &&
      !/[^\x21-\x7e]|[#\\]/u.test(challenge.auth_url.slice(prefix.length)) &&
      (challenge.accepts_code === true && challenge.prompt_id === "1" ||
        challenge.accepts_code === false && challenge.prompt_id === null);
  }
  const fields = challenge.kind === "codex_browser" ? [challenge.auth_url] :
    challenge.kind === "codex_device_code" ? [challenge.user_code, challenge.verification_url] : [];
  return fields.length > 0 && fields.every((value) => typeof value === "string" &&
    !/[\u0000-\u001f\u007f-\u009f]/u.test(value) &&
    !/[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(value));
}

export function validateClaudeAuthCode(value: unknown): void {
  if (typeof value !== "string" || value.length > 8192 ||
    !/^[\x21-\x22\x24-\x7e]+#[\x21-\x22\x24-\x7e]+$/u.test(value)) {
    throw new Error("native_auth_claude_input_invalid");
  }
}

export function nativeClaudeInputAllowed(state: NativeAuthState | undefined, blocked: boolean): boolean {
  if (!state?.open || !state.valid || blocked || state.invalidated || state.rebound ||
    state.recover || state.in_flight || state.context.harness !== "claude") return false;
  const operation = state.operation;
  const observed = state.presentation;
  if (!operation || !observed || observed.display_error || observed.account ||
    operation.phase !== "awaiting_owner" || observed.phase !== "awaiting_owner" ||
    operation.action.kind !== "sign_in" || operation.action.method !== "claude_official" ||
    !sameAction(operation.action, observed.action) ||
    !sameTarget(operation, state.context) || observed.ticket !== state.context.ticket ||
    !/^[1-9][0-9]{0,19}$/.test(operation.generation) ||
    observed.challenge?.kind !== "claude_official" || !safeChallenge(observed.challenge) ||
    !observed.challenge.accepts_code || observed.challenge.prompt_id !== "1") return false;
  const fields = ["operation_id", "database_instance", "engine_epoch", "connection",
    "config_id", "source_hash", "generation"] as const;
  if (fields.some((field) => observed[field] !== operation[field])) return false;
  return !(state.claude_input?.operation_id === operation.operation_id &&
    state.claude_input.generation === operation.generation);
}

function presentationError(reply: NativeAuthView): string | undefined {
  const bytes = new TextEncoder().encode(JSON.stringify(reply)).length;
  const escaped = nativeAuthText(JSON.stringify(reply)).replace(/[&<>"']/g, "&quot;");
  if (bytes > 65536 || new TextEncoder().encode(escaped).length > 60000) {
    return "native_auth_presentation_limit";
  }
  if (reply.challenge && !safeChallenge(reply.challenge)) {
    return "native_auth_cannot_display_safely";
  }
  return reply.display_error ?? undefined;
}

function receipt(state: NativeAuthState, reply: NativeAuthView): NativeAuthState {
  if (!state.in_flight) throw new Error("native_auth_dispatch_invariant");
  const operation = { operation_id: reply.operation_id, database_instance: reply.database_instance,
    engine_epoch: reply.engine_epoch, connection: reply.connection, config_id: reply.config_id,
    source_hash: reply.source_hash, generation: reply.generation, action: reply.action,
    phase: reply.phase, code: reply.code };
  const error = presentationError(reply);
  return { ...state, in_flight: undefined, operation, error, rebound: false, recover: false,
    recoverAction: undefined, recoveryContext: undefined,
    presentation: state.open && state.valid && !error ? reply : undefined };
}

function failed(state: NativeAuthState, code: string): NativeAuthState {
  const action = state.in_flight && actionFor(state.in_flight.action);
  const recover = code === "native_auth_outcome_unknown_read_same_operation";
  return { ...state, presentation: undefined, in_flight: undefined, error: code,
    recover: Boolean(state.recover || recover), recoverAction: action ?? state.recoverAction,
    recoveryContext: state.recoveryContext ?? state.context,
    valid: false, invalidated: true };
}

export function reduceNativeAuth(state: NativeAuthState | undefined,
  event: Event): NativeAuthState | undefined {
  if (event.kind === "open") return open(state, event.detail, event.navigation);
  if (!state) return undefined;
  if (event.kind === "claude_input_begin") {
    if (!nativeClaudeInputAllowed(state, false) || !Number.isSafeInteger(event.request) ||
      event.request <= 0) return state;
    const operation = state.operation;
    if (!operation) throw new Error("native_auth_dispatch_invariant");
    const next: NativeAuthState = { ...state, presentation: undefined, error: undefined,
      claude_input: { operation_id: operation.operation_id, generation: operation.generation,
        prompt_id: "1", request: event.request, status: "sending" } };
    if (next.presentation || next.claude_input?.operation_id !== operation.operation_id ||
      next.claude_input.request !== event.request) throw new Error("native_auth_dispatch_invariant");
    return next;
  }
  if (event.kind === "claude_input_accepted" || event.kind === "claude_input_failure") {
    const attempt = state.claude_input;
    const operation = state.operation;
    if (!attempt || !operation || event.navigation !== state.context.navigation ||
      event.request !== attempt.request || attempt.status !== "sending" ||
      attempt.operation_id !== operation.operation_id || attempt.generation !== operation.generation) return state;
    return { ...state, presentation: undefined,
      claude_input: { ...attempt, status: event.kind === "claude_input_accepted" ? "accepted" : "uncertain" },
      error: event.kind === "claude_input_failure" ? "native_auth_claude_input_outcome_unknown" : undefined };
  }
  if (event.kind === "close") return { ...state, open: false, presentation: undefined,
    confirming_logout: false };
  if (event.kind === "invalidate") return { ...state, valid: false, invalidated: true,
    presentation: undefined, confirming_logout: false };
  if (event.kind === "back") return { ...state, confirming_logout: false };
  if (event.kind === "confirm_logout") return { ...state,
    confirming_logout: state.open && state.valid && !nativeAuthActive(state) };
  if (event.kind === "begin") {
    if (!nativeAuthAllowed(state, event.action) || !Number.isSafeInteger(event.request) ||
      event.request <= 0) return state;
    return { ...state, in_flight: { action: event.action, request: event.request },
      presentation: undefined, error: undefined, confirming_logout: false };
  }
  if (event.kind === "reply" || event.kind === "failure") {
    if (event.navigation !== state.context.navigation ||
      event.request !== state.in_flight?.request) return state;
    if (event.kind === "failure") return failed(state, event.code);
    if (!matchesReply(state, event.reply)) {
      return failed(state, "native_auth_outcome_unknown_read_same_operation");
    }
    return receipt(state, event.reply);
  }
  return state;
}

export function nativeAuthError(code: string): string {
  switch (code) {
    case "native_auth_cannot_display_safely":
      return "The challenge cannot display safely. No partial code or URL is shown.";
    case "native_auth_presentation_limit":
      return "The complete observation exceeds the display bound. Refresh in Settings.";
    case "native_profile_not_managed":
      return "Select a native-managed Codex or Claude connection in Settings. Refresh metadata.";
    case "stale_model_access":
      return "The selected owner metadata changed. Refresh and inspect the connection in Settings.";
    case "native_auth_outcome_unknown_read_same_operation":
      return "Delivery is uncertain. Reconnect and Read the original native operation.";
    case "native_auth_claude_input_outcome_unknown":
      return "Private code delivery is uncertain. Read or Cancel the original operation. Do not submit the code again.";
    case "native_auth_claude_input_invalid":
      return "Paste the complete code and state shown by the official Claude sign-in page.";
    default:
      return "The native authentication action is unavailable. " +
        "Refresh Settings and inspect its reason.";
  }
}

export function nativeAuthFailureCode(error: unknown): string {
  const code = typeof error === "object" && error !== null && "code" in error ? error.code : "";
  const allowed = ["not_connected", "not_configured", "owner_revoked", "stale_model_access",
    "native_auth_database_mismatch", "native_auth_method_mismatch", "pending_intent",
    "native_auth_operation_conflict", "native_auth_operation_scope_conflict",
    "native_auth_operation_missing", "native_auth_operation_capacity", "native_auth_not_active",
    "native_auth_recovery_required", "native_auth_readonly", "native_profile_source_invalid",
    "native_profile_source_conflict", "native_profile_connection_missing",
    "native_profile_not_managed",
    "native_profile_binding_conflict", "native_profile_custody_unsafe", "native_profile_incomplete",
    "native_profile_identity_bounds", "native_profile_identity_changed",
    "native_profile_identity_invalid",
    "native_profile_capacity", "native_profile_new_home_not_empty", "native_profile_in_use",
    "native_profile_lock_unavailable", "owner_queue_not_submitted",
    "native_auth_identity_unavailable",
    "native_auth_cannot_display_safely", "native_auth_presentation_limit"];
  return typeof code === "string" && allowed.includes(code) ? code :
    "native_auth_outcome_unknown_read_same_operation";
}
