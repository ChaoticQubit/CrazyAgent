import { useEffect, useRef } from "react";
import { accountLabels, listAccounts } from "../../api";
import { loadExtras, type AccountExtras, type LimitExtras, type ResetKind, type UnreadClass } from "../../api-usage";
import type { Route } from "../../ui";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

const KIND_WORDS: Record<ResetKind, string> = {
  five_hour: "5-hour",
  weekly: "weekly",
  model: "model",
  all_limits: "all limits",
};

/** The separator between the parts of an account's limits line, as the plan line uses. */
export const PART_GAP = " · ";

/** The words for free resets the last read found no count of, the same as the TUI's. */
export const UNREAD = "Free resets: couldn't read";

/** The words for a free resets count the engine could not read, by why, the same as the TUI's:
 * the plain words with a short reason, or off when the owner turned the read off. Input: the
 * class. Output: the words. */
export function unreadText(kind: UnreadClass | string): string {
  return UNREAD_WORDS[kind as UnreadClass] ?? UNREAD;
}

const UNREAD_WORDS: Record<UnreadClass, string> = {
  unspecified: UNREAD,
  asker: "Free resets: couldn't read (Anthropic answers only Claude Code)",
  withheld: "Free resets: couldn't read (Anthropic withheld them)",
  no_reason: "Free resets: couldn't read (Anthropic gave no reason)",
  no_answer: "Free resets: couldn't read (no answer)",
  not_understood: "Free resets: couldn't read (answer not understood)",
  off: "Free resets: off in Settings",
};

/** How long before the first look a gain still counts as new, in milliseconds: one usage
 * poll and some slack, so a reset that arrived while the app was closed for long is not
 * told as new. */
export const FRESH_GAIN_MS = 15 * 60_000;

/** The most new-reset notices one look tells, so a burst never floods the screen. */
export const NOTICES_PER_LOOK_MAX = 3;

/** How often the app reads the stored extras for the new free reset notice, in ms. */
export const RESET_WATCH_MS = 60_000;

const POOLS_ROUTE: Route = { name: "settings", tab: "pools" };

/** When a free reset expires: "today 14:30" on the same local day, else "Nov 20". Inputs:
 * the expiry and now in ms since the epoch. Output: the words, the same as the TUI's. */
