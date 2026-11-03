import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import { displayText } from "./screens/conversation";

/** A setting value as the engine stores it: a switch, a whole number or text; null where a
 * key is not applied or the engine keeps it unset. */
export type SettingValue = boolean | number | string | null;

/** How a setting is edited: a switch, one of a list, a whole number or text. */
export type SettingType = "bool" | "enum" | "integer" | "string";

/** Where a setting's shown value comes from: the catalog default, the app value, this
 * project's override, the engine, which forces a locked key, or nowhere because this harness
 * version starts with CrazyAgent's defaults only. */
export type SettingSource = "default" | "app" | "project" | "engine" | "not_applied";

/** Whether the owner can change a setting here, with the engine's reason when they can't: it
 * locks the key itself, or shows it read-only (not qualified for this harness version, or set
 * once for the whole app). */
export type SettingAccess = { kind: "editable" } | { kind: "locked"; reason: string } | { kind: "read_only"; reason: string };

/** Which values a page edits: the app value of every project, or one project's override. */
export type SettingScope = "app" | "project";

/** One harness setting as the window shows it. `here` is the value set at the page's scope,
 * if any; `value` is what the next start of the harness gets and `source` where it comes from;
 * `version` goes back with a set or reset, so a setting that changed since the page was read is
 * refused as stale. `here_valid` is false when the catalog in force refuses the stored value,
 * which is then not in force. A key stored here that the catalog no longer lists has `listed`
 * false and its key as label. */
export interface HarnessSetting {
  key: string;
  label: string;
  help: string;
  type: SettingType;
  options: string[];
  default: SettingValue;
  min: number | null;
  max: number | null;
  max_bytes: number | null;
  here: SettingValue;
  here_valid: boolean;
  listed: boolean;
  value: SettingValue;
  source: SettingSource;
  version: number;
  access: SettingAccess;
}

/** One harness's settings page: its name, the installed version, whether it is qualified, and
 * every key in catalog order, then keys stored here that the catalog no longer lists.
 * `project_id` is null on the app page. */
export interface HarnessSettingsPage {
  harness: string;
  name: string;
  installed: string | null;
  qualified: boolean;
  qualified_versions: string[];
  project_id: string | null;
  keys: HarnessSetting[];
}

/** What the owner is editing in one row: the switch state, or the field or option text. */
export type SettingDraft = boolean | string;

/** What a key left to the harness shows as its value: the harness decides until the owner
 * sets one. */
export const HARNESS_DEFAULT = "Harness default";

/** The longest text value the engine keeps, whatever bound a key names. */
export const VALUE_BYTES_MAX = 1024;

/** The most rows one harness page holds, as the engine keeps them at one scope: every catalog
 * key, then keys stored there that the catalog no longer lists. */
export const ROWS_MAX = 512;

const HARNESS_ID = /^[a-z0-9-]{1,32}$/;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** What the owner reads when a setting changed since the page showed it. */
export const STALE_LINE = "This setting changed since you opened it. The page now shows the saved value; make your change again.";

/** What a page says when the engine has no settings list for the harness. */
export const NO_CATALOG_LINE = "CrazyAgent has no settings list for this harness yet. It starts with CrazyAgent's defaults.";

/** When a change takes effect, for the page note. */
export const EFFECT_LINE = "Changes apply the next time a master or agent starts with this harness. Running ones keep their settings.";

