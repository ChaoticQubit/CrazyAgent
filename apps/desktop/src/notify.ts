import { call } from "./api";
import type { Result } from "./api";
import { loadQuestions, pendingByProject, type QuestionCount } from "./api-questions";
import { navigate } from "./ui";

export type SystemNotifications = "available" | "unavailable";

/** One pending question the watch knows of: its id and project. */
export interface PendingQuestion {
  id: number;
  project_id: string;
}

export const PAGES_MAX = 64;
export const NOTIFY_PER_READ_MAX = 3;

export const UNAVAILABLE_LINE = "System notifications are unavailable on this system.";
export const AVAILABLE_LINE = "When a question waits for you while CrazyAgent is in the background, macOS shows a notification. If none appear, macOS is blocking them: allow CrazyAgent in System Settings, Notifications.";
export const FAILED_LINE = "Couldn't show a macOS notification. Check that CrazyAgent is allowed in System Settings, Notifications.";

/** The plain line Settings shows about system notifications. Input: what the app reported,
 * or undefined while it loads. Output: the line, or null while loading or in a build
 * without the command. */
export function systemNotificationsLine(state: Result<SystemNotifications> | undefined): string | null {
  if (state === undefined || state.kind === "not_ready") return null;
  if (state.kind === "error") return UNAVAILABLE_LINE;
  return state.value === "available" ? AVAILABLE_LINE : UNAVAILABLE_LINE;
}

/** Picks the new pending questions to notify now, oldest first, at most `NOTIFY_PER_READ_MAX`
 * while notifying; the rest stay unknown for the next read, so a burst arrives spread out. The
 * first look only remembers, so an app start notifies nothing old. Inputs: the ids known
 * before or null, every pending question read now, whether this read notifies. Output: the
 * ids known now and the questions to send. */
export function freshQuestions(known: ReadonlySet<number> | null, pending: readonly PendingQuestion[], notifying: boolean): { known: Set<number>; send: PendingQuestion[] } {
  const fresh = known === null ? [] : pending.filter((item) => !known.has(item.id)).sort((a, b) => a.id - b.id);
  const send = notifying ? fresh.slice(0, NOTIFY_PER_READ_MAX) : [];
  const later = new Set(notifying ? fresh.slice(NOTIFY_PER_READ_MAX).map((item) => item.id) : []);
  return { known: new Set(pending.map((item) => item.id).filter((id) => !later.has(id))), send };
}

/** The toast a notification that went wrong earns: only the plugin's refusal points at the
 * system setting; a question settled meanwhile or a full run says nothing. Input: what the
 * app answered. Output: the line, or null. */
export function failureLine(result: Result<void>): string | null {
  return result.kind === "error" && result.code === "notifications_failed" ? FAILED_LINE : null;
}

/** Whether a fresh question also goes to the system: only while the window is in the
 * background and the system shows notifications. Inputs: whether the window is in the
 * background and what the app reported. Output: true to notify. */
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

/** Asks the app for one macOS notification for a pending question. The app reads the question
 * from the engine and writes the text itself; the window only names it. Inputs: the project id
 * and the question id. Output: ok once the notification plugin took it, which macOS may still
 * drop unreported, or the refusal. */
export function notifyQuestion(projectId: string, questionId: number): Promise<Result<void>> {
  return call<void>("desktop_notify_question", { projectId, questionId });
}

/** Takes the project whose question notification the owner clicked, once. Output: the
 * project id, or null when no click waits. */
export function takeNotificationTarget(): Promise<Result<string | null>> {
  return call<string | null>("desktop_notification_target", {});
}

/** Reads every pending question of one project; pending ones come first, oldest first, every
 * page holds at least one while any remain, and a project holds at most 64, so `PAGES_MAX`
 * pages reach them all. Input: the project id. Output: the pending questions, or null when a
 * read failed so the watch skips this look. */
async function pendingOf(projectId: string): Promise<PendingQuestion[] | null> {
  const found: PendingQuestion[] = [];
  let after: number | null = null;
  for (let page = 0; page < PAGES_MAX; page += 1) {
    const result = await loadQuestions(projectId, after);
    if (result.kind !== "ok") return null;
    const pending = result.value.items.filter((item) => item.status === "pending");
    found.push(...pending.map((item) => ({ id: item.id, project_id: item.project_id })));
    if (result.value.next === null || pending.length < result.value.items.length) break;
    after = result.value.next;
  }
  return found;
}

/** Watches pending questions from the counts the app's alerts read takes, so the counts are
 * read once per look: each look reads the pending questions of every project with any and,
 * while the window is in the background, sends one macOS notification per new question, a few
 * per look. When the window comes to the front after a click it opens that project's
 * Questions tab. Input: the toast action, used once when the plugin refuses. Output: `look`,
 * awaited once per read with that read's counts, and `stop`, which ends the watch. */
export function questionWatch(showToast: (message: string) => void) {
  let live = true;
  let known: Set<number> | null = null;
  let failed = false;
  const system = systemNotifications();
  const onFocus = () => {
    void takeNotificationTarget().then((result) => {
      if (live && result.kind === "ok" && result.value !== null) navigate({ name: "project", id: result.value, tab: "questions" });
    });
  };
  window.addEventListener("focus", onFocus);
  return {
    async look(counts: readonly QuestionCount[]) {
      const pages = await Promise.all([...pendingByProject(counts).keys()].map(pendingOf));
      if (!live || pages.some((page) => page === null)) return;
      const notifying = shouldNotify(inBackground(), await system);
      const watched = freshQuestions(known, pages.flatMap((page) => page ?? []), notifying);
      known = watched.known;
      for (const question of watched.send) {
        const line = failureLine(await notifyQuestion(question.project_id, question.id));
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
