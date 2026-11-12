import { call } from "./api";
import type { Result } from "./api";
import type { SettingAccess, SettingScope, SettingSource } from "./api-harness-settings";
import { failureCopy } from "./api-projects";
import { displayText } from "./screens/conversation";

/** A model slot of one harness: the master's model, the agents' default, the allowed list. */
export type ModelSlot = "master" | "agent" | "allowed";

/** Where a candidate id comes from: the harness's list, or an id the owner added that waits
 * for its test launch, passed it or failed it. */
export type ModelOrigin = "catalog" | "pending" | "verified" | "failed";

/** A slot value: one model id, a list of ids, or null for the account's own model. */
export type ModelValue = string | string[] | null;

/** One slot as the engine reads it at one scope: the value stored here, the value a start
 * uses with where it comes from, the version a change names, and whether it can change. */
export interface ModelRow {
  slot: ModelSlot;
  here: ModelValue;
  effective: ModelValue;
  source: SettingSource;
  version: number;
  access: SettingAccess;
}

/** One model id the owner can see for a harness, with where it comes from. */
export interface ModelCandidate {
  id: string;
  origin: ModelOrigin;
}

/** A harness's Models section from the engine: its three slots in page order, every
 * candidate, the harness's list first, then the owner's ids, whether the list was checked on
 * the installed harness version, with the reason when it was not, and the engine's line
 * while the harness reported no live list of its own. */
export interface Models {
  harness: string;
  rows: ModelRow[];
  candidates: ModelCandidate[];
  qualified: boolean;
  reason: string | null;
  live_note: string | null;
}

/** The Models page: the section, the project it was read for (null on the app page) and the
 * versions and references the harness's model list was checked against. */
export interface ModelsPage {
  models: Models;
  project_id: string | null;
  qualified_versions: string[];
  sources: string[];
}

/** What the master may pick for an agent. */
export type PickRule = "strict" | "hint" | "default";

/** What a start does when no pool account can run the chosen model. */
export type FallbackRule = "notice" | "stop" | "silent";

/** The owner's two app-wide model rules with the version a change names for each. */
export interface ModelRules {
  pick: PickRule;
  pick_version: number;
  fallback: FallbackRule;
  fallback_version: number;
}

/** What adding an owner id did: it waits for its test launch, the harness's list already has
 * it, or it already passed. */
export type ModelAdded = "pending" | "listed" | "verified";

/** What a Save on one slot sends: nothing, a reset or a new value. */
export type ModelChange<T> = { kind: "none" } | { kind: "reset" } | { kind: "set"; value: T };

/** The most ids one allowed list holds, as the engine keeps it. */
export const ALLOWED_MAX = 16;
/** The longest model id, in bytes, as the engine keeps it. */
export const MODEL_BYTES_MAX = 256;
/** The reason a slot shows on a harness whose model CrazyAgent does not choose. */
export const NOT_SUPPORTED = "Not supported by this harness";
/** The choice that runs the account's own model on the app page. */
export const ACCOUNT_MODEL = "Account's own model";
/** The choice that keeps the app value on a project page. */
export const APP_VALUE = "Use the app value";
/** The line after a change that lost to a newer one. */
export const MODEL_STALE_LINE = "This model choice changed since you opened it. The page now shows the saved value; make your change again.";

/** Each slot's label. */
export const SLOT_LABELS: Record<ModelSlot, string> = {
  master: "Master model",
  agent: "Agent default model",
  allowed: "Allowed models",
};

/** Each slot's help line. */
export const SLOT_HELP: Record<ModelSlot, string> = {
  master: "The model the master runs on with this harness.",
  agent: "The model an agent runs on when the master picks none.",
  allowed: "The models the master may pick for an agent, with a short reason.",
};

/** The pick rule's choices, in page order. */
export const PICK_OPTIONS: { value: PickRule; label: string }[] = [
  { value: "strict", label: "Only the allowed list" },
  { value: "hint", label: "Any offered model" },
  { value: "default", label: "Always the default" },
];

/** What each pick rule does, in one line. */
export const PICK_HELP: Record<PickRule, string> = {
  strict: "The master picks from the allowed list or uses the agent default. Any other pick is refused.",
  hint: "The master may pick any model this harness offers. The allowed list is a suggestion.",
  default: "Every agent runs the agent default. The master's pick is set aside.",
};

