import { call } from "./api";
import type { Result } from "./api";
import { PAGE_MAX, loadNotifications, routeOf, sendNotice, type NoticeAction, type NoticePage, type NoticeQuery, type NoticeReport, type NoticeTarget } from "./api-notifications";
import { goTo, highlight } from "./screens/notifications/panel";
import type { EngineState } from "./ui/shell";

export type SystemNotifications = "available" | "unavailable";

/** A notification the owner clicked: its store item and the page it opens. */
export interface Clicked {
  id: number;
  target: NoticeTarget;
}

/** What the engine lost watch knows: whether the engine ran since the app opened, and
 * whether it was lost since it last ran. */
export interface EngineWatch {
  ran: boolean;
  lost: boolean;
}

export const NOTIFY_PER_READ_MAX = 3;

export const UNAVAILABLE_LINE = "System notifications are unavailable on this system.";
export const AVAILABLE_LINE = "While CrazyAgent is in the background, macOS shows a notification for each item Settings, Notifications sends there. If none appear, macOS is blocking them: allow CrazyAgent in System Settings, Notifications.";
export const FAILED_LINE = "Couldn't show a macOS notification. Check that CrazyAgent is allowed in System Settings, Notifications.";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The plain line Settings shows about system notifications. Input: what the app reported,
 * or undefined while it loads. Output: the line, or null while loading or in a build
 * without the command. */
export function systemNotificationsLine(state: Result<SystemNotifications> | undefined): string | null {
  if (state === undefined || state.kind === "not_ready") return null;
  if (state.kind === "error") return UNAVAILABLE_LINE;
  return state.value === "available" ? AVAILABLE_LINE : UNAVAILABLE_LINE;
}

/** The store read one look of the watch takes: unfolded items in every filter newer than
 * the cursor, or on the first look only the store's newest id. Input: the cursor, null
 * before the first look. Output: the query. */
export function bannerQuery(kept: number | null): NoticeQuery {
  assert(kept === null || (Number.isSafeInteger(kept) && kept >= 0), "the cursor is a store id");
  return { filter: "all", project_id: null, kind: null, fold: false, before: null, after: kept, limit: kept === null ? 1 : PAGE_MAX };
}

/** Picks the store items to send as banners now, oldest first, at most
 * `NOTIFY_PER_READ_MAX` while notifying: only items the engine marks for a banner that are
 * neither read nor resolved. The cursor stops at the last item sent while more wait, so a
 * burst goes out spread over reads; once every listed item is handled it moves to the
 * store's newest id. The first look and a store whose newest id fell below the cursor only
 * remember, so nothing old is sent. Inputs: the cursor or null, the page read with
 * `bannerQuery`, whether this read notifies. Output: the cursor now and the ids to send. */
export function freshNotices(kept: number | null, page: NoticePage, notifying: boolean): { kept: number; send: number[] } {
  if (kept === null || page.newest < kept) return { kept: page.newest, send: [] };
  if (!notifying) return { kept: Math.max(kept, page.newest), send: [] };
  const fresh = page.items.filter((item) => item.id > kept).sort((a, b) => a.id - b.id);
  const send: number[] = [];
  let next = kept;
  for (const item of fresh) {
    if (send.length === NOTIFY_PER_READ_MAX) return { kept: next, send };
    next = item.id;
    if (item.banner && item.read_ms === null && item.resolved_ms === null) send.push(item.id);
  }
  const moved = Math.max(next, page.newest);
  assert(moved >= kept && send.length <= NOTIFY_PER_READ_MAX, "the cursor only moves forward");
  return { kept: moved, send };
}

/** Opens the page of a clicked notification and marks only its item read, never the fold
 * around it. Inputs: the click and the way to go and to send. Output: the anchor of the part
 * to highlight, or null. */
export async function openClicked(clicked: Clicked, { go, send }: {
  go: (hash: string) => void;
  send: (action: NoticeAction) => Promise<Result<void>>;
}): Promise<string | null> {
  assert(Number.isSafeInteger(clicked.id) && clicked.id >= 1, "a click names a store item");
  const { hash, anchor } = routeOf(clicked.target);
  assert(hash.startsWith("#/"), "a notification opens a page of the app");
  go(hash);
  await send({ read: { ids: [clicked.id] } });
  return anchor;
}

