import { call, knownError, namedFailure, type Result } from "./api";
import { problemText, shown, type Origin, type PluginStatus, type Preview, type Problem, type SecretView } from "./api-app-plugins";
import { displayText } from "./screens/conversation";
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
  runs: ConnectorRuns | null;
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
  writes: "Allowed writes",
  writesAbout: "Connectors that may change things in this project without asking again.",
  writesNone: "No connector may change things here without asking.",
  writesFailed: "Couldn't load the allowed writes.",
  revoke: "Revoke",
} as const;

/** The connectors a project may write with without asking again, each with the title of the
 * first listed connector of that name, else its name. Inputs: the page read and the allowed
 * writes. Output: the rows, in the allowed writes' order. */
export function writeRows(page: ConnectorsView, writes: AllowedWrites): { name: string; title: string }[] {
  if (writes.project !== page.project) throw new Error("the allowed writes are for the project the page read");
  return writes.connectors.map((name) => ({ name, title: page.connectors.find((listed) => listed.name === name)?.title ?? name }));
}

/** The toast after a Revoke, the same in the TUI. Input: the connector's title. Output: the
 * sentence. */
export function writesRevoked(title: string): string {
  return `${title} asks again before its next write.`;
}

/** The line naming the connectors a project cannot switch yet, the same in the TUI. Input:
 * their titles, at least one. Output: the line. */
export function notReadyLine(titles: string[]): string {
  if (titles.length === 0) throw new Error("the not ready line names at least one connector");
  return `Not ready yet: ${titles.join(", ")}. Set them up in Settings, Connectors.`;
}

/** A connector's state word, checked in a fixed order: Refused, Needs review, Unavailable
 * (it signs in with OAuth, which this build does not have), Needs a key, Needs sign-in,
 * then Ready, the same in the TUI. Input: the connector. Output: the word. */
export function connectorWord(connector: ConnectorView): string {
  const status = connector.status;
  if (status.kind === "refused") return "Refused";
  if (status.kind === "needs_review") return "Needs review";
  if (connector.sign_in === "not_yet") return "Unavailable";
  if (status.kind === "needs_key") return "Needs a key";
  if (status.kind === "needs_sign_in") return "Needs sign-in";
  return "Ready";
}

/** One row of a project's Connectors section: a listed connector, or the name of an enabled
 * grant whose connector the page no longer lists, with the project's grant if any. */
export type ProjectRow = { name: string; title: string; ready: boolean; word: string; grant: Grant | null };

/** What a project's Connectors section shows: every Ready connector and every one with an
 * enabled grant, in the page's order then the grants whose connector is gone, and the titles
 * of the rest (DEC-168 addendum 20). One row per name: the first listed wins, so a built-in
 * connector hides the owner's refused file it shadows. Input: the page. Output: the rows and
 * the others. */