/** The fallback rule's choices, in page order. */
export const FALLBACK_OPTIONS: { value: FallbackRule; label: string }[] = [
  { value: "notice", label: "Tell me" },
  { value: "stop", label: "Stop and say why" },
  { value: "silent", label: "Quietly" },
];

/** What each fallback rule does, in one line. */
export const FALLBACK_HELP: Record<FallbackRule, string> = {
  notice: "When no pool account can run the chosen model, it runs on an account's own model and you and the master are told once.",
  stop: "When no pool account can run the chosen model, nothing starts and the reason shows.",
  silent: "When no pool account can run the chosen model, it runs on an account's own model and no one is told.",
};

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const REFUSALS: Record<string, string> = {
  model_choice_stale: MODEL_STALE_LINE,
  model_choice_custom_full: "This harness already keeps the most model ids you can add. Remove one first.",
  model_choice_custom_not_pending: "This model id isn't waiting for a test launch any more. The page shows its state.",
  model_choice_harness_unknown: "CrazyAgent doesn't know this harness.",
  model_choice_not_supported: `${NOT_SUPPORTED}.`,
  model_choice_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the page before you try again.",
  model_choice_project_unknown: "This project is no longer in CrazyAgent.",
  model_choice_reply_bounds: "The engine's answer was too large to read. Open the page again.",
  model_choice_request_bounds: "This change is too large for the engine. Choose fewer models.",
  model_choice_request_invalid: "The engine couldn't read this change. Open the page again.",
  model_choice_unavailable: "Model choice isn't ready in this engine yet.",
  model_choice_value_invalid: "This harness doesn't offer that model, or the id has characters a model id can't have. Use letters, digits and - _ . : / @ + only.",
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
  versioned_item_invalid: "CrazyAgent couldn't record this model choice. Open the page again.",
  versioned_unavailable: "CrazyAgent couldn't record this right now. Try again in a moment.",
};

/** The refusal codes of the model choice protocol that have their own sentence. */
export const MODEL_CHOICE_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused model choice action. Input: the refusal code and the
 * engine cause behind it. Output: the sentence, or the plain fallback naming the code. */
export function modelChoiceRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** Tells whether an answer means model choice is not ready: this build has no model choice
 * commands, or the engine answered model_choice_unavailable. */
export function modelsNotReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "model_choice_unavailable");
}

/** Tells whether a change lost to a newer one, so the page reads again. */
export function modelsStale<T>(result: Result<T>): boolean {
  return result.kind === "error" && result.code === "model_choice_stale";
}

function usable(origin: ModelOrigin): boolean {
  return origin === "catalog" || origin === "verified";
}

/** The ids the owner may choose or tick, in page order: the harness's list, then the owner's
 * ids that passed their test launch. Input: the candidates. Output: the ids. */
export function usableIds(candidates: readonly ModelCandidate[]): string[] {
  return candidates.filter((candidate) => usable(candidate.origin)).map((candidate) => candidate.id);
}

/** The state words beside a candidate id, empty for the harness's own ids. */
export function originWord(origin: ModelOrigin): string {
  switch (origin) {
    case "catalog": return "";
    case "pending": return "Waiting for its test launch";
    case "verified": return "Your id, tested";
    case "failed": return "Test launch failed";
  }
}

/** The line naming what the harness's model list was checked against. Inputs: the page and
 * the harness's name. Output: one sentence. */
export function catalogLine(page: Pick<ModelsPage, "qualified_versions">, name: string): string {
  const shown = displayText(name);
  if (page.qualified_versions.length === 0) return `CrazyAgent has no model list for ${shown}.`;
  return `Model list checked against ${shown} ${page.qualified_versions.map(displayText).join(", ")}.`;
}

/** Says that the harness's model list was not checked on the installed version, in the
 * engine's words; the ids still save. Input: the section. Output: the sentence, or "". */
export function uncheckedLine(models: Pick<Models, "qualified" | "reason">): string {
  if (models.qualified || models.reason === null) return "";
  return `Version not checked: ${displayText(models.reason)}.`;
}

/** Says that the harness reported no live model list, in the engine's words (DEC-141); the
 * shipped ids still show. Input: the section. Output: the sentence, or "". */
export function liveLine(models: Pick<Models, "live_note">): string {
  if (models.live_note === null) return "";
  return `${displayText(models.live_note)}.`;
}

/** A slot value in words. Input: the value. Output: the words. */
export function valueText(value: ModelValue): string {
  if (value === null) return ACCOUNT_MODEL;
  if (Array.isArray(value)) return value.length === 0 ? "None ticked" : value.map(displayText).join(", ");
  return displayText(value);
}

