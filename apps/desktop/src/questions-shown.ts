import { useSyncExternalStore } from "react";
import { QUESTIONS_SHOWN_DEFAULT, questionsShown } from "./api-questions";
import type { QuestionsShown } from "./api-questions";
import { choiceListeners } from "./state";

const QUESTIONS_SHOWN_KEY = "crazyagent.questions-shown";
const questionsHeard = choiceListeners(QUESTIONS_SHOWN_KEY);

/** Reads where a waiting question shows on a project page, as kept in this app (not in the
 * engine). Output: the choice; the default when storage is unavailable or holds something
 * else. */
export function readQuestionsShown(): QuestionsShown {
  try {
    return questionsShown(window.localStorage.getItem(QUESTIONS_SHOWN_KEY));
  } catch {
    return QUESTIONS_SHOWN_DEFAULT;
  }
}

/** Remembers where a waiting question shows on a project page and tells this window's
 * readers at once; other windows hear it through the storage event. Input: the choice.
 * Output: true once stored; false when storage refused it, and then nobody is told. */
export function saveQuestionsShown(choice: QuestionsShown): boolean {
  try {
    window.localStorage.setItem(QUESTIONS_SHOWN_KEY, choice);
  } catch {
    return false;
  }
  questionsHeard.tell();
  return true;
}

/** Calls `listener` after each Questions show choice, made in this window or in another
 * window of the app. Input: the listener. Output: the call that stops it. */
export function onQuestionsShown(listener: () => void): () => void {
  return questionsHeard.listen(listener);
}

/** Follows where a waiting question shows, the one source every reader of the choice uses,
 * so a choice made over an open project page applies to it at once. Output: the stored
 * choice, rendering again on a change. */
export function useQuestionsShown(): QuestionsShown {
  return useSyncExternalStore(onQuestionsShown, readQuestionsShown, readQuestionsShown);
}