const REFUSALS: Record<string, string> = {
  harness_settings_stale: STALE_LINE,
  harness_settings_key_locked: "CrazyAgent sets this one itself, so it can't be changed.",
  harness_settings_key_not_qualified: "This harness version isn't qualified for this setting, so it can't be changed.",
  harness_settings_key_app_only: "This setting is set once for the whole app. Change it in Settings, Harnesses.",
  harness_settings_key_invalid: "This setting isn't on this harness's list. The page shows what is there.",
  harness_settings_no_catalog: NO_CATALOG_LINE,
  harness_settings_catalog_invalid: "CrazyAgent's settings list for this harness is damaged. Reinstall CrazyAgent.",
  harness_settings_page_invalid: "The settings list changed while it was read. Open the page again.",
  harness_settings_value_invalid: "This value doesn't fit the setting. Check its limits, then save again.",
  harness_settings_project_unknown: "This project is no longer in CrazyAgent.",
  harness_settings_unavailable: "Harness settings aren't ready in this engine yet.",
  harness_settings_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the page before you try again.",
  owner_authentication_failed: "CrazyAgent couldn't confirm this app with the engine. Restart the app, then try again.",
  owner_authority_unavailable: "The engine isn't ready for owner actions yet. Try again in a moment.",
  owner_busy: "The engine is busy with other owner requests. Try again in a moment.",
  owner_grant_stale_or_revoked: "This app's access to the engine changed. Reconnect, then try again.",
  owner_request_failed: "The engine couldn't take this request. Try again.",
  owner_root_replaced_or_unsafe: "The engine's data folder changed or isn't safe. Restart the engine, then try again.",
  owner_root_unavailable: "The engine's data folder isn't available. Restart the engine, then try again.",
  owner_socket_replaced: "The engine was restarted. Reconnect, then try again.",
  owner_socket_unsafe: "The engine connection isn't safe to use. Restart the engine, then try again.",
  peer_owner_mismatch: "The engine runs as another user. Start it as yourself, then try again.",
  versioned_item_invalid: "CrazyAgent couldn't record a change to this setting. Open the page again.",
  versioned_unavailable: "CrazyAgent couldn't record this right now. Try again in a moment.",
  changes_unavailable: "The engine's records aren't available right now. Try again in a moment.",
};

/** The refusal codes of the harness settings protocol that have their own sentence. */
export const HARNESS_SETTINGS_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused harness settings action. Inputs: the refusal code and the
 * engine cause behind it. Output: the sentence, or the plain fallback naming the code. */
export function harnessSettingsRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** Tells whether a harness settings answer means the page is not ready yet: this build has no
 * harness settings commands, or the engine answered harness_settings_unavailable. */
export function notReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "harness_settings_unavailable");
}

/** Tells whether a set or reset was refused because the setting changed since the page read
 * it, so the page reads it again. Input: the result. Output: true for that refusal. */
export function isStale(result: Result<void>): boolean {
  return result.kind === "error" && result.code === "harness_settings_stale";
}

/** Whether a harness id has the engine's shape, so a route can carry it. */
export function harnessIdValid(id: string): boolean {
  return HARNESS_ID.test(id);
}

/** The page's version line. Input: the page. Output: one sentence. */
export function versionLine(page: Pick<HarnessSettingsPage, "name" | "installed" | "qualified" | "qualified_versions">): string {
  const name = displayText(page.name);
  const qualified = page.qualified_versions.map(displayText).join(", ") || "none yet";
  if (page.qualified && page.installed !== null) return `${name} ${displayText(page.installed)} is qualified. You can change the settings below.`;
  const found = page.installed === null ? `CrazyAgent couldn't read the installed ${name} version` : `${name} ${displayText(page.installed)} isn't qualified yet`;
  return `${found}, so every setting is read-only and agents start with CrazyAgent's defaults. Qualified versions: ${qualified}.`;
}

/** The page's keys as three groups in page order: the ones the owner can change, the ones
 * CrazyAgent sets itself, and the read-only ones. Empty groups drop. Inputs: the keys and the
 * filter text. Output: the titled groups. */