/** Where a slot's value comes from, in one line. Inputs: the row and the page's scope.
 * Output: the line. */
export function modelSourceLine(row: Pick<ModelRow, "slot" | "source" | "effective">, scope: SettingScope): string {
  const list = row.slot === "allowed";
  switch (row.source) {
    case "project":
      return list && Array.isArray(row.effective) && row.effective.length === 0 ? "Set for this project. None ticked: only the agent default runs." : "Set for this project.";
    case "app":
      if (scope === "project") return `Uses the app value: ${valueText(row.effective)}.`;
      return list && Array.isArray(row.effective) && row.effective.length === 0 ? "Set here for every project. None ticked: only the agent default runs." : "Set here for every project.";
    case "default":
      return list ? "None ticked: only the agent default runs." : "Nothing set: the account's own model runs.";
    case "engine":
      return "Set by CrazyAgent.";
    case "not_applied":
      return `${NOT_SUPPORTED}.`;
  }
}

/** Says what happens to a stored value the harness no longer offers, or a value stored on a
 * slot that doesn't apply. Inputs: the row and the candidates. Output: the line, or "". */
export function passedOverLine(row: ModelRow, candidates: readonly ModelCandidate[]): string {
  if (row.here === null) return "";
  if (row.access.kind !== "editable") return `Saved ${valueText(row.here)} isn't used here.`;
  const ids = usableIds(candidates);
  if (Array.isArray(row.here)) {
    const gone = row.here.filter((id) => !ids.includes(id));
    if (gone.length === 0) return "";
    return `Saved ${gone.map(displayText).join(", ")} ${gone.length === 1 ? "isn't" : "aren't"} offered now and ${gone.length === 1 ? "is" : "are"} left out.`;
  }
  return ids.includes(row.here) ? "" : `Saved ${displayText(row.here)} isn't offered now, so ${valueText(row.effective)} runs.`;
}

/** Whether a slot has a stored value to clear: open on every harness, so a value left from
 * an earlier choice can always go. */
export function canResetModel(row: Pick<ModelRow, "here">): boolean {
  return row.here !== null;
}

/** The words on a slot's reset. Inputs: the row and the scope. Output: the label. */
export function resetModelLabel(row: Pick<ModelRow, "slot" | "access">, scope: SettingScope): string {
  if (row.access.kind !== "editable") return "Clear saved value";
  if (row.slot === "allowed") return scope === "app" ? "Clear the list" : "Use the app list";
  return scope === "app" ? "Use the account's own model" : APP_VALUE;
}

/** The choice a master or agent slot starts on: its stored id when the harness offers it,
 * else "" for the account's own model or the app value. */
export function choiceDraft(row: ModelRow, candidates: readonly ModelCandidate[]): string {
  assert(row.slot !== "allowed", "a choice is a master or agent slot");
  return typeof row.here === "string" && usableIds(candidates).includes(row.here) ? row.here : "";
}

/** The choices of a master or agent slot: the account's own model on the app page or the app
 * value on a project page, then every usable id. */
export function choiceOptions(row: ModelRow, candidates: readonly ModelCandidate[], scope: SettingScope): { value: string; label: string }[] {
  assert(row.slot !== "allowed", "a choice is a master or agent slot");
  const first = { value: "", label: scope === "app" ? ACCOUNT_MODEL : APP_VALUE };
  return [first, ...usableIds(candidates).map((id) => ({ value: id, label: displayText(id) }))];
}

/** What Save sends for a master or agent slot: nothing when the choice is unchanged, a reset
 * when it goes back to "" (the engine refuses null), else the id. */
export function choiceChange(row: ModelRow, candidates: readonly ModelCandidate[], draft: string): ModelChange<string> {
  if (draft === choiceDraft(row, candidates)) return { kind: "none" };
  if (draft === "") return row.here === null ? { kind: "none" } : { kind: "reset" };
  assert(usableIds(candidates).includes(draft), "a choice is a usable id");
  return { kind: "set", value: draft };
}

/** The ticks an allowed list starts on: the list in force, the app's on a project that keeps
 * it. */
export function allowedDraft(row: Pick<ModelRow, "effective">): string[] {
  return Array.isArray(row.effective) ? [...row.effective] : [];
}

function ordered(ids: readonly string[], candidates: readonly ModelCandidate[]): string[] {
  const order = candidates.map((candidate) => candidate.id);
  return [...new Set(ids)].sort((a, b) => order.indexOf(a) - order.indexOf(b));
}

