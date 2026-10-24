import { POOL_MEMBER_MAX } from "../../api";

let draft: string[] = [];

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Keeps the accounts picked on the Accounts page for the next new pool, like the TUI's
 * "New pool from selected". Input: account ids, 1 to 15, each once. Output: none. */
export function startPoolWith(ids: readonly string[]): void {
  assert(ids.length >= 1 && ids.length <= POOL_MEMBER_MAX, "a new pool starts with 1 to 15 accounts");
  assert(new Set(ids).size === ids.length, "each account is picked once");
  draft = [...ids];
}

/** Takes the accounts kept for the next new pool, once. Output: the ids, empty when none. */
export function takePoolDraft(): string[] {
  const taken = draft;
  draft = [];
  return taken;
}
