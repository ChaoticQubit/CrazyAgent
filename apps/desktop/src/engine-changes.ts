import { watchChanges, type WatchChange, type WatchTopic } from "./api-watch";

/** How often a page that cannot watch the engine hears "unknown" and reloads what it shows:
 * the period the app's own feed polls at when it has no connection (watch_feed
 * `UNKNOWN_EVERY`). */
export const UNWATCHED_MS = 5_000;

type Listener = () => void;

const listeners = new Map<WatchTopic, Set<Listener>>();
let started: Promise<void> | null = null;

/** Hands one engine change to its listeners: each listener of a topic it names, once, and
 * every listener when it names "unknown". Input: the change. Output: none. */
export function changed(change: WatchChange): void {
  const everyone = change.topics.includes("unknown");
  const called = new Set<Listener>();
  for (const [topic, heard] of listeners) {
    if (!everyone && !change.topics.includes(topic)) continue;
    for (const listener of heard) {
      if (called.has(listener)) continue;
      called.add(listener);
      listener();
    }
  }
}

/** Calls `listener` after each engine change to one of `topics`, and after each change of
 * unknown subject (v2.5 S3c). Inputs: the topics, at least one, and the listener. Output: the
 * function that stops it. */
export function onChanged(topics: readonly WatchTopic[], listener: Listener): () => void {
  if (topics.length === 0) throw new Error("onChanged needs a topic");
  for (const topic of topics) {
    const heard = listeners.get(topic) ?? new Set<Listener>();
    heard.add(listener);
    listeners.set(topic, heard);
  }
  return () => {
    for (const topic of topics) listeners.get(topic)?.delete(listener);
  };
}

/** Starts following the engine's changes for this page, once. A consumer subscribes, awaits
 * this, then reads, so no change falls between its read and the watch. A page that cannot
 * watch hears "unknown" every `UNWATCHED_MS` instead. Output: resolves once the page hears
 * changes. */
export function startWatch(): Promise<void> {
  started ??= watchChanges(changed).then((result) => {
    if (result.kind !== "ok") window.setInterval(() => changed({ topics: ["unknown"] }), UNWATCHED_MS);
  });
  return started;
}