export function settingGroups(keys: readonly HarnessSetting[], query = ""): { kind: SettingAccess["kind"]; title: string; keys: HarnessSetting[] }[] {
  assert(keys.length <= ROWS_MAX, "a harness page holds at most ROWS_MAX rows");
  const shown = keys.filter((key) => matchesQuery(key, query));
  const groups = [
    { kind: "editable" as const, title: "Settings", keys: shown.filter((key) => key.access.kind === "editable") },
    { kind: "locked" as const, title: "Set by CrazyAgent", keys: shown.filter((key) => key.access.kind === "locked") },
    { kind: "read_only" as const, title: "Read-only", keys: shown.filter((key) => key.access.kind === "read_only") },
  ];
  assert(groups.reduce((total, group) => total + group.keys.length, 0) === shown.length, "every shown key is in one group");
  return groups.filter((group) => group.keys.length > 0);
}

/** Whether a key matches the filter text by its label, its key or its help, ignoring case.
 * Inputs: the key and the filter. Output: true when it matches or the filter is blank. */
export function matchesQuery(key: Pick<HarnessSetting, "key" | "label" | "help">, query: string): boolean {
  const wanted = query.trim().toLowerCase();
  if (wanted === "") return true;
  return [key.label, key.key, key.help].some((text) => text.toLowerCase().includes(wanted));
}

/** A value as the owner reads it: On or Off, the option or the number, or the text, with
 * hidden characters made visible; Not set for none. Input: the value. Output: the text. */
export function valueText(value: SettingValue): string {
  if (value === null) return "Not set";
  if (typeof value === "boolean") return value ? "On" : "Off";
  if (typeof value === "number") return String(value);
  return value === "" ? "Empty" : displayText(value);
}

/** A row's value as the owner reads it: Harness default when the catalog default leaves it to
 * the harness, otherwise as `valueText` reads it. Input: the key. Output: the text. */
export function shownValue(key: Pick<HarnessSetting, "value" | "source">): string {
  return key.value === null && key.source === "default" ? HARNESS_DEFAULT : valueText(key.value);
}

/** Where the shown value comes from, in plain words for the page's scope. Inputs: the key and
 * the page's scope. Output: the line. */
export function sourceLine(key: Pick<HarnessSetting, "source">, scope: SettingScope): string {
  switch (key.source) {
    case "engine": return "Set by CrazyAgent";
    case "project": return "Set for this project";
    case "app": return scope === "app" ? "Set for every project" : "From the app setting";
    case "default": return "Default";
    case "not_applied": return "Not applied: agents start with CrazyAgent's defaults";
  }
}

/** The draft an editable row starts from: the shown value, as the row's control holds it, or
 * "" while the harness default decides. Input: the key. Output: the draft. */
export function draftOf(key: Pick<HarnessSetting, "type" | "value" | "source">): SettingDraft {
  if (key.value === null) {
    assert(key.source === "default", "only the harness default leaves a key without a value");
    return "";
  }
  if (key.type === "bool") {
    assert(typeof key.value === "boolean", "a switch key holds a boolean");
    return key.value;
  }
  return String(key.value);
}

/** The value a draft sends, checked against the key's type and bounds as the engine will check
 * it again. Inputs: the key and the draft. Output: the value, or a plain error. */
export function draftValue(key: Pick<HarnessSetting, "type" | "options" | "min" | "max" | "max_bytes">, draft: SettingDraft): { value: SettingValue } | { error: string } {
  if (key.type === "bool") return typeof draft === "boolean" ? { value: draft } : { error: "Turn this setting on or off." };
  if (typeof draft !== "string") return { error: "Type a value for this setting." };
  if (key.type === "enum") return key.options.includes(draft) ? { value: draft } : { error: "Pick one of the options." };
  if (key.type === "integer") return wholeNumber(key, draft);
  const bytes = new TextEncoder().encode(draft).length;
  const most = key.max_bytes ?? VALUE_BYTES_MAX;
  assert(most > 0 && most <= VALUE_BYTES_MAX, "a text key's bound is inside the engine's bound");
  if (/[\u0000-\u001f\u007f-\u009f]/.test(draft)) return { error: "Use one line of plain text." };
  return bytes <= most ? { value: draft } : { error: `Use at most ${most} bytes.` };
}