/** What Save sends for the allowed list: nothing when the same ids are ticked, else the ticks
 * in page order; none ticked saves an own empty list, so only the agent default runs. */
export function allowedChange(row: ModelRow, candidates: readonly ModelCandidate[], draft: readonly string[]): ModelChange<string[]> {
  const next = ordered(draft, candidates);
  const now = ordered(allowedDraft(row), candidates);
  assert(next.length <= ALLOWED_MAX, "an allowed list holds at most ALLOWED_MAX ids");
  if (next.length === now.length && next.every((id, index) => id === now[index])) return { kind: "none" };
  return { kind: "set", value: next };
}

/** Ticks or unticks one id, keeping page order. Inputs: the ticks, the id, on or off and the
 * candidates. Output: the new ticks, at most ALLOWED_MAX. */
export function toggleAllowed(draft: readonly string[], id: string, on: boolean, candidates: readonly ModelCandidate[]): string[] {
  const next = ordered(on ? [...draft, id] : draft.filter((item) => item !== id), candidates);
  assert(next.length <= ALLOWED_MAX, "an allowed list holds at most ALLOWED_MAX ids");
  return next;
}

/** Whether one candidate's tick can change: a usable id that is ticked, or one with room left
 * on the list. Pending and failed ids never can. */
export function tickable(candidate: ModelCandidate, draft: readonly string[]): boolean {
  return usable(candidate.origin) && (draft.includes(candidate.id) || draft.length < ALLOWED_MAX);
}

/** The line after adding an owner id. Inputs: what the engine did and the id. Output: one
 * sentence. */
export function addedLine(added: ModelAdded, id: string): string {
  const shown = displayText(id);
  switch (added) {
    case "pending": return `${shown} is added. It waits for its test launch and can be chosen once that passes.`;
    case "listed": return `${shown} is already on this harness's model list.`;
    case "verified": return `${shown} already passed its test launch.`;
  }
}

/** The line after removing an owner id. Inputs: whether it was there and the id. */
export function removedLine(removed: boolean, id: string): string {
  return removed ? `${displayText(id)} is removed.` : `${displayText(id)} was already gone.`;
}

/** Reads one harness's Models section; fresh marks the section opening, which reads the
 * harness's own model list once first. Inputs: the harness id, the project id, or null for
 * the app, and fresh. Output: the page. */
export function loadModels(harness: string, projectId: string | null, fresh: boolean): Promise<Result<ModelsPage>> {
  return call<ModelsPage>("desktop_model_choice", { harness, projectId, fresh });
}

/** Sets one slot at the page's scope. Inputs: the harness, the project or null, the slot, the
 * value and the version read. Output: ok, or the refusal; a lost race is model_choice_stale. */
export function setModelChoice(harness: string, projectId: string | null, slot: ModelSlot, value: string | string[], version: number): Promise<Result<void>> {
  return call<void>("desktop_set_model_choice", { harness, projectId, slot, value, version });
}

/** Clears one slot at the page's scope. Inputs: the harness, the project or null, the slot and
 * the version read. Output: ok, or the refusal. */
export function resetModelChoice(harness: string, projectId: string | null, slot: ModelSlot, version: number): Promise<Result<void>> {
  return call<void>("desktop_reset_model_choice", { harness, projectId, slot, version });
}

/** Adds a model id the owner typed for a harness. Inputs: the harness and the id. Output: what
 * the engine did, or the refusal. */
export function addModel(harness: string, model: string): Promise<Result<ModelAdded>> {
  return call<ModelAdded>("desktop_add_model", { harness, model });
}

/** Removes a model id the owner added. Inputs: the harness and the id. Output: whether it was
 * there, or the refusal. */
export function removeModel(harness: string, model: string): Promise<Result<boolean>> {
  return call<boolean>("desktop_remove_model", { harness, model });
}

/** Reads the owner's two model rules. Output: the rules, or the refusal. */
export function loadModelRules(): Promise<Result<ModelRules>> {
  return call<ModelRules>("desktop_model_rules");
}

/** Sets one model rule. Inputs: the rule, its value and the version read. Output: ok, or the
 * refusal; a lost race is model_choice_stale. */
export function setModelRule(rule: "pick" | "fallback", value: PickRule | FallbackRule, version: number): Promise<Result<void>> {
  return call<void>("desktop_set_model_rule", { rule, value, version });
}