/** The toast a notification that went wrong earns: only the plugin's refusal points at the
 * system setting; an item settled meanwhile or a full run says nothing. Input: what the app
 * answered. Output: the line, or null. */
export function failureLine(result: Result<void>): string | null {
  return result.kind === "error" && result.code === "notifications_failed" ? FAILED_LINE : null;
}

/** Whether a fresh item also goes to the system: only while the window is in the background
 * and the system shows notifications. Inputs: whether the window is in the background and
 * what the app reported. Output: true to notify. */
export function shouldNotify(background: boolean, state: Result<SystemNotifications> | undefined): boolean {
  return background && state?.kind === "ok" && state.value === "available";
}

/** Whether the window is in the background now: hidden, or not focused. */
export function inBackground(): boolean {
  return typeof document !== "undefined" && (document.visibilityState === "hidden" || !document.hasFocus());
}

/** Asks the app whether this system shows its notifications. Output: available on macOS,
 * otherwise unavailable. */
export function systemNotifications(): Promise<Result<SystemNotifications>> {
  return call<SystemNotifications>("desktop_system_notifications", {});
}

/** Asks the app for one macOS notification for a store item. The app reads the item from the
 * engine, checks the owner's settings and writes the text itself; the window only names it.
 * Input: the item id. Output: ok once the plugin took it or the app passed over it, which
 * macOS may still drop unreported, or the refusal. */
export function notifyNotice(id: number): Promise<Result<void>> {
  return call<void>("desktop_notify_notice", { id });
}

/** Takes the item whose notification the owner clicked, once. Output: the item and its
 * page, or null when no click waits. */
export function takeNotificationTarget(): Promise<Result<Clicked | null>> {
  return call<Clicked | null>("desktop_notification_target", {});
}

/** Steps the engine lost watch on a new engine state: the engine is lost only when it goes
 * down after it ran, and the report goes once, when it runs again, never on the first
 * connect; another engine version or a connect in progress changes nothing. Inputs: the
 * watch and the state. Output: the watch now and whether to report. */
export function engineLostStep(watch: EngineWatch, state: EngineState): { watch: EngineWatch; report: boolean } {
  assert(watch.ran || !watch.lost, "an engine that never ran was never lost");
  if (state === "running") return { watch: { ran: true, lost: false }, report: watch.lost };
  if (state === "down") return { watch: { ran: watch.ran, lost: watch.ran }, report: false };
  return { watch, report: false };
}

/** Reports an event only the app sees to the store as an Info item; the engine keeps one
 * item per update version and at most one engine lost a minute. Input: the report. Output:
 * ok once it stands, else the refusal; a refused report is not sent again until the next
 * update check or loss. */
export function reportNotice(report: NoticeReport): Promise<Result<void>> {
  return sendNotice({ report: { report } });
}

/** Watches the notification store for banners, one read per look: while the window is in
 * the background each new item the engine marks for a banner goes to macOS once, a few per
 * look. When the window comes to the front after a click it opens that item's page,
 * highlights its part and marks it read. Input: the toast action, used once when the plugin
 * refuses. Output: `look`, awaited once per alerts read, and `stop`, which ends the watch. */
export function noticeWatch(showToast: (message: string) => void) {
  let live = true;
  let kept: number | null = null;
  let failed = false;
  const system = systemNotifications();
  const onFocus = () => {
    void takeNotificationTarget().then(async (result) => {
      if (!live || result.kind !== "ok" || result.value === null) return;
      highlight(await openClicked(result.value, { go: goTo, send: sendNotice }));
    });
  };
  window.addEventListener("focus", onFocus);
  return {
    async look() {
      const result = await loadNotifications(bannerQuery(kept));
      if (!live || result.kind !== "ok") return;
      const notifying = shouldNotify(inBackground(), await system);
      const picked = freshNotices(kept, result.value, notifying);
      kept = picked.kept;
      for (const id of picked.send) {
        const line = failureLine(await notifyNotice(id));
        if (line && !failed) {
          failed = true;
          showToast(line);
        }
      }
    },
    stop() {
      live = false;
      window.removeEventListener("focus", onFocus);
    },
  };
}
