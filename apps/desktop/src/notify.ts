import { useEffect, useRef } from "react";
import { call } from "./api";
import type { Result } from "./api";
import { loadQuestionCounts, loadQuestions, pendingByProject } from "./api-questions";
import { navigate } from "./ui";

export type SystemNotifications = "available" | "unavailable";

/** One pending question the watch knows of: its id and project. */
export interface PendingQuestion {
  id: number;
  project_id: string;
}

export const QUESTION_READ_MS = 10_000;
export const PAGES_MAX = 2;

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

/** Finds the pending questions that appeared since the watch last looked. The first look only
 * remembers what is pending, so an app start never notifies for old questions. A settled
 * question never returns to pending, so the ids known now are exactly the ones pending now.
 * Inputs: the ids known before, or null before the first look, and every pending question
 * read now. Output: the ids known now and the fresh questions, oldest first. */
export function freshQuestions(known: ReadonlySet<number> | null, pending: readonly PendingQuestion[]): { known: Set<number>; fresh: PendingQuestion[] } {
  const fresh = known === null ? [] : pending.filter((item) => !known.has(item.id)).sort((a, b) => a.id - b.id);
  return { known: new Set(pending.map((item) => item.id)), fresh };
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

/** Shows one macOS notification for a pending question. The app reads the question from the
 * engine and writes the text itself; the window only names it. Inputs: the project id and the
 * question id. Output: ok once handed to macOS, or the refusal. */
export function notifyQuestion(projectId: string, questionId: number): Promise<Result<void>> {
  return call<void>("desktop_notify_question", { projectId, questionId });
}

/** Takes the project whose question notification the owner clicked, once. Output: the
 * project id, or null when no click waits. */
export function takeNotificationTarget(): Promise<Result<string | null>> {
  return call<string | null>("desktop_notification_target", {});
}

/** Reads every pending question of one project, at most `PAGES_MAX` pages; pending ones come
 * first, oldest first. Input: the project id. Output: the pending questions, or null when a
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

/** Watches pending questions while the engine runs: every 10 s it reads them and, while the
 * window is in the background, sends one macOS notification per new question. When the window
 * comes to the front after a click it opens that project's Questions tab. Inputs: whether the
 * engine runs and the toast action, used once when macOS refuses a notification. */
export function useQuestionNotifications(running: boolean, showToast: (message: string) => void) {
  const toast = useRef(showToast);
  toast.current = showToast;
  useEffect(() => {
    if (!running) return;
    let live = true;
    let timer = 0;
    let known: Set<number> | null = null;
    let failed = false;
    const system = systemNotifications();
    const read = async () => {
      const counts = await loadQuestionCounts();
      if (!live) return;
      if (counts.kind === "ok") {
        const pages = await Promise.all([...pendingByProject(counts.value).keys()].map(pendingOf));
        if (live && pages.every((page) => page !== null)) {
          const watched = freshQuestions(known, pages.flatMap((page) => page ?? []));
          known = watched.known;
          if (shouldNotify(inBackground(), await system)) {
            for (const question of watched.fresh) {
              const sent = await notifyQuestion(question.project_id, question.id);
              if (sent.kind === "error" && !failed) {
                failed = true;
                toast.current(FAILED_LINE);
              }
            }
          }
        }
      }
      if (live) timer = window.setTimeout(read, QUESTION_READ_MS);
    };
    const onFocus = () => {
      void takeNotificationTarget().then((result) => {
        if (live && result.kind === "ok" && result.value !== null) navigate({ name: "project", id: result.value, tab: "questions" });
      });
    };
    window.addEventListener("focus", onFocus);
    void read();
    return () => {
      live = false;
      window.clearTimeout(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [running]);
}
