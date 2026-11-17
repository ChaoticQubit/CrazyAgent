import { Channel } from "@tauri-apps/api/core";
import { call, type Result } from "./api";

/** An engine state group a change is about, as the engine's watch names it (crazyagent
 * protocol `Topic`). "unknown" means any view the window shows may have changed. */
export type WatchTopic =
  | "worktrees"
  | "ide"
  | "memory"
  | "projects"
  | "landing"
  | "settings"
  | "plugins"
  | "sessions"
  | "intake"
  | "engine"
  | "accounts"
  | "team"
  | "unknown";

/** One message of the engine watch: the topics whose views the window reloads now. */
export interface WatchChange {
  topics: WatchTopic[];
}

/** Follows the engine's watch for this page (v2.5 S3b): named topics come at once and
 * "unknown" at most once every 5 s, also while the app cannot watch, as the old poll did.
 * A page load replaces the last page's handler. Input: the handler for each change.
 * Output: ok once the app follows the engine for this page. */
export function watchChanges(onChange: (change: WatchChange) => void): Promise<Result<void>> {
  const changes = new Channel<WatchChange>();
  changes.onmessage = onChange;
  return call<void>("desktop_watch", { changes });
}
