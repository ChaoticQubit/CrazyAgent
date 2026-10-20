import type { ModelAccessDetail } from "./engine-client";
import { displayText } from "./screens/conversation";

export type NativeAuthMethod = "codex_browser" | "codex_device_code" | "claude_official";
export type NativeAuthCommand = NativeAuthMethod | "read" | "cancel" | "logout";
export type NativeAuthAction = { kind: "sign_in"; method: NativeAuthMethod } | { kind: "logout" };
export type NativeAuthPhase = "starting" | "awaiting_owner" | "checking_account" |
  "authenticated_observed" | "unauthenticated_observed" | "cancelling" |
  "unavailable" | "uncertain";
type StatusField = { state: "missing" } | { state: "null" } | { state: "value"; text: string };
type Challenge = { kind: "codex_browser"; auth_url: string } |
  { kind: "codex_device_code"; user_code: string; verification_url: string };
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
}

type Event = { kind: "open"; detail: ModelAccessDetail; navigation: number } |
  { kind: "begin"; action: NativeAuthCommand; request: number } |
  { kind: "reply"; navigation: number; request: number; reply: NativeAuthView } |
  { kind: "failure"; navigation: number; request: number; code: string } |
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
  if (!state.open || !state.valid || state.in_flight) return false;
  if (action === "read" || action === "cancel") {
    const target = state.operation ?? state.recoveryContext;
    return Boolean(target && sameTarget(target, state.context));
  }
  if (nativeAuthActive(state)) return false;
  if (action === "logout") return state.confirming_logout;
  return state.context.harness === "codex" ?
    action === "codex_browser" || action === "codex_device_code" :
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
  const fields = challenge.kind === "codex_browser" ? [challenge.auth_url] :
    [challenge.user_code, challenge.verification_url];
  return fields.every((value) => !/[\u0000-\u001f\u007f-\u009f]/u.test(value) &&
    !/[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(value));
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
