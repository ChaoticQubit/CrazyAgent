import { call, knownError, namedFailure, type Result } from "./api";
import type { Tone } from "./ui";

export type PluginKind = "embedding";
export type Origin = "bundled" | "user";
export type RunsView =
  | { kind: "local"; runtime: string; download_bytes: number }
  | { kind: "http"; url: string; hosts: string[]; local_unencrypted: boolean; dimensions: number };
export type Binding = { kind: "none" } | { kind: "account"; account: string; name: string } | { kind: "key" };
export interface SecretView { name: string; binding: Binding }
export interface Problem { code: string; line: number | null; field: string | null; message: string }
export type PluginStatus =
  | { kind: "ready" }
  | { kind: "needs_review" }
  | { kind: "changed"; trusted_sha256: string }
  | { kind: "needs_key" }
  | { kind: "refused"; problem: Problem };
export interface PluginView {
  name: string;
  title: string;
  version: string;
  description: string;
  kind: PluginKind;
  origin: Origin;
  status: PluginStatus;
  enabled: boolean;
  runs: RunsView | null;
  secrets: SecretView[];
  sha256: string;
}
export interface Listing { plugins: PluginView[]; more: boolean }
export interface PermissionWords { name: string; words: string }
export interface PluginChanged { plugin: string; trusted_sha256: string; current_sha256: string }
export type TurnOff = "refuse_new_calls" | "stop_runs";
export interface WhenTurnedOff { value: TurnOff; project_override: boolean }
export interface AppPluginsPage { listing: Listing; words: PermissionWords[]; notices: PluginChanged[]; when_turned_off: WhenTurnedOff }
export interface TrustView {
  name: string;
  title: string;
  version: string;
  description: string;
  kind: PluginKind;
  runs: RunsView;
  secrets: string[];
  sha256: string;
  replaces: boolean;
  changed: boolean;
  grew_hosts: string[];
}
export type Preview = { kind: "ready"; trust: TrustView } | { kind: "refused"; problem: Problem };
export interface PluginUser { run_id: string; label: string }
export interface GrantedCapability { name: string; tier: string }
export interface PluginGrants {
  plugin: string;
  tier: string;
  origin: Origin;
  state: "on" | "off";
  essential: boolean;
  capabilities: GrantedCapability[];
}

/** One sentence per refusal the app plugins protocol answers with, the engine's
 * `APP_PLUGINS_CODES`; the owner connection codes it shares fall back to the app's own
 * sentences. */
export const APP_PLUGIN_REFUSALS: Record<string, string> = {
  app_plugin_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  owner_outcome_unknown_reconcile_same_identity: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  owner_actor_stopped_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  app_plugin_request_bounds: "This plugin request was too large to send.",
  app_plugin_request_invalid: "The engine didn't accept this plugin request. Reload the page and try again.",
  app_plugins_unavailable: "App plugins aren't available in this engine yet.",
  app_plugins_readonly: "CrazyAgent's data is read-only right now, so plugins can't change. Nothing was changed.",
  app_plugin_not_found: "That plugin is gone. The list now shows the current ones.",
  app_plugin_name_taken: "A built-in plugin already has that name. Give your plugin another name.",
  app_plugin_bundled: "Built-in plugins can be turned off but not removed or replaced.",
  app_plugin_essential: "This built-in plugin stays on. CrazyAgent needs it to work.",
  app_plugin_changed: "The plugin folder changed after you checked it. Check it again before you trust it.",
  app_plugin_needs_review: "This plugin is new or its files changed. Review it before it runs.",
  app_plugin_refused: "CrazyAgent refused this plugin. Its reason is shown with it.",
  app_plugin_past_cap: "You have as many plugins as CrazyAgent holds. Remove one before you add another.",
  app_plugin_port_handed_out: "Another plugin already uses that local port. Change the port and try again.",
  app_plugin_folder_unsafe: "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again.",
  app_plugin_folder_unavailable: "CrazyAgent couldn't open your plugins folder. Try again.",
  app_plugin_secret_not_declared: "This plugin doesn't declare that secret.",
  app_plugin_account_not_found: "That account is gone. Pick another one or enter a key.",
  app_plugin_account_has_no_key: "That account has no saved API key to share. Pick another one or enter a key.",
  app_plugin_key_invalid: "That key can't be used: it is empty, too long or has a line break.",
  app_plugin_key_store_unavailable: "CrazyAgent couldn't reach its saved keys. Nothing was changed; try again.",
  app_plugin_setting_unsafe: "Your settings file isn't a plain file private to you, so CrazyAgent didn't change it.",
  app_plugin_setting_unreadable: "Your settings file has an error, so CrazyAgent didn't change it. Fix the file and try again.",
  app_plugin_setting_unavailable: "CrazyAgent couldn't save this setting. Nothing was changed; try again.",
};