export function projectRows(view: ConnectorsView): { rows: ProjectRow[]; others: string[] } {
  const grantOf = new Map(view.grants.map((grant) => [grant.connector, grant]));
  const rows: ProjectRow[] = [];
  const others: string[] = [];
  const seen = new Set<string>();
  for (const connector of view.connectors) {
    if (seen.has(connector.name)) continue;
    seen.add(connector.name);
    const grant = grantOf.get(connector.name) ?? null;
    const ready = connectorWord(connector) === "Ready";
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
  refusedFile: "Fix its file, or remove it on the App plugins page.",
  settings: "Connector settings",
  write: "Write actions",
  writeHelp: "What happens when an agent calls a connector tool that changes something: each call waits for your Allow, your first Allow in a project lets later calls run, or those tools stay hidden.",
  wait: "Approval wait",
  waitHelp: "How long a call that changes something waits for your answer before it fails.",
  native: "Harness-native connectors",
  nativeHelp: "Connectors from the harness's own account (claude.ai connectors in Claude Code, ChatGPT apps in Codex). The engine neither filters nor logs them.",
  sizeCap: "Tool result size cap",
  sizeCapHelp: "A longer tool result is cut to this size, with a note, before an agent reads it.",
  harnesses: "Harnesses",
  harnessesAbout: "Which harnesses reach connectors through the engine.",
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

/** The write action choices in the order both apps list them, the default first. */
export const WRITE_CHOICES: { choice: WritePolicy; label: string }[] = [
  { choice: "ask_every_time", label: "Ask every time" }, { choice: "ask_once_per_project", label: "Ask once per project and connector" }, { choice: "read_only", label: "Read-only" },
];
/** The approval wait choices in the order both apps list them; 10 min is the default. */
export const WAIT_CHOICES: { choice: Wait; label: string }[] = [
  { choice: "minutes2", label: "2 min" }, { choice: "minutes10", label: "10 min" }, { choice: "minutes30", label: "30 min" },
];
/** The harness-native choices in the order both apps list them, the default first. */
export const NATIVE_CHOICES: { choice: NativeConnectors; label: string }[] = [{ choice: "off", label: "Off" }, { choice: "on", label: "On" }];
/** The size cap choices in the order both apps list them; 256 KiB is the default. */
export const SIZE_CHOICES: { choice: SizeCap; label: string }[] = [
  { choice: "kib64", label: "64 KiB" }, { choice: "kib256", label: "256 KiB" }, { choice: "mib1", label: "1 MiB" },
];
const NOT_CONNECTOR: Problem = { code: "app_plugin_not_connector", line: null, field: "kind", message: "this is not a connector; add it on the App plugins page." };

/** The settings an owner has until they pick others: the engine's `ConnectorSettings::DEFAULT`. */
export const CONNECTOR_DEFAULTS: ConnectorSettings = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };

/** What the owner picked on the Connectors page for each setting it shows, or null where they
 * picked nothing. */
export type SettingPicks = { write: WritePolicy | null; wait: Wait | null; native: NativeConnectors | null; size_cap: SizeCap | null };

/** The settings a Connectors page save sends: the ones read, with only the four this page
 * shows replaced by their picks, so the keep setting goes back as it was. Inputs: the
 * settings read and the picks. Output: the settings to save, or null when no pick differs
 * from what was read. */
export function settingsToSave(read: ConnectorSettings, picks: SettingPicks): ConnectorSettings | null {
  const next: ConnectorSettings = {
    ...read,
    write: picks.write ?? read.write,
    wait: picks.wait ?? read.wait,
    native: picks.native ?? read.native,
    size_cap: picks.size_cap ?? read.size_cap,
  };
  const changed = next.write !== read.write || next.wait !== read.wait || next.native !== read.native || next.size_cap !== read.size_cap;
  return changed ? next : null;
}

/** A connector's state word with its tone and the line under it, checked in a fixed order:
 * Refused (with its problem), Needs review, Unavailable (with the OAuth line), Needs a key,
 * then Ready. Input: the connector. Output: the word, its tone and the line, if any. */
export function connectorState(connector: ConnectorView): { word: string; tone: Tone; detail: string | null } {
  const status = connector.status;
  const word = connectorWord(connector);
  if (status.kind === "refused") return { word, tone: "danger", detail: problemText(status.problem) };
  if (status.kind === "needs_review") return { word, tone: "warn", detail: "Review this connector before a project can use it." };
  if (connector.sign_in === "not_yet") return { word, tone: "warn", detail: CONNECTORS_PAGE.oauthNotYet };
  if (status.kind === "needs_key") return { word, tone: "warn", detail: "Set a key for each secret it needs." };
  if (status.kind === "needs_sign_in") return { word, tone: "warn", detail: "Sign in on its page before a project can use it." };
  return { word, tone: "ok", detail: null };
}

/** Where a connector comes from and its version, one line; a refused file has no version
 * to name. Input: the connector. Output: the line. */
export function originLine(connector: { name: string; version: string; origin: Origin }): string {
  const version = connector.version === "" ? "" : `, version ${connector.version}`;
  return `${connector.name}${version}. ${connector.origin === "bundled" ? "Built in" : "Added by you"}.`;
}

/** Every host a connector reaches, one line. Input: the hosts. Output: the line. */
export function reachesLine(hosts: string[]): string {
  return `Reaches ${hosts.join(", ")}`;
}

/** What a connector's trust screen says, in order and as plain text: version, address,
 * hosts, read-only mode, how the key is sent, the OAuth sign-in when declared, the tools it
 * says only read, the secrets it asks for and the SHA-256. Input: the trust view. Output:
 * the lines. */
export function trustLines(trust: ConnectorTrustView): string[] {
  const listed = (items: string[]) => (items.length > 0 ? items.join(", ") : "none");
  const { read_only: readOnly, key, oauth } = trust.connector;
  const lines = [`Version ${trust.version}`, `Address: ${trust.runs.url}`, reachesLine(trust.runs.hosts)];
  if (readOnly.kind === "url") lines.push(`Read-only address: ${readOnly.url}`);
  else if (readOnly.kind === "header") lines.push(`Read-only header: ${readOnly.name}: ${readOnly.value}`);
  else lines.push("Read-only mode at the vendor: none");
  lines.push(key === null ? "Key: none" : `Key: the ${key.header} header${key.scheme ? ` with ${key.scheme}` : ""}, from ${key.secrets.join(", ")}`);
  if (oauth !== null) {
    lines.push(`Signs in on ${oauth.hosts.join(", ")}`, `Registers by ${oauth.registration.join(", then ")}`,
      `Scopes while reading: ${listed(oauth.scopes_read)}`, `Scopes when writing: ${listed(oauth.scopes_write)}`);
  }
  lines.push(`Tools it says only read: ${listed(trust.connector.read_tools)}`, `Secrets it needs: ${listed(trust.secrets)}`, `SHA-256 ${trust.sha256}`);
  return lines;
}

/** A connector preview as this page may show it: a ready answer that is not a connector's
 * (no connector block or address) reads as refused with the engine's own sentence. Input:
 * the engine's answer. Output: the preview. */
export function connectorPreview(preview: Preview | ConnectorPreview): ConnectorPreview {
  if (preview.kind === "refused") return preview;
  const trust = preview.trust as Partial<ConnectorTrustView> & { kind: string; runs: { kind: string } };
  if (trust.kind === "connector" && trust.runs.kind === "connector" && trust.connector) return preview as ConnectorPreview;
  return { kind: "refused", problem: NOT_CONNECTOR };
}

/** The line in the key dialog. Inputs: the connector title and its hosts. Output: the line. */
export function keyLine(title: string, hosts: string[]): string {
  return `${title} sends this key to ${hosts.join(", ")}. Only the engine keeps it.`;
}

/** The keep choices in the order both apps list them, with their words. */
export const KEEP_CHOICES: { choice: KeepLog; label: string }[] = [
  { choice: "days7", label: "7 days" }, { choice: "days30", label: "30 days" }, { choice: "days90", label: "90 days" },
];

/** The word for each call kind, the same in the TUI. */
export const KIND_WORDS: Record<CallKind, string> = { read: "Read", write: "Write", list: "Tool list", unknown: "Unknown tool" };

/** The engine's own sentence for each connector code it words, first letter raised, the
 * words the TUI shows for the same code. */
export const CONNECTOR_REFUSALS: Record<string, string> = {
  connectors_unavailable: "The engine could not be reached.",
  connector_request_invalid: "The request could not be read.",
  connector_request_bounds: "The request is too large.",
  connector_outcome_unknown: "The engine did not answer in time; check the list again.",
};

/** The app's sentence, in connector words, for each other refusal the Connectors protocol
 * answers past the owner codes; the engine words these for app plugins. The TUI uses the
 * same words. */
export const CONNECTOR_PAGE_REFUSALS: Record<string, string> = {
  connector_project_unknown: "That project is not set up here any more.",
  app_plugin_bundled: "Built-in connectors can't be removed or replaced.",
  app_plugin_changed: "The manifest changed after you checked it. Check it again before you trust it.",
  app_plugin_folder_unavailable: "CrazyAgent couldn't open your plugins folder. Try again.",
  app_plugin_folder_unsafe: "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again.",
  app_plugin_key_invalid: "That key can't be used: it is empty, too long or has spaces or control characters.",
  app_plugin_key_store_unavailable: "CrazyAgent couldn't save the key. Nothing was saved; try again.",
  app_plugin_name_taken: "A built-in plugin or connector already has that name. Give yours another name.",
  app_plugin_needs_key: "This connector needs a key first. Set one on its page in Settings, Connectors.",
  app_plugin_needs_review: "This connector's file changed or is new. Review it in Settings, Connectors first.",
  app_plugin_not_found: "That connector is gone. The page now shows the current ones.",
  app_plugin_refused: "CrazyAgent refused this manifest. Its reason is shown with it.",
  app_plugin_secret_not_declared: "This connector doesn't declare that secret.",
  app_plugins_unavailable: "Connectors aren't available in this engine yet.",
  owner_entropy_unavailable: "This computer couldn't make a random value for the request. Nothing changed; try again.",
  connector_approval_unknown: "That approval is gone.",
  connector_approval_closed: "This write was already answered or its wait ran out. The page now shows where it stands.",
};

/** One plain sentence for a refused Connectors request: the engine's sentence for its own
 * codes, else the app's line for the code. Input: the refusal. Output: the sentence. */
export function connectorProblem(refusal: { code: string; cause?: string; message?: string }): string {
  if (Object.hasOwn(CONNECTOR_REFUSALS, refusal.code)) return namedFailure(CONNECTOR_REFUSALS[refusal.code], refusal.code, refusal.cause);
  if (Object.hasOwn(CONNECTOR_PAGE_REFUSALS, refusal.code)) return namedFailure(CONNECTOR_PAGE_REFUSALS[refusal.code], refusal.code, refusal.cause);
  return refusal.message ?? namedFailure(knownError(refusal.code), refusal.code, refusal.cause);
}

/** One agent- or vendor-supplied name or text as the approval page shows it: each typed
 * backslash doubled, then each hidden character escaped, so a typed escape never shows like
 * the character it names, as the engine shows the full arguments. Input: the text the engine
 * answered. Output: the text to show. */
export function approvalText(text: string): string {
  return displayText(text.replaceAll("\\", "\\\\"));
}

function approvalShown(view: ApprovalView): ApprovalView {
  return { ...shown(view), title: approvalText(view.title), tool: approvalText(view.tool), agent: approvalText(view.agent), preview: approvalText(view.preview) };
}

async function connectorCall<T>(command: string, args: Record<string, unknown>, show: (value: T) => T = shown): Promise<Result<T>> {
  const result = await call<T>(command, args);
  if (result.kind === "error") return { ...result, message: connectorProblem(result) };
  return result.kind === "ok" ? { kind: "ok", value: show(result.value) } : result;
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

/** Reads what each harness reaches through the gateway, in the engine crate's words, from
 * the bridge with no engine call. Output: one row per harness. */
export function loadConnectorReach(): Promise<Result<string[]>> {
  return connectorCall<string[]>("desktop_connector_reach", {});
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

/** Where one connector write stands: waiting for the owner, allowed, denied, or past its wait. */
export type ApprovalState = "waiting" | "allowed" | "denied" | "expired";

/** One connector write as the approval page shows it, the engine's ApprovalView: what asks,
 * where, the preview, the full arguments with hidden characters already escaped and `hidden`
 * marking that they hold some, where it stands and its times in epoch milliseconds. */
export interface ApprovalView {
  id: number;
  project: string;
  project_name: string;
  agent: string;
  connector: string;
  title: string;
  tool: string;
  preview: string;
  arguments: string;
  hidden: boolean;
  state: ApprovalState;
  created_ms: number;
  expires_ms: number;
  answered_ms: number | null;
}

/** The connectors one project may write with without asking again, by name. */
export interface AllowedWrites { project: string; connectors: string[] }

/** The approval page's words, the same in the TUI (WORDS-t4.md r6, r8). */
export const APPROVAL_PAGE = {
  title: "Connector write",
  purpose: "An agent wants a connector to change something. Nothing is sent until you allow it.",
  hidden: "This write holds hidden or control characters; each one is shown as a \\u escape.",
  fold: "All arguments",
  allow: "Allow",
  deny: "Deny",
  allowed: "Write allowed.",
  denied: "Write denied.",
  expired: "Expired",
} as const;

/** The approval page's lines, in order: connector, tool, project, agent, when it asked and
 * what it sends. Input: the approval. Output: the lines. */
export function approvalLines(view: ApprovalView): string[] {
  return [
    `Connector: ${view.title}`,
    `Tool: ${view.tool}`,
    `Project: ${view.project_name}`,
    `Agent: ${view.agent}`,
    `Asked: ${stampWords(view.created_ms)}`,
    `What it sends: ${view.preview}`,
  ];
}

/** The time a waiting write has left, in whole seconds rounded up and never below 0:00.
 * Inputs: now and the end of the wait, in epoch milliseconds. Output: such as "9:59". */
export function timeLeft(nowMs: number, expiresMs: number): string {
  const seconds = Math.max(0, Math.ceil((expiresMs - nowMs) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** Where a write stands as the page shows it: one still waiting when its wait ran out reads
 * as expired, since the engine refuses an answer from then on. Inputs: the approval and now
 * in epoch milliseconds. Output: the state to show. */
export function shownState(approval: ApprovalView, now: number): ApprovalState {
  return approval.state === "waiting" && now >= approval.expires_ms ? "expired" : approval.state;
}

/** Where an answered write stands: "Allowed <stamp>", "Denied <stamp>" or "Expired".
 * Input: the approval. Output: the line, empty while it waits. */
export function answeredLine(view: ApprovalView): string {
  if (view.state === "waiting") return "";
  if (view.state === "expired") return APPROVAL_PAGE.expired;
  if (view.answered_ms === null) throw new Error("an allowed or denied write has its answer time");
  return `${view.state === "allowed" ? "Allowed" : "Denied"} ${stampWords(view.answered_ms)}`;
}

/** Reads one connector write by the id its notice carries. Input: the id. Output: the approval. */
export function loadApproval(id: number): Promise<Result<ApprovalView>> {
  return connectorCall<ApprovalView>("desktop_connector_approval", { id }, approvalShown);
}

/** Answers one waiting connector write once. Inputs: the id and whether the owner allows it.
 * Output: the approval after the answer. */
export function answerApproval(id: number, allow: boolean): Promise<Result<ApprovalView>> {
  return connectorCall<ApprovalView>("desktop_connector_answer", { id, allow }, approvalShown);
}

/** Reads the connectors a project may write with without asking again. Input: the project.
 * Output: them by name. */
export function loadAllowedWrites(project: string): Promise<Result<AllowedWrites>> {
  return connectorCall<AllowedWrites>("desktop_connector_writes", { project });
}

/** Takes back a project's standing allow for one connector. Inputs: the project and the
 * connector. Output: the standing allows left. */
export function revokeWrites(project: string, connector: string): Promise<Result<AllowedWrites>> {
  return connectorCall<AllowedWrites>("desktop_connector_revoke_writes", { project, connector });
}
