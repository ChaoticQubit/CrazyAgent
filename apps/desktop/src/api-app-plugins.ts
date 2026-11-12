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
export type PluginStatus = { kind: "ready" } | { kind: "needs_review" } | { kind: "needs_key" } | { kind: "refused"; problem: Problem };
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
}
export type Preview = { kind: "ready"; trust: TrustView } | { kind: "refused"; problem: Problem };
export type SearchChoice = { kind: "full_text" } | { kind: "plugin"; name: string };
export interface SearchOption { name: string; title: string; runs: RunsView; usable: boolean; reason: string | null; sends: string[] }
export type ModelState =
  | { state: "not_downloaded"; size_bytes: number }
  | { state: "downloading"; done_bytes: number; size_bytes: number }
  | { state: "ready"; size_bytes: number }
  | { state: "failed"; problem: Problem; size_bytes: number };
export interface Progress { embedded: number; total: number }
export interface SearchView { version: number; choice: SearchChoice; options: SearchOption[]; model: ModelState; progress: Progress }
export type SearchSave = { kind: "saved"; search: SearchView } | { kind: "stale"; current: number };
const MB = 1_000_000;

/** One sentence per refusal the app plugins protocol answers with, the engine's
 * `APP_PLUGINS_CODES`; owner protocol codes shared with other features fall back to the app's
 * own sentences. */
export const APP_PLUGIN_REFUSALS: Record<string, string> = {
  app_plugin_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  app_plugin_request_bounds: "This plugin request was too large to send. A manifest can be at most 16 KB.",
  app_plugin_request_invalid: "The engine didn't accept this plugin request. Reload the page and try again.",
  app_plugins_unavailable: "App plugins aren't available in this engine yet.",
  app_plugin_not_found: "That plugin is gone. The list now shows the current ones.",
  app_plugin_name_taken: "A built-in plugin already has that name. Give your plugin another name.",
  app_plugin_bundled: "Built-in plugins can be turned off but not removed or replaced.",
  app_plugin_changed: "The manifest changed after you checked it. Check it again before you trust it.",
  app_plugin_needs_review: "This plugin's file changed or is new. Review it before it runs.",
  app_plugin_refused: "CrazyAgent refused this manifest. Its reason is shown with it.",
  app_plugin_name_invalid: "A plugin name is lowercase letters, digits and dashes, starting with a letter, at most 40 characters.",
  app_plugin_folder_unsafe: "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again.",
  app_plugin_folder_unavailable: "CrazyAgent couldn't open your plugins folder. Try again.",
  app_plugin_file_unsafe: "That plugin file isn't a plain file private to you, so CrazyAgent won't read it.",
  app_plugin_secret_not_declared: "This plugin doesn't declare that secret.",
  app_plugin_account_not_found: "That account is gone. Pick another one or enter a key.",
  app_plugin_account_has_no_key: "That account has no saved API key to share. Pick another one or enter a key.",
  app_plugin_key_invalid: "That key can't be used: it is empty, too long or has spaces or control characters.",
  app_plugin_key_store_unavailable: "CrazyAgent couldn't save the key. Nothing was saved; try again.",
  owner_entropy_unavailable: "This computer couldn't make a random value for the request. Nothing changed; try again.",
  app_plugin_file_unreadable: "CrazyAgent couldn't read that file. Pick a manifest file you can open.",
  app_plugin_file_not_text: "That file isn't text. A manifest is a UTF-8 .yaml file.",
  picker_failed: "Couldn't open the file chooser. Paste the manifest instead.",
  picker_unavailable: "This computer has no file chooser CrazyAgent can use. Paste the manifest instead.",
};

/** What the Search page says after a save named a version another window had replaced. */
export const SEARCH_STALE = "Search settings changed in another window. This page now shows the saved ones; choose again.";

/** One plain line for a plugin or manifest problem: the engine's sentence, then its code and,
 * when known, the manifest line and field. Input: the problem. Output: the line. */
export function problemText(problem: Problem): string {
  const where = [problem.code, problem.line === null ? null : `line ${problem.line}`, problem.field].filter((part) => part !== null);
  return `${problem.message} (${where.join(", ")})`;
}

/** The one label a plugin row shows, in a fixed order: Refused (with its problem), Needs
 * review, Needs a key, then Enabled or Off. The TUI uses the same words. Input: the plugin.
 * Output: the word, its tone and the line under it, if any. */
export function pluginState(plugin: PluginView): { word: string; tone: Tone; detail: string | null } {
  const status = plugin.status;
  if (status.kind === "refused") return { word: "Refused", tone: "danger", detail: problemText(status.problem) };
  if (status.kind === "needs_review") return { word: "Needs review", tone: "warn", detail: "Review this plugin before it runs." };
  if (status.kind === "needs_key") return { word: "Needs a key", tone: "warn", detail: "Set a key for each secret it needs." };
  return plugin.enabled ? { word: "Enabled", tone: "ok", detail: null } : { word: "Off", tone: "neutral", detail: null };
}

/** One plain sentence for a refused app plugin or Search request. Input: the refusal code
 * and its engine cause. Output: the sentence, or the plain fallback naming the code. */
export function appPluginRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(APP_PLUGIN_REFUSALS, refusal.code) ? APP_PLUGIN_REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** The accounts whose saved API key a plugin secret can use. Input: the accounts. Output: the
 * API-key ones, in the order given. */
export function keyAccounts<T extends { method: string }>(accounts: T[]): T[] {
  return accounts.filter((account) => account.method === "api_key");
}

/** The host an HTTP plugin's address names, lowercase and without a port, as the Search
 * confirm names it and the engine compares it. Input: the address. Output: the host, or null
 * when the text is no address. */
