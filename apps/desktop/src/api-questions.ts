import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import { displayText } from "./screens/conversation";

export type QuestionStatus = "pending" | "answered" | "dismissed" | "answered_in_terminal";

/** One question an agent or an engine step asked the owner, as the engine recorded it. The
 * `version` goes back with an answer or a dismissal, so a question that changed since the
 * owner read it is refused as stale. */
export interface OwnerQuestion {
  id: number;
  project_id: string;
  asker: string;
  question: string;
  options: string[];
  recommended: string | null;
  context: string | null;
  status: QuestionStatus;
  version: number;
  answer: string | null;
  created_ms: number;
  settled_ms: number | null;
}

/** One page of a project's questions; pass `next` back as `after` for the older ones. */
export interface QuestionPage {
  items: OwnerQuestion[];
  next: number | null;
}

/** How many questions of one project wait for the owner. */
export interface QuestionCount {
  project_id: string;
  pending: number;
}

/** The owner's answer: one of the question's options by position, or typed text. */
export type QuestionChoice = { option: number } | { text: string };

export const ANSWER_BYTES_MAX = 4096;
export const OPTIONS_MAX = 6;

/** What the owner reads when a question changed since it was shown. */
export const STALE_LINE = "This question changed. Review it again.";

const REFUSALS: Record<string, string> = {
  questions_stale: STALE_LINE,
  questions_not_found: "This question is no longer there. The list shows what is left.",
  questions_answer_invalid: `Pick one of the options, or write an answer of up to ${ANSWER_BYTES_MAX} bytes.`,
  questions_option_invalid: "That option isn't on this question. Review it again.",
  questions_text_invalid: "The answer has hidden characters CrazyAgent can't keep. Remove them, then answer again.",
  questions_project_missing: "This project is no longer in CrazyAgent.",
  questions_unavailable: "Questions aren't ready in this engine yet.",
  questions_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
};

/** The refusal codes of the questions protocol that have their own sentence. */
export const QUESTIONS_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused question action. Inputs: the refusal code and the engine
 * cause behind it. Output: the sentence, or the plain fallback naming the code. */
export function questionsRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** Tells whether a questions answer means the view is not ready yet: this build has no
 * questions commands, or the engine answered questions_unavailable. Input: the answer. */
export function notReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "questions_unavailable");
}

/** Who asked, in plain words, from the engine's asker name such as `master:<session>`,
 * `agent:<team id>`, `engine:research:<kind>` or `terminal:<agent>`. Input: the asker.
 * Output: the label. */
export function askerLabel(asker: string): string {
  const [kind, ...rest] = asker.split(":");
  const tail = rest.join(":");
  if (kind === "master") return "Master";
  if (kind === "agent" && tail) return `Agent ${displayText(tail)}`;
  if (kind === "terminal" && tail) return `Asked in the terminal of ${displayText(tail)}`;
  if (kind === "engine") {
    if (tail === "mapping") return "Project mapping";
    if (tail === "planning") return "Roadmap planning";
    if (tail.startsWith("research:")) return `Research: ${displayText(tail.slice("research:".length).replaceAll("_", " "))}`;
    if (tail.startsWith("landing:")) return "Landing changes";
  }
  return displayText(asker);
}

/** The status word and tone of a question's badge. Input: the status. Output: the badge. */
export function statusBadge(status: QuestionStatus): { word: string; tone: "warn" | "ok" | "neutral" } {
  switch (status) {
    case "pending": return { word: "Waiting for you", tone: "warn" };
    case "answered": return { word: "Answered", tone: "ok" };
    case "answered_in_terminal": return { word: "Answered in the terminal", tone: "ok" };
    case "dismissed": return { word: "Dismissed", tone: "neutral" };
  }
}

/** Pending questions first, the one waiting longest on top, then the settled ones, the most
 * recently settled on top. Input: the questions. Output: a new ordered list. */