/** The words of each turn-off choice, as the setting and the confirm show them. */
export const TURN_OFF_WORDS: Record<TurnOff, { title: string; detail: string }> = {
  refuse_new_calls: { title: "Refuse new calls", detail: "Running work goes on without the plugin; each new call to it is refused with a reason in the run's log." },
  stop_runs: { title: "Stop runs using it", detail: "Runs using the plugin are stopped." },
};

/** The line under the setting when a project's own settings file picks the choice. */
export const PROJECT_OVERRIDE_LINE = "A project settings file sets this, and it wins over the choice saved here.";

/** The confirm line for removing a plugin: the key typed for it goes with it. */
export const REMOVE_KEY_LINE = "Removing it also deletes the key you entered for it.";

/** The note under the locked switch of an essential built-in plugin. */
export const ESSENTIAL_NOTE = "This plugin stays on. CrazyAgent needs it to save what the agents were doing when the app quits.";

/** One plain line for a plugin problem: the engine's sentence, then its code and, when
 * known, the line and field. Input: the problem. Output: the line. */
export function problemText(problem: Problem): string {
  const where = [problem.code, problem.line === null ? null : `line ${problem.line}`, problem.field].filter((part) => part !== null);
  return `${problem.message} (${where.join(", ")})`;
}

/** The one label a plugin row shows, in a fixed order: Refused (with its problem), Changed,
 * Needs review, Needs a key, then Enabled or Off. Input: the plugin. Output: the word, its
 * tone, the line under it and whether the row offers Review. */
export function pluginState(plugin: PluginView): { word: string; tone: Tone; detail: string | null; review: boolean } {
  const status = plugin.status;
  if (status.kind === "refused") return { word: "Refused", tone: "danger", detail: problemText(status.problem), review: false };
  if (status.kind === "changed") return { word: "Changed - review", tone: "warn", detail: "Its files changed since you trusted it. It stays off until you review it.", review: true };
  if (status.kind === "needs_review") return { word: "Needs review", tone: "warn", detail: "Review this plugin before it runs.", review: true };
  if (status.kind === "needs_key") return { word: "Needs a key", tone: "warn", detail: "Set a key for each secret it needs.", review: false };
  return plugin.enabled ? { word: "Enabled", tone: "ok", detail: null, review: false } : { word: "Off", tone: "neutral", detail: null, review: false };
}

/** The tone of a refused request's notice: a change waiting its turn in the database still
 * lands, so it warns; every other refusal is danger. Input: the code. Output: the tone. */
export function failureTone(code: string): "warn" | "danger" {
  return code === "access_change_pending" ? "warn" : "danger";
}

/** One plain sentence for a refused app plugin request. Input: the refusal code and its
 * engine cause. Output: the sentence, or the plain fallback naming the code. */
export function appPluginRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(APP_PLUGIN_REFUSALS, refusal.code) ? APP_PLUGIN_REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** The plain words of one permission, from the page's table, or the name itself when the
 * table holds no row. Inputs: the table and the permission name. Output: the words. */
export function permissionWords(words: readonly PermissionWords[], name: string): string {
  return words.find((row) => row.name === name)?.words ?? name;
}

/** The short summary a plugin row shows. A code plugin counts its permissions; an embedding
 * plugin, which holds none, counts the hosts it reaches and the keys it needs. Inputs: the
 * grants row, or null, and the plugin view, or null. Output: such as "2 permissions" or
 * "reaches 1 host, needs 1 key". */
export function grantSummary(grants: PluginGrants | null, plugin: PluginView | null): string {
  if (grants !== null) return plural(grants.capabilities.length, "permission");
  const hosts = plugin?.runs?.kind === "http" ? plugin.runs.hosts.length : 0;
  const keys = plugin?.secrets.length ?? 0;
  const parts = [hosts > 0 ? `reaches ${plural(hosts, "host")}` : null, keys > 0 ? `needs ${plural(keys, "key")}` : null].filter((part) => part !== null);
  return parts.length === 0 ? "runs on this computer only" : parts.join(", ");
}

/** A count with its noun. Inputs: the count and the singular noun. Output: such as "1 key". */
function plural(count: number, noun: string): string {
  return `${count} ${count === 1 ? noun : `${noun}s`}`;
}

/** The hosts a plugin's key would be sent to, for the bind step. Input: the plugin. Output:
 * the hosts, empty for a local plugin. */
export function keyHosts(plugin: PluginView): string[] {
  return plugin.runs?.kind === "http" ? plugin.runs.hosts : [];
}

