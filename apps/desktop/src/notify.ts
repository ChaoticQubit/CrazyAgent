import { useEffect } from "react";
import { call } from "./api";
import type { Result } from "./api";
import { navigate } from "./ui";

export type SystemNotifications = "available" | "unavailable";

/** One pending question the watch knows of: its id and project. */
export interface PendingQuestion {
  id: number;
  project_id: string;
}

export const KNOWN_MAX = 1024;

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
 * remembers what is pending, so an app start never notifies for old questions; afterwards a
 * question is fresh once, however often it is read again. At most `KNOWN_MAX` ids are kept,
 * the newest. Inputs: the ids known before, or null before the first look, and the pending
 * questions read now. Output: the ids known now and the fresh questions, oldest first. */
export function freshQuestions(known: ReadonlySet<number> | null, pending: readonly PendingQuestion[]): { known: Set<number>; fresh: PendingQuestion[] } {
  const fresh = known === null ? [] : pending.filter((item) => !known.has(item.id)).sort((a, b) => a.id - b.id);
  const ids = [...(known ?? []), ...pending.map((item) => item.id)];
  const kept = new Set([...new Set(ids)].sort((a, b) => b - a).slice(0, KNOWN_MAX));
  return { known: kept, fresh };
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

/** Opens the Questions tab of the project whose notification the owner clicked, each time
 * the window comes to the front after a click. */
export function useNotificationClicks() {
  useEffect(() => {
    let live = true;
    const onFocus = () => {
      void takeNotificationTarget().then((result) => {
        if (live && result.kind === "ok" && result.value !== null) navigate({ name: "project", id: result.value, tab: "questions" });
      });
    };
    window.addEventListener("focus", onFocus);
    return () => {
      live = false;
      window.removeEventListener("focus", onFocus);
    };
  }, []);
}