export function orderQuestions(questions: readonly OwnerQuestion[]): OwnerQuestion[] {
  const pending = questions.filter((item) => item.status === "pending").sort((a, b) => a.created_ms - b.created_ms || a.id - b.id);
  const settled = questions.filter((item) => item.status !== "pending").sort((a, b) => (b.settled_ms ?? b.created_ms) - (a.settled_ms ?? a.created_ms) || b.id - a.id);
  return [...pending, ...settled];
}

/** The typed answer as it is sent: trimmed, not blank and at most `ANSWER_BYTES_MAX` bytes.
 * Input: the field text. Output: the answer, or null when it cannot be sent. */
export function typedAnswer(text: string): string | null {
  const answer = text.trim();
  if (answer.length === 0 || new TextEncoder().encode(answer).length > ANSWER_BYTES_MAX) return null;
  return answer;
}

/** Whether an option is the question's recommended answer. Inputs: the question and the
 * option's position. Output: true for the recommended one. */
export function isRecommended(question: Pick<OwnerQuestion, "options" | "recommended">, index: number): boolean {
  return question.recommended !== null && question.options[index] === question.recommended;
}

/** The sentence the owner reads after an answer or a dismissal went through. Input: what
 * was done. Output: the sentence. */
export function settledNote(kind: "answered" | "dismissed"): string {
  return kind === "answered" ? "Answered. The agent that asked gets your answer once." : "Dismissed. The agent that asked is told once.";
}

/** Reads one page of a project's questions, pending first. Inputs: the project id and the
 * `next` of the page before, or null for the first page. Output: the page. */
export function loadQuestions(projectId: string, after: number | null): Promise<Result<QuestionPage>> {
  return call<QuestionPage>("desktop_questions", { projectId, after });
}

/** Answers a pending question with one option or typed text. Inputs: the question id, the
 * version the owner read and the choice. Output: ok once recorded, or the refusal; a
 * question that changed is `questions_stale`. */
export function answerQuestion(id: number, version: number, choice: QuestionChoice): Promise<Result<void>> {
  const option = "option" in choice ? choice.option : null;
  const text = "text" in choice ? choice.text : null;
  return call<void>("desktop_answer_question", { id, version, option, text });
}

/** Dismisses a pending question; the asker is told once. Inputs: the question id and the
 * version the owner read. Output: ok once recorded, or the refusal. */
export function dismissQuestion(id: number, version: number): Promise<Result<void>> {
  return call<void>("desktop_dismiss_question", { id, version });
}

/** Reads how many questions wait for the owner in each project. Output: the counts. */
export function loadQuestionCounts(): Promise<Result<QuestionCount[]>> {
  return call<QuestionCount[]>("desktop_question_counts", {});
}

/** The pending count of each project that has any. Input: the counts. Output: the map. */
export function pendingByProject(counts: readonly QuestionCount[]): Map<string, number> {
  return new Map(counts.filter((item) => item.pending > 0).map((item) => [item.project_id, item.pending]));
}

/** The unseen question notifications of each project, which the pending count already
 * covers. Input: the owner notifications. Output: the map. */
export function unseenQuestionAlerts(notifications: readonly { project_id: string; kind: string; seen_ms: number | null }[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const item of notifications) {
    if (item.kind === "question" && item.seen_ms === null) counts.set(item.project_id, (counts.get(item.project_id) ?? 0) + 1);
  }
  return counts;
}

/** What each project's sidebar flag counts: its unseen notifications other than questions,
 * plus its pending questions, so a question counts once and stays counted until answered
 * or dismissed. Inputs: the unseen notifications, the unseen question notifications and the
 * pending questions, by project. Output: the nonzero counts by project. */
export function withPendingQuestions(unseen: ReadonlyMap<string, number>, questionAlerts: ReadonlyMap<string, number>, pending: ReadonlyMap<string, number>): Map<string, number> {
  const flags = new Map<string, number>();
  for (const project of new Set([...unseen.keys(), ...pending.keys()])) {
    const alerts = unseen.get(project) ?? 0;
    const count = alerts - Math.min(alerts, questionAlerts.get(project) ?? 0) + (pending.get(project) ?? 0);
    if (count > 0) flags.set(project, count);
  }
  return flags;
}