/** The hosts a trust screen lists, the ones that grew since the trusted version first.
 * Input: the trust view. Output: the hosts in that order, each once. */
export function reviewHosts(trust: TrustView): string[] {
  const all = trust.runs.kind === "http" ? trust.runs.hosts : [];
  const grew = trust.grew_hosts.filter((host) => all.includes(host));
  return [...grew, ...all.filter((host) => !grew.includes(host))];
}

/** The one app notification for the change notices a page read drained: a plugin whose
 * files changed since the owner trusted it stays off until reviewed. Input: the page.
 * Output: the line, or null when nothing changed. */
export function changedLine(page: AppPluginsPage): string | null {
  const titles = page.notices.map((notice) => page.listing.plugins.find((plugin) => plugin.name === notice.plugin)?.title ?? notice.plugin);
  if (titles.length === 0) return null;
  if (titles.length === 1) return `${titles[0]} changed since you trusted it. It stays off until you review it in App plugins.`;
  return `${titles.length} plugins changed since you trusted them: ${titles.join(", ")}. They stay off until you review them in App plugins.`;
}

/** The accounts whose saved API key a plugin secret can use. Input: the accounts. Output: the
 * API-key ones, in the order given. */
export function keyAccounts<T extends { method: string }>(accounts: T[]): T[] {
  return accounts.filter((account) => account.method === "api_key");
}

/** Reads every app plugin, the permission words and the change notices not yet shown.
 * Output: the page. */
export function loadAppPlugins(): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugins");
}

/** Reads what each built-in code plugin is granted and whether its switch is locked on.
 * Output: one row per built-in code plugin. */
export function loadPluginGrants(): Promise<Result<PluginGrants[]>> {
  return call<PluginGrants[]>("desktop_app_plugin_grants");
}

/** Reads the trust screen of a plugin folder the owner picked, saving nothing. Input: the
 * folder path. Output: ready with its trust screen, or refused with its problem. */
export function previewAppPlugin(folder: string): Promise<Result<Preview>> {
  return call<Preview>("desktop_app_plugin_preview", { folder });
}

/** Reads the trust screen of a plugin already in the plugins folder. Input: the name.
 * Output: ready or refused. */
export function reviewAppPlugin(name: string): Promise<Result<Preview>> {
  return call<Preview>("desktop_app_plugin_review", { name });
}

/** Adds a plugin folder the owner approved. Inputs: the folder, the SHA-256 its trust screen
 * showed and whether it replaces the owner's plugin of that name. Output: the page. */
export function addAppPlugin(folder: string, sha256: string, replace: boolean): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_add", { folder, sha256, replace });
}

/** Trusts a reviewed plugin in the plugins folder. Inputs: the name and the SHA-256 its trust
 * screen showed. Output: the page. */
export function trustAppPlugin(name: string, sha256: string): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_trust", { name, sha256 });
}

/** Turns a plugin on or off. Inputs: the name, the switch and, for off, what happens to the
 * runs using it for this one change, or null for the saved setting. Output: the page. */
export function setAppPluginEnabled(name: string, enabled: boolean, whenOff: TurnOff | null): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_set_enabled", { name, enabled, whenOff });
}

/** Reads the runs using a plugin, for the turn-off confirm. Input: the name. Output: the
 * runs. */
export function appPluginUsers(name: string): Promise<Result<PluginUser[]>> {
  return call<PluginUser[]>("desktop_app_plugin_users", { name });
}

/** Removes one of the owner's plugins and the key entered for it. Input: the name. Output:
 * the page. */
export function removeAppPlugin(name: string): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_remove", { name });
}

/** Lets a secret use an API-key account's saved key. Inputs: the plugin, the secret and the
 * account id. Output: the page. */
export function bindAppPluginAccount(name: string, secret: string, account: string): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_bind_account", { name, secret, account });
}

/** Saves a key the owner entered for a secret. The key goes to the engine in this one call
 * and is never kept by the page. Inputs: the plugin, the secret and the key. Output: the
 * page. */
export function setAppPluginKey(name: string, secret: string, key: string): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_set_key", { name, secret, key });
}

/** Saves what happens to running work when a plugin is turned off. Input: the choice.
 * Output: the page, with the choice in force. */
export function setAppPluginWhenOff(whenOff: TurnOff): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_set_when_off", { whenOff });
}

/** Clears where a secret's key comes from and deletes a key entered for it. Inputs: the
 * plugin and the secret. Output: the page. */
export function unbindAppPlugin(name: string, secret: string): Promise<Result<AppPluginsPage>> {
  return call<AppPluginsPage>("desktop_app_plugin_unbind", { name, secret });
}