export function expiryText(atMs: number, nowMs: number): string {
  const at = new Date(atMs);
  const now = new Date(nowMs);
  const today = at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth() && at.getDate() === now.getDate();
  if (today) return `today ${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  return `${MONTHS[at.getMonth()]} ${at.getDate()}`;
}

/** The count an account holds now: its stored count, or null when not reported or when the
 * next expiry has passed, which the engine also reads as not reported until read again.
 * Inputs: the contract and now. Output: the resets, or null. */
export function liveResets(extras: LimitExtras, nowMs: number) {
  const resets = extras.resets;
  if (resets === null) return null;
  if (!Number.isInteger(resets.count) || resets.count < 0) throw new Error(`a free reset count is a whole number from 0, got ${resets.count}`);
  if (resets.next_expiry_ms !== null && resets.next_expiry_ms <= nowMs) return null;
  return resets;
}

function kindWords(kind: string): string {
  return Object.hasOwn(KIND_WORDS, kind) ? KIND_WORDS[kind as ResetKind] : kind.replaceAll("_", " ");
}

/** An account's free resets in words: "No free resets", "2 free resets (5-hour, weekly), next
 * expires Nov 20", with "(may break)" for a private read, "Free resets: couldn't read" or "Free
 * resets: not reported by <harness>". Inputs: the contract and now. Output: the words, never a
 * bare 0. */
export function resetsText(extras: LimitExtras, nowMs: number): string {
  const resets = liveResets(extras, nowMs);
  if (resets === null) return extras.resets_unread ? unreadText(extras.resets_unread) : `Free resets: ${extras.not_reported}`;
  const label = extras.may_break ? " (may break)" : "";
  if (resets.count === 0) return `No free resets${label}`;
  const counted = resets.count === 1 ? "1 free reset" : `${resets.count} free resets`;
  const kinds = resets.kinds.length === 0 ? "" : ` (${resets.kinds.map(kindWords).join(", ")})`;
  const expiry = resets.next_expiry_ms === null ? "" : `, next expires ${expiryText(resets.next_expiry_ms, nowMs)}`;
  return `${counted}${kinds}${expiry}${label}`;
}

/** An account's credits in the provider's words, or not reported by its harness. Input: the
 * contract. Output: the words. */
export function creditsText(extras: LimitExtras): string {
  return `Credits: ${extras.credits ?? extras.not_reported}`;
}

/** The one line under an account's usage, the same on Pools, Metrics and the TUI: free
 * resets, credits and where to spend, with two not reported fields said once. Inputs: the
 * contract and now. Output: the line. */
export function extrasLine(extras: LimitExtras, nowMs: number): string {
  const resets = liveResets(extras, nowMs);
  const parts = resets === null && !extras.resets_unread && extras.credits === null
    ? [`Free resets and credits: ${extras.not_reported}`]
    : [resetsText(extras, nowMs), creditsText(extras)];
  if (extras.spend.kind === "elsewhere") parts.push(`Spend at ${extras.spend.place}`);
  return parts.join(PART_GAP);
}

/** The notice for a new free reset. Inputs: the account's name and its count now. Output:
 * the sentence. */
export function newResetLine(name: string, count: number): string {
  if (!Number.isInteger(count) || count < 1) throw new Error("a new free reset leaves at least one");
  return `${name} has a new free reset. ${count === 1 ? "1 free reset" : `${count} free resets`} now.`;
}

/** One new free reset to tell: the account and its count now. */
export interface NewReset {
  account: string;
  count: number;
}

/** Picks the new free resets to tell from one read: each account whose count rose since the
 * last look, by the engine's gain stamp, and for an account not seen before only a gain from
 * the last `FRESH_GAIN_MS`. A count not reported now is never told. Inputs: the stamps seen,
 * null before the first look, the read and now. Output: the stamps seen now and the resets to
 * tell, at most `NOTICES_PER_LOOK_MAX`. */
export function freshResets(known: ReadonlyMap<string, number> | null, rows: readonly AccountExtras[], nowMs: number): { known: Map<string, number>; send: NewReset[] } {
  const seen = new Map<string, number>();
  const send: NewReset[] = [];
  for (const row of rows) {
    const gained = row.extras.resets_gained_ms ?? null;
    if (gained === null) continue;
    seen.set(row.account, gained);
    const resets = liveResets(row.extras, nowMs);
    if (resets === null || resets.count < 1) continue;
    const before = known?.get(row.account);
    const fresh = before === undefined ? gained > nowMs - FRESH_GAIN_MS : before !== gained;
    if (fresh && send.length < NOTICES_PER_LOOK_MAX) send.push({ account: row.account, count: resets.count });
  }
  return { known: seen, send };
}

/** The one toast for a look's new free resets: each account by its label, joined, never by
 * its id. Inputs: the resets to tell, at most `NOTICES_PER_LOOK_MAX`, and the labels by
 * account id. Output: the toast, or null when none arrived. */
export function resetNotice(send: readonly NewReset[], labels: ReadonlyMap<string, string>): string | null {
  if (send.length > NOTICES_PER_LOOK_MAX) throw new Error(`at most ${NOTICES_PER_LOOK_MAX} new free resets per look, got ${send.length}`);
  if (send.length === 0) return null;
  return send.map((item) => newResetLine(labels.get(item.account) ?? "An account", item.count)).join(" ");
}

/** Tells the owner when an account gains a free reset: while the engine runs, every
 * `RESET_WATCH_MS` it reads what the engine stored, never the detail rows, and shows one
 * toast with Open to Pools for the arrivals since the last look. Inputs: whether the engine
 * runs and the toast action. */
export function useResetNotice(running: boolean, showToast: (message: string, open?: Route) => void) {
  const toast = useRef(showToast);
  toast.current = showToast;
  useEffect(() => {
    if (!running) return;
    let live = true;
    let timer = 0;
    let known: Map<string, number> | null = null;
    const read = async () => {
      if (!live) return;
      const result = await loadExtras(false);
      if (!live) return;
      if (result.kind === "ok") {
        const next = freshResets(known, result.value, Date.now());
        known = next.known;
        if (next.send.length > 0) {
          const accounts = await listAccounts();
          if (!live) return;
          const notice = resetNotice(next.send, accountLabels(accounts.kind === "ok" ? accounts.value : []));
          if (notice !== null) toast.current(notice, POOLS_ROUTE);
        }
      }
      if (result.kind !== "not_ready") timer = window.setTimeout(read, RESET_WATCH_MS);
    };
    void read();
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [running]);
}
