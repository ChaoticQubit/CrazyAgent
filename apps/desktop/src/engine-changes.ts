import { watchChanges, type WatchChange, type WatchTopic } from "./api-watch";

/** How often a page that cannot watch the engine hears "unknown" and reloads what it shows:
 * the period the app's own feed polls at when it has no connection (watch_feed
 * `UNKNOWN_EVERY`). */
export const UNWATCHED_MS = 5_000;

/** The shortest gap between two deliveries to the listeners: a burst of named commits, such
 * as team events while agents run, reloads each listener at most once per this window, with
 * the first change at once and the rest merged at its end (250-S3C, rv-sec2 #337 N1). */
export const COALESCE_MS = 250;

type Listener = () => void;

const listeners = new Map<WatchTopic, Set<Listener>>();
const held = new Set<WatchTopic>();
let holding = false;
let started: Promise<void> | null = null;

/** Hands one engine change to its listeners: at once when no delivery went out in the last
 * `COALESCE_MS`, else merged into the one at that window's end. Input: the change. Output:
 * none. */
export function changed(change: WatchChange): void {
  if (holding) {
    for (const topic of change.topics) held.add(topic);
    return;
  }
  deliver(change.topics);
}

/** Calls each listener of a named topic once, and every listener when "unknown" is named,
 * then holds later changes for `COALESCE_MS`. Input: the topics, at least one. Output: none. */
function deliver(topics: readonly WatchTopic[]): void {
  if (topics.length === 0) throw new Error("a change names a topic");
  holding = true;
  setTimeout(release, COALESCE_MS);
  const everyone = topics.includes("unknown");
  const called = new Set<Listener>();
  for (const [topic, heard] of listeners) {
    if (!everyone && !topics.includes(topic)) continue;
    for (const listener of heard) {
      if (called.has(listener)) continue;
      called.add(listener);
      listener();
    }
  }
}

/** Ends a hold window: delivers what it merged, which opens the next one, or lets the next
 * change go out at once. Output: none. */
function release(): void {
  holding = false;
  if (held.size === 0) return;
  const topics = [...held];
  held.clear();
  deliver(topics);
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
    if (result.kind !== "ok") setInterval(() => changed({ topics: ["unknown"] }), UNWATCHED_MS);
  });
  return started;
}