export function hostOf(url: string): string | null {
  try {
    const host = new URL(url).hostname.toLowerCase();
    return host === "" ? null : host;
  } catch {
    return null;
  }
}

/** Tells whether choosing a Search option sends text off to an HTTP host and so needs the
 * confirm. Input: the option. Output: true for an HTTP plugin. */
export function httpOption(option: SearchOption): boolean {
  return option.runs.kind === "http";
}

/** How often, in milliseconds, Search reads again while something on it moves. */
export const SEARCH_POLL_MS = 2000;

/** Whether the Search page has something moving to read again for: a model download that
 * runs, or a usable chosen plugin that has not embedded everything yet. Input: the page read.
 * Output: true while either moves. */
export function searchPolling(view: SearchView): boolean {
  if (view.model.state === "downloading") return true;
  const choice = view.choice;
  if (choice.kind !== "plugin") return false;
  const chosen = view.options.find((option) => option.name === choice.name);
  return chosen !== undefined && chosen.usable && view.progress.embedded < view.progress.total;
}

/** A size in whole decimal megabytes. Input: bytes. Output: such as "126 MB". */
export function megabytes(bytes: number): string {
  return `${Math.round(bytes / MB)} MB`;
}

/** The Search choice a radio value stands for. Input: "full_text" or "plugin:<name>".
 * Output: the choice. */
export function searchChoice(value: string): SearchChoice {
  return value.startsWith("plugin:") ? { kind: "plugin", name: value.slice("plugin:".length) } : { kind: "full_text" };
}

/** The radio value of a Search choice. Input: the choice. Output: its value. */
export function choiceValue(choice: SearchChoice): string {
  return choice.kind === "plugin" ? `plugin:${choice.name}` : "full_text";
}

/** Reads every app plugin, built-in first. Output: the listing. */
export function loadAppPlugins(): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugins");
}

/** Reads the trust screen of a manifest without saving it. Input: the manifest text. Output:
 * ready with its trust screen, or refused with its problem. */
export function previewAppPlugin(yaml: string): Promise<Result<Preview>> {
  return call<Preview>("desktop_app_plugin_preview", { yaml });
}

/** Reads the trust screen of a file in the owner's plugins folder. Input: the plugin name.
 * Output: ready or refused. */
export function reviewAppPlugin(name: string): Promise<Result<Preview>> {
  return call<Preview>("desktop_app_plugin_review", { name });
}

/** Adds a manifest the owner confirmed. Inputs: the same text the trust screen was read from,
 * its SHA-256 and whether it replaces the owner's plugin of that name. Output: the listing. */
export function addAppPlugin(yaml: string, sha256: string, replace: boolean): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_add", { yaml, sha256, replace });
}

/** Trusts a reviewed file in the owner's plugins folder. Inputs: the name and the SHA-256 its
 * trust screen showed. Output: the listing. */
export function trustAppPlugin(name: string, sha256: string): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_trust", { name, sha256 });
}

/** Turns a plugin on or off. Inputs: the name and the switch. Output: the listing. */
export function setAppPluginEnabled(name: string, enabled: boolean): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_set_enabled", { name, enabled });
}

/** Removes one of the owner's plugins. Input: the name. Output: the listing. */
export function removeAppPlugin(name: string): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_remove", { name });
}

/** Lets a secret use an API-key account's saved key. Inputs: the plugin, the secret and the
 * account id. Output: the listing. */
export function bindAppPluginAccount(name: string, secret: string, account: string): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_bind_account", { name, secret, account });
}

/** Saves a key the owner entered for a secret. The key goes to the engine in this one call
 * and is never kept by the page. Inputs: the plugin, the secret and the key. Output: the
 * listing. */
export function setAppPluginKey(name: string, secret: string, key: string): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_set_key", { name, secret, key });
}

/** Clears where a secret's key comes from and deletes a key entered for it. Inputs: the plugin
 * and the secret. Output: the listing. */
export function unbindAppPlugin(name: string, secret: string): Promise<Result<Listing>> {
  return call<Listing>("desktop_app_plugin_unbind", { name, secret });
}

/** Opens the native chooser for one .yaml manifest and reads it, at most 16 KB of UTF-8.
 * Output: the text, or null when the owner closed the chooser. */
export function pickAppPluginFile(): Promise<Result<string | null>> {
  return call<string | null>("desktop_pick_app_plugin_file");
}

/** Reads the Search settings page. Output: the choice, options, model state and progress. */
export function loadSearch(): Promise<Result<SearchView>> {
  return call<SearchView>("desktop_search_settings");
}

/** Saves what search ranks with, naming the version this page read. Inputs: the choice, the
 * host the confirm named for an HTTP plugin (else null) and the version. Output: saved with
 * the page after it, or stale with the current version. */
export function saveSearch(choice: SearchChoice, confirmHost: string | null, version: number): Promise<Result<SearchSave>> {
  return call<SearchSave>("desktop_set_search", { choice, confirmHost, version });
}

/** Starts the built-in model download the owner confirmed. Input: the size in bytes the
 * confirm named, which the engine checks against the pinned size. Output: the Search page. */
export function downloadSearchModel(confirmBytes: number): Promise<Result<SearchView>> {
  return call<SearchView>("desktop_search_model_download", { confirmBytes });
}

/** Cancels the running built-in model download. Output: the Search page. */
export function cancelSearchModel(): Promise<Result<SearchView>> {
  return call<SearchView>("desktop_search_model_cancel");
}

/** Deletes the downloaded built-in model. Output: the Search page. */
export function deleteSearchModel(): Promise<Result<SearchView>> {
  return call<SearchView>("desktop_search_model_delete");
}
