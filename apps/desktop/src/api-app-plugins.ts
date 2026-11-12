import { type Result } from "./api";

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
export type Tone = "ok" | "warn" | "danger" | "neutral";

export const APP_PLUGIN_REFUSALS: Record<string, string> = {};
export const SEARCH_STALE = "";

export function pluginState(_plugin: PluginView): { word: string; tone: Tone; detail: string | null } {
  return { word: "", tone: "neutral", detail: null };
}
export function problemText(_problem: Problem): string { return ""; }
export function appPluginRefusal(_refusal: { code: string; cause?: string }): string { return ""; }
export function keyAccounts<T extends { method: string }>(_accounts: T[]): T[] { return []; }
export function hostOf(_url: string): string | null { return ""; }
export function httpOption(_option: SearchOption): boolean { return false; }
export function megabytes(_bytes: number): string { return ""; }
export function searchChoice(_value: string): SearchChoice { return { kind: "full_text" }; }
export function choiceValue(_choice: SearchChoice): string { return ""; }
export function loadAppPlugins(): Promise<Result<Listing>> { return Promise.resolve({ kind: "not_ready" }); }
export function loadSearch(): Promise<Result<SearchView>> { return Promise.resolve({ kind: "not_ready" }); }
