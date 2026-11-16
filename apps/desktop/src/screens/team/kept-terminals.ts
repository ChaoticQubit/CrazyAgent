export const KEPT_MAX = 2;
export const HIDDEN_FLUSH_MS = 250;
export const HIDDEN_BYTES_MAX = 1024 * 1024;

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
