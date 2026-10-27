/** The most bytes of the engine's locked-keys file the extension reads. */
export const LOCKED_BYTES_MAX = 64 * 1024;

/** What one locked key holds now: its effective value and the owner's User value, if set. */
export interface Seen {
  value: unknown;
  user: unknown;
}

/** One locked key to write back to the owner's User settings, with its locked value. */
export interface Restore {
  key: string;
  value: unknown;
}

/**
 * Reads the engine's locked-keys file: one JSON object of setting keys and values.
 * Input: the file text. Output: the locked settings, or undefined for anything else.
 */
export function parseLocked(text: string): Record<string, unknown> | undefined {
  if (Buffer.byteLength(text, "utf8") > LOCKED_BYTES_MAX) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return undefined;
  return parsed as Record<string, unknown>;
}

/**
 * Lists the locked keys to write back: those whose User value contradicts the lock, and
 * those with no User value whose effective value differs. A key whose User value already
 * matches is left alone even when a higher layer wins, so one write never repeats. Keys
 * outside the lock are never listed. Inputs: the locked settings and what each locked key
 * holds now. Output: the restores, in the lock's key order.
 */
export function restores(locked: Record<string, unknown>, seen: Record<string, Seen>): Restore[] {
  return Object.entries(locked).flatMap(([key, value]) => {
    const now = Object.hasOwn(seen, key) ? seen[key] : undefined;
    if (now === undefined || same(now.user, value)) return [];
    if (now.user === undefined && same(now.value, value)) return [];
    return [{ key, value }];
  });
}

function same(left: unknown, right: unknown): boolean {
  return left !== undefined && JSON.stringify(left) === JSON.stringify(right);
}