function wholeNumber(key: Pick<HarnessSetting, "min" | "max">, draft: string): { value: number } | { error: string } {
  const text = draft.trim();
  const range = key.min !== null && key.max !== null ? ` from ${key.min} to ${key.max}`
    : key.min !== null ? ` of at least ${key.min}` : key.max !== null ? ` of at most ${key.max}` : "";
  const error = { error: `Use a whole number${range}.` };
  if (!/^-?\d{1,16}$/.test(text)) return error;
  const value = Number(text);
  if (!Number.isSafeInteger(value)) return error;
  if (key.min !== null && value < key.min) return error;
  if (key.max !== null && value > key.max) return error;
  return { value };
}

/** Whether a row's draft differs from the shown value, so Save has something to send. A
 * switch key left to the harness drafts "" until the owner picks On or Off.
 * Inputs: the key and the draft. Output: true when the owner changed the control. */
export function draftChanged(key: Pick<HarnessSetting, "type" | "value" | "source">, draft: SettingDraft): boolean {
  const unset = key.type === "bool" && key.value === null && draft === "";
  assert(unset || typeof draft === (key.type === "bool" ? "boolean" : "string"), "a draft matches its control");
  return draft !== draftOf(key);
}

/** Whether a row offers a reset: a value set at the page's scope on an editable key, or on a
 * key the catalog no longer lists, which the engine lets the owner clear. Input: the key.
 * Output: true when Reset has something to clear. */
export function canReset(key: Pick<HarnessSetting, "access" | "here" | "listed">): boolean {
  assert(key.listed || key.access.kind === "read_only", "an unlisted key is read-only");
  assert(key.listed || key.here !== null, "an unlisted key shows only for its saved value");
  return key.here !== null && (key.access.kind === "editable" || !key.listed);
}

/** Why the value saved at the page's scope is not in force: this harness version no longer
 * allows it, or no longer lists the setting. Input: the key. Output: the line, or "" when the
 * saved value, if any, is one the catalog allows. */
export function storedLine(key: Pick<HarnessSetting, "here" | "here_valid" | "listed">): string {
  if (key.here_valid) return "";
  assert(key.here !== null, "only a saved value can be not valid");
  const saved = valueText(key.here);
  if (!key.listed) return `This harness version no longer lists this setting, so the saved value ${saved} isn't used.`;
  return `The saved value ${saved} isn't allowed for this harness version, so it isn't used.`;
}

/** The reset button's words. A project reset clears only the project's value, which then
 * follows the app value or, with none, the default; a key the catalog no longer lists has no
 * default, so its reset clears the saved value. Inputs: the key and the scope. Output: the
 * label. */
export function resetLabel(key: Pick<HarnessSetting, "listed">, scope: SettingScope): string {
  if (!key.listed) return "Clear saved value";
  return scope === "app" ? "Use default" : "Clear project value";
}

/** Reads one harness's settings page. Inputs: the harness id and the project id, or null for
 * the app page. Output: the page. */
export function loadHarnessSettings(harness: string, projectId: string | null): Promise<Result<HarnessSettingsPage>> {
  return call<HarnessSettingsPage>("desktop_harness_settings", { harness, projectId });
}

/** Sets one setting in the page's scope. Inputs: the harness, the project or null for the app,
 * the key, the value and the version the page read. Output: ok once stored, or the refusal; a
 * setting that changed since is `harness_settings_stale`. */
export function setHarnessSetting(harness: string, projectId: string | null, key: string, value: SettingValue, version: number): Promise<Result<void>> {
  return call<void>("desktop_set_harness_setting", { harness, projectId, key, value, version });
}

/** Clears one setting in the page's scope, so the app value or the default applies again.
 * Inputs: the harness, the project or null, the key and the version read. Output: ok, or the
 * refusal. */
export function resetHarnessSetting(harness: string, projectId: string | null, key: string, version: number): Promise<Result<void>> {
  return call<void>("desktop_reset_harness_setting", { harness, projectId, key, version });
}
