import { call, knownError, namedFailure, type Result } from "./api";
import type { Origin, PluginStatus, Preview, SecretView } from "./api-app-plugins";

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
export type Who = "both" | "master" | "agents";
export type SignIn = "key" | "not_yet";
export interface ConnectorRuns { kind: "connector"; url: string; hosts: string[]; oauth_hosts: string[]; vendor_read_only: boolean }
export interface ConnectorView {
  name: string;
  title: string;
  version: string;
  description: string;
  origin: Origin;
  status: PluginStatus;
  sign_in: SignIn;
  runs: ConnectorRuns;
  secrets: SecretView[];
  sha256: string;
}
export interface Grant { connector: string; enabled: boolean; who: Who }
export interface ConnectorsView {
  connectors: ConnectorView[];
  more: boolean;
  settings: ConnectorSettings;
  version: number;
  project: string | null;
  grants: Grant[];
}

/** The Who a project switch change sends: the project's row's Who, so turning a connector
 * off and on again keeps the owner's last choice, or both when the project has no row for
 * it. Inputs: the page and the connector. Output: the Who. */
export function switchWho(view: ConnectorsView, connector: string): Who {
  return view.grants.find((grant) => grant.connector === connector)?.who ?? "both";
}

/** The Who choices in the order both apps list them, the default first, with their words. */
export const WHO_CHOICES: { choice: Who; label: string }[] = [
  { choice: "both", label: "Master and agents with tools on" }, { choice: "master", label: "Master only" }, { choice: "agents", label: "Agents only" },
];

/** The words of the project's Connectors section, the same in the TUI. */
export const PROJECT_CONNECTORS = {
  title: "Connectors",
  purpose: "Connectors from Settings, Connectors that this project's master and agents can use. Each is off until you turn it on here.",
  who: "Who can use it",
  none: "No connectors yet. Add one in Settings, Connectors.",
  notFound: "Not found",
  notChanged: "The connector was not changed.",
} as const;

/** The line naming the connectors a project cannot switch yet, the same in the TUI. Input:
 * their titles, at least one. Output: the line. */
export function notReadyLine(titles: string[]): string {
  if (titles.length === 0) throw new Error("the not ready line names at least one connector");
  return `Not ready yet: ${titles.join(", ")}. Set them up in Settings, Connectors.`;
}

/** A connector's state word, checked in a fixed order: Refused, Needs review, Needs a key,
 * then Ready, the same in the TUI. Input: the connector. Output: the word. */
export function connectorWord(connector: ConnectorView): string {
  const status = connector.status;
  if (status.kind === "refused") return "Refused";
  if (status.kind === "needs_review") return "Needs review";
  if (status.kind === "needs_key") return "Needs a key";
  return "Ready";
}

/** One row of a project's Connectors section: a listed connector, or the name of an enabled
 * grant whose connector the page no longer lists, with the project's grant if any. */
export type ProjectRow = { name: string; title: string; ready: boolean; word: string; grant: Grant | null };

/** What a project's Connectors section shows: every Ready connector and every one with an
 * enabled grant, in the page's order then the grants whose connector is gone, and the titles
 * of the rest (DEC-168 addendum 20). Input: the page. Output: the rows and the others. */
export function projectRows(view: ConnectorsView): { rows: ProjectRow[]; others: string[] } {
  const grantOf = new Map(view.grants.map((grant) => [grant.connector, grant]));
  const rows: ProjectRow[] = [];
  const others: string[] = [];
  for (const connector of view.connectors) {
    const grant = grantOf.get(connector.name) ?? null;
    const ready = connector.status.kind === "ready";
    if (ready || grant?.enabled) rows.push({ name: connector.name, title: connector.title, ready, word: connectorWord(connector), grant });
    else others.push(connector.title);
  }
  const listed = new Set(view.connectors.map((connector) => connector.name));
  for (const grant of view.grants) {
    if (grant.enabled && !listed.has(grant.connector)) rows.push({ name: grant.connector, title: grant.connector, ready: false, word: PROJECT_CONNECTORS.notFound, grant });
  }
  return { rows, others };
}

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

/** Reads the Connectors page. Input: the project whose switches to include, or null.
 * Output: every connector, the settings with their version and that project's switches. */
export function loadConnectors(project: string | null): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connectors", { project });
}

/** Reads the trust screen of a picked or pasted connector manifest without saving it.
 * Input: the manifest text. Output: ready or refused. */
export function previewConnector(yaml: string): Promise<Result<Preview>> {
  return connectorCall<Preview>("desktop_connector_preview", { yaml });
}

/** Reads the trust screen of a listed connector that needs review. Input: the name.
 * Output: ready or refused. */
export function reviewConnector(name: string): Promise<Result<Preview>> {
  return connectorCall<Preview>("desktop_connector_review", { name });
}

/** Adds a connector manifest the owner confirmed on its trust screen. Inputs: the text, the
 * SHA-256 the screen showed and whether it replaces the owner's own. Output: the page. */
export function addConnector(yaml: string, sha256: string, replace: boolean): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_add", { yaml, sha256, replace });
}

/** Trusts a reviewed connector file. Inputs: the name and the SHA-256 its trust screen
 * showed. Output: the page. */
export function trustConnector(name: string, sha256: string): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_trust", { name, sha256 });
}

/** Removes one of the owner's connectors, with its keys and its switch in every project.
 * Input: the name. Output: the page. */
export function removeConnector(name: string): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_remove", { name });
}

/** Saves a key the owner typed for one of a connector's secrets; only the engine keeps it.
 * Inputs: the connector, the secret and the key. Output: the page, which never holds it. */
export function setConnectorKey(name: string, secret: string, key: string): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_set_key", { name, secret, key });
}

/** Deletes the key saved for one of a connector's secrets. Inputs: the connector and the
 * secret. Output: the page. */
export function clearConnectorKey(name: string, secret: string): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_clear_key", { name, secret });
}

/** Turns a connector on or off for one project and says who it reaches. Inputs: the
 * project, the connector, the switch and who. Output: the page with that project's switches. */
export function setConnectorGrant(project: string, connector: string, enabled: boolean, who: Who): Promise<Result<ConnectorsView>> {
  return connectorCall<ConnectorsView>("desktop_connector_grant", { project, connector, enabled, who });
}
