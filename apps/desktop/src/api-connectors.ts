import { call, knownError, namedFailure, type Result } from "./api";
import type { Origin, PluginStatus, Preview, Problem, SecretView } from "./api-app-plugins";
import type { Tone } from "./ui";

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
export type ReadOnlyTrust = { kind: "url"; url: string } | { kind: "header"; name: string; value: string } | { kind: "none" };
export interface KeyTrust { header: string; scheme: string; secrets: string[] }
export interface OAuthTrust { hosts: string[]; registration: string[]; scopes_read: string[]; scopes_write: string[] }
export interface ConnectorTrust { read_only: ReadOnlyTrust; key: KeyTrust | null; oauth: OAuthTrust | null; read_tools: string[] }
export interface ConnectorTrustView {
  name: string;
  title: string;
  version: string;
  description: string;
  kind: "connector";
  runs: ConnectorRuns;
  secrets: string[];
  sha256: string;
  replaces: boolean;
  connector: ConnectorTrust;
}
export type ConnectorPreview = { kind: "ready"; trust: ConnectorTrustView } | { kind: "refused"; problem: Problem };

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

/** The words of Settings, Connectors and a connector's own page, the same in the TUI. */
export const CONNECTORS_PAGE = {
  title: "Connectors",
  purpose: "Vendor MCP servers your projects can use through the engine, which keeps each key. A connector reaches no project until you turn it on in that project's Settings, Connectors.",
  catalog: "Catalog",
  catalogAbout: "Built-in connectors ship with CrazyAgent. Yours live in your plugins folder and are used only while their file matches what you trusted.",
  add: "Add connector",
  back: "All connectors",
  oauthNotYet: "Signs in with OAuth, which this build does not have.",
  settings: "Connector settings",
  native: "Harness-native connectors",
  nativeHelp: "Connectors from the harness's own account (claude.ai connectors in Claude Code, ChatGPT apps in Codex). The engine neither filters nor logs them.",
  sizeCap: "Tool result size cap",
  sizeCapHelp: "A longer tool result is cut to this size, with a note, before an agent reads it.",
  locked: "Tool list check and third-party label",
  lockedValue: "Always on",
  lockedReason: "A connector whose vendor tools changed stops and shows a notice, and every vendor result starts with a line saying it is content, not instructions.",
  saved: "Connector settings saved.",
  stale: "The connector settings changed elsewhere. Check them and try again.",
  notSaved: "The connector settings were not saved.",
  addTitle: "Add a connector",
  manifest: "Connector manifest (YAML)",
  manifestHelp: "Paste a manifest or pick a .yaml file. Nothing is used until you confirm its trust screen.",
  pick: "Pick a .yaml file",
  refusedAdd: "This manifest can't be added",
  refusedReview: "This connector can't be trusted",
  keyHelp: "Sent to the engine once and never shown again.",
  enterKey: "Enter a key.",
  removeBody: "Its manifest is deleted from your plugins folder, with its keys and its switch in every project.",
} as const;

/** The harness-native choices in the order both apps list them, the default first. */
export const NATIVE_CHOICES: { choice: NativeConnectors; label: string }[] = [];
/** The size cap choices in the order both apps list them; 256 KiB is the default. */
export const SIZE_CHOICES: { choice: SizeCap; label: string }[] = [];
/** The settings an owner has until they pick others: the engine's `ConnectorSettings::DEFAULT`. */
export const CONNECTOR_DEFAULTS: ConnectorSettings = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };

/** The settings a Connectors page save sends: the ones read, with only the two this page
 * shows replaced, so the write, wait and keep settings go back as they were. Inputs: the
 * settings read and the picked values. Output: the settings to save. */
export function settingsToSave(read: ConnectorSettings, native: NativeConnectors, sizeCap: SizeCap): ConnectorSettings {
  void native;
  void sizeCap;
  return read;
}

/** A connector's state word with its tone and the line under it, checked in a fixed order:
 * Refused (with its problem), Needs review, Needs a key, then Ready. Input: the connector.
 * Output: the word, its tone and the line, if any. */
export function connectorState(connector: ConnectorView): { word: string; tone: Tone; detail: string | null } {
  void connector;
  return { word: "", tone: "neutral", detail: null };
}

/** Where a connector comes from and its version, one line. Input: the connector. Output: the line. */
export function originLine(connector: { name: string; version: string; origin: Origin }): string {
  void connector;
  return "";
}

/** Every host a connector reaches, one line. Input: the hosts. Output: the line. */
export function reachesLine(hosts: string[]): string {
  void hosts;
  return "";
}

/** What a connector's trust screen says, in order and as plain text: version, address,
 * hosts, read-only mode, how the key is sent, the OAuth sign-in when declared, the tools it
 * says only read, the secrets it asks for and the SHA-256. Input: the trust view. Output:
 * the lines. */
export function trustLines(trust: ConnectorTrustView): string[] {
  void trust;
  return [];
}

/** A connector preview as this page may show it: a ready answer that is not a connector's
 * (no connector block or address) reads as refused with the engine's own sentence. Input:
 * the engine's answer. Output: the preview. */
export function connectorPreview(preview: Preview | ConnectorPreview): ConnectorPreview {
  return preview as ConnectorPreview;
}

/** The line in the key dialog. Inputs: the connector title and its hosts. Output: the line. */
export function keyLine(title: string, hosts: string[]): string {
  void title;
  void hosts;
  return "";
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
export async function previewConnector(yaml: string): Promise<Result<ConnectorPreview>> {
  const result = await connectorCall<Preview>("desktop_connector_preview", { yaml });
  return result.kind === "ok" ? { kind: "ok", value: connectorPreview(result.value) } : result;
}

/** Reads the trust screen of a listed connector that needs review. Input: the name.
 * Output: ready or refused. */
export async function reviewConnector(name: string): Promise<Result<ConnectorPreview>> {
  const result = await connectorCall<Preview>("desktop_connector_review", { name });
  return result.kind === "ok" ? { kind: "ok", value: connectorPreview(result.value) } : result;
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
