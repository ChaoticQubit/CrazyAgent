export const KEPT_MAX = 2;
export const HIDDEN_FLUSH_MS = 250;
export const HIDDEN_BYTES_MAX = 1024 * 1024;
export const KEPT_STORE_KEY = "crazyagent.kept-terminals";
export const KEPT_TEXT_MAX = 256;
export const KEPT_PX_MAX = 16384;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Names one kept terminal view: the project it shows for, the terminal, and the pane for a
 * native teammate known only by its pane in its lead's terminal (DEC-187). */
export type KeptKey = { project: string | null; terminal: string; pane: string | null };

/** The text two equal keys share and no other key has. Input: the key. Output: the text. */
export function keyId(key: KeptKey): string {
  return JSON.stringify([key.project, key.terminal, key.pane]);
}

/** What a kept set needs of a view: its key, whether a panel shows it now, and how to let it
 * go, which ends its connection and frees its terminal. */
export interface Held {
  readonly key: KeptKey;
  readonly shown: boolean;
  drop(): void;
}

/** The terminal views one window keeps loaded, most recently used first (DEC-187: the owner
 * keeps the last two). Past the cap the least recently used view no panel shows is dropped,
 * so only views shown at that moment can hold the set over its cap, and only until they are
 * hidden. The set never lets a view go itself: it hands the dropped views to the caller.
 * Input: the cap. */
export class KeptSet<E extends Held> {
  private readonly order: string[] = [];
  private readonly held = new Map<string, E>();
  private readonly max: number;

  constructor(max: number) {
    assert(Number.isInteger(max) && max >= 1, "a kept set holds at least one view");
    this.max = max;
  }

  /** How many views the set keeps. Output: the count. */
  get size(): number {
    return this.held.size;
  }

  /** The kept view for a key. Input: the key. Output: the view, or undefined. */
  get(key: KeptKey): E | undefined {
    return this.held.get(keyId(key));
  }

  /** The kept keys, most recently used first. Output: the keys. */
  keys(): KeptKey[] {
    return this.order.flatMap((id) => {
      const view = this.held.get(id);
      return view ? [view.key] : [];
    });
  }

  /** Whether a view for this key can be kept behind the others: the set does not keep the key
   * and is under its cap. Input: the key. Output: the answer. */
  roomBehind(key: KeptKey): boolean {
    return !this.held.has(keyId(key)) && this.held.size < this.max;
  }

  /** Keeps a view loaded ahead of use as the least recently used, so it never drops a view the
   * owner opened. Input: a view `roomBehind` allows. Output: none. */
  keepBehind(view: E): void {
    assert(this.roomBehind(view.key), "a view kept behind takes a free place");
    const id = keyId(view.key);
    this.held.set(id, view);
    this.order.push(id);
    this.check();
  }

  /** Keeps a new view as the most recently used, then drops views past the cap as `trim`
   * does. Input: a view whose key the set does not keep. Output: the dropped views. */
  add(view: E): E[] {
    const id = keyId(view.key);
    assert(!this.held.has(id), "a key is kept once");
    this.held.set(id, view);
    this.order.unshift(id);
    return this.trim();
  }

  /** Marks a kept view as the most recently used; a key the set does not keep changes
   * nothing. Input: the key. Output: none. */
  touch(key: KeptKey): void {
    const id = keyId(key);
    const at = this.order.indexOf(id);
    if (at <= 0) return;
    this.order.splice(at, 1);
    this.order.unshift(id);
  }

  /** Drops the least recently used views no panel shows while the set keeps more than its
   * cap. Output: the dropped views, least recent first. */
  trim(): E[] {
    const dropped: E[] = [];
    for (let at = this.order.length - 1; at >= 0 && this.held.size > this.max; at -= 1) {
      const id = this.order[at];
      const view = this.held.get(id);
      if (!view || view.shown) continue;
      this.held.delete(id);
      this.order.splice(at, 1);
      dropped.push(view);
    }
    this.check();
    return dropped;
  }

  /** Drops every kept view whose key a test picks, shown or not, as when its project is
   * removed or the session starts again. Input: the test. Output: the dropped views. */
  dropWhere(test: (key: KeptKey) => boolean): E[] {
    const dropped: E[] = [];
    for (let at = this.order.length - 1; at >= 0; at -= 1) {
      const id = this.order[at];
      const view = this.held.get(id);
      if (!view || !test(view.key)) continue;
      this.held.delete(id);
      this.order.splice(at, 1);
      dropped.push(view);
    }
    this.check();
    return dropped;
  }

  private check(): void {
    assert(this.order.length === this.held.size, "the order names every kept view once");
    assert(this.held.size <= this.max || [...this.held.values()].every((view) => view.shown), "past the cap only shown views stay");
  }
}

