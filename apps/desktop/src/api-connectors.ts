import { call, knownError, namedFailure, type Result } from "./api";

export type WritePolicy = "ask_every_time" | "ask_once_per_project" | "read_only";
export type Wait = "minutes2" | "minutes10" | "minutes30";
export type NativeConnectors = "off" | "on";
export type SizeCap = "kib64" | "kib256" | "mib1";
export type KeepLog = "days7" | "days30" | "days90";
export interface ConnectorSettings { write: WritePolicy; wait: Wait; native: NativeConnectors; size_cap: SizeCap; keep_log: KeepLog }
export type CallKind = "read" | "write" | "list" | "unknown";
export interface CallView {
  connector: string;
  tool: string;
  kind: CallKind;
  project: string;
  project_name: string;
  agent: string;
  started_ms: number;
  ms: number;
  bytes_in: number;
  bytes_out: number;
  outcome: string;
  outcome_text: string;
}
export interface ProjectChoice { id: string; name: string }
export interface ActivityView {
  calls: CallView[];
  more: boolean;
  connector: string | null;
  project: string | null;
  connectors: string[];
  projects: ProjectChoice[];
  settings: ConnectorSettings;
  version: number;
}
export type ConnectorSettingsSave = { kind: "saved" } | { kind: "stale"; current: number };

/** The keep choices in the order both apps list them, with their words. */
export const KEEP_CHOICES: { choice: KeepLog; label: string }[] = [
  { choice: "days7", label: "7 days" }, { choice: "days30", label: "30 days" }, { choice: "days90", label: "90 days" },
];

/** The word for each call kind, the same in the TUI. */
export const KIND_WORDS: Record<CallKind, string> = { read: "Read", write: "Write", list: "Tool list", unknown: "Unknown tool" };

/** The engine's own sentence for each refusal the activity page can meet, first letter
 * raised, the words the TUI shows for the same code. */
export const CONNECTOR_REFUSALS: Record<string, string> = {
  connectors_unavailable: "The engine could not be reached.",
  connector_request_invalid: "The request could not be read.",
  connector_request_bounds: "The request is too large.",
  connector_outcome_unknown: "The engine did not answer in time; check the list again.",
};

/** One plain sentence for a refused Connectors request: the engine's sentence for its own
 * codes, else the app's line for the code. Input: the refusal. Output: the sentence. */
export function connectorProblem(refusal: { code: string; cause?: string; message?: string }): string {
  if (Object.hasOwn(CONNECTOR_REFUSALS, refusal.code)) return namedFailure(CONNECTOR_REFUSALS[refusal.code], refusal.code, refusal.cause);
  return refusal.message ?? namedFailure(knownError(refusal.code), refusal.code, refusal.cause);
}

async function connectorCall<T>(command: string, args: Record<string, unknown>): Promise<Result<T>> {
  const result = await call<T>(command, args);
  return result.kind === "error" ? { ...result, message: connectorProblem(result) } : result;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const KIB = 1024;
const MIB = 1024 * 1024;

function bytesWord(bytes: number): string {
  const whole = Math.max(0, Math.trunc(bytes));
  if (whole < KIB) return `${whole} B`;
  const [unit, size] = whole < MIB ? ["KiB", KIB] : ["MiB", MIB];
  const tenths = Math.floor((whole * 10 + size / 2) / size);
  return `${Math.floor(tenths / 10)}.${tenths % 10} ${unit}`;
}

/** A call's sizes as both apps show them, such as "120 B in, 2.0 KiB out". Inputs: the
 * bytes sent and received. Output: the words. */
export function sizeWords(bytesIn: number, bytesOut: number): string {
  return `${bytesWord(bytesIn)} in, ${bytesWord(bytesOut)} out`;
}

/** A call's local time with its day, such as "Nov 15 17:04", the same in the TUI. Input:
 * epoch milliseconds. Output: the words. */
export function stampWords(ms: number): string {
  const at = new Date(ms);
  const two = (value: number) => String(value).padStart(2, "0");
  return `${MONTHS[at.getMonth()]} ${at.getDate()} ${two(at.getHours())}:${two(at.getMinutes())}`;
}

/** Reads the connector activity: the newest calls with the filters given, the choices the
 * log holds and the connector settings with their version. Inputs: the connector and the
 * project to filter by, null for all. Output: the view. */
export function loadConnectorActivity(connector: string | null, project: string | null): Promise<Result<ActivityView>> {
  return connectorCall<ActivityView>("desktop_connector_activity", { connector, project });
}

/** Saves the connector settings with the version the page read; a save drops the logged
 * calls the new keep time no longer holds. Inputs: the settings and the version. Output:
 * saved, or stale with the current version. */
export function saveConnectorSettings(settings: ConnectorSettings, version: number): Promise<Result<ConnectorSettingsSave>> {
  return connectorCall<ConnectorSettingsSave>("desktop_connector_settings_set", { settings, version });
}