/** A terminal the owner used, as the app remembers it to load at the next start (DEC-187):
 * its key, with a project, and the size in px its panel last had, if known. */
export type SavedView = { key: KeptKey & { project: string }; size: { width: number; height: number } | null };

function name(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= KEPT_TEXT_MAX;
}

function px(value: unknown): value is number {
  return Number.isInteger(value) && (value as number) > 0 && (value as number) <= KEPT_PX_MAX;
}

/** Reads the remembered terminals from their stored text, most recently used first. Anything
 * not exactly a remembered terminal is skipped, a key is kept once, and at most `KEPT_MAX`
 * come back. Input: the stored text, or null. Output: the terminals. */
export function savedViews(text: string | null): SavedView[] {
  let value: unknown;
  try {
    value = JSON.parse(text ?? "[]");
  } catch {
    return [];
  }
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  const saved: SavedView[] = [];
  for (const item of value.slice(0, KEPT_MAX * 4)) {
    if (saved.length === KEPT_MAX) break;
    if (typeof item !== "object" || item === null) continue;
    const { project, terminal, pane, width, height } = item as Record<string, unknown>;
    if (!name(project) || !name(terminal) || !(pane === null || name(pane))) continue;
    const sized = px(width) && px(height);
    if (!sized && !(width === null && height === null)) continue;
    const key = { project, terminal, pane };
    const id = keyId(key);
    if (seen.has(id)) continue;
    seen.add(id);
    saved.push({ key, size: sized ? { width, height } : null });
  }
  assert(saved.length <= KEPT_MAX, "at most the cap is remembered");
  return saved;
}

/** The stored text for remembered terminals. Input: at most `KEPT_MAX` terminals. Output: the
 * text. */
export function savedText(saved: SavedView[]): string {
  assert(saved.length <= KEPT_MAX, "at most the cap is remembered");
  return JSON.stringify(saved.map(({ key, size }) => ({ ...key, width: size?.width ?? null, height: size?.height ?? null })));
}

/** The remembered terminals after the owner shows one: it comes first, its older entry goes
 * and the list keeps the cap. Inputs: the list and the shown terminal. Output: the new list. */
export function shownFirst(saved: SavedView[], shown: SavedView): SavedView[] {
  const id = keyId(shown.key);
  const kept = saved.find((entry) => keyId(entry.key) === id);
  const size = shown.size ?? kept?.size ?? null;
  return [{ key: shown.key, size }, ...saved.filter((entry) => keyId(entry.key) !== id)].slice(0, KEPT_MAX);
}

/** The remembered terminals after a shown one's panel changed size, in the same order; a key
 * not remembered changes nothing. Inputs: the list, the key and the size. Output: the list. */
export function resized(saved: SavedView[], key: KeptKey, size: { width: number; height: number }): SavedView[] {
  const id = keyId(key);
  return saved.map((entry) => keyId(entry.key) === id ? { key: entry.key, size } : entry);
}

/** Reads the remembered terminals from this app's storage; storage that is unavailable or
 * refuses reads as none. Output: the terminals, most recently used first. */
export function readSaved(): SavedView[] {
  try {
    return savedViews(window.localStorage.getItem(KEPT_STORE_KEY));
  } catch {
    return [];
  }
}

/** Stores the remembered terminals; storage that refuses keeps the old list. Input: the
 * terminals. Output: none. */
export function writeSaved(saved: SavedView[]): void {
  try {
    window.localStorage.setItem(KEPT_STORE_KEY, savedText(saved));
  } catch {
    return;
  }
}

/** The output a hidden terminal view holds until its next write, so a view nobody sees
 * parses its output in a few large writes instead of one per frame (DEC-187). Once
 * `HIDDEN_BYTES_MAX` are held the caller writes them at once, so the hold stays bounded. */
export class HiddenOutput {
  private chunks: Uint8Array[] = [];
  private held = 0;

  /** How many bytes are held. Output: the count. */
  get bytes(): number {
    return this.held;
  }

  /** Holds one output chunk after the ones before it. Input: the bytes. Output: true once
   * `HIDDEN_BYTES_MAX` or more are held, so the caller takes and writes them now. */
  push(bytes: Uint8Array): boolean {
    assert(this.held < HIDDEN_BYTES_MAX, "a full hold is taken before the next chunk");
    this.chunks.push(bytes);
    this.held += bytes.length;
    return this.held >= HIDDEN_BYTES_MAX;
  }

  /** Takes every held chunk, in order, as one array. Output: the bytes, or null when none
   * are held. */
  take(): Uint8Array | null {
    if (this.held === 0) return null;
    const all = new Uint8Array(this.held);
    let at = 0;
    for (const chunk of this.chunks) {
      all.set(chunk, at);
      at += chunk.length;
    }
    assert(at === this.held, "every held byte is taken once");
    this.chunks = [];
    this.held = 0;
    return all;
  }
}
