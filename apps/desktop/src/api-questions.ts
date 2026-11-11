import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import { displayText } from "./screens/conversation";

export type QuestionStatus = "pending" | "answered" | "dismissed" | "answered_in_terminal";

/** How the owner answers a question: pick one option, pick any options, or write free text. */
export type QuestionKind = "single" | "multi" | "text";

/** One question an agent or an engine step asked the owner, as the engine recorded it. The
 * `version` goes back with an answer or a dismissal, so a question that changed since the
 * owner read it is refused as stale. */
export interface OwnerQuestion {
  id: number;
  project_id: string;
  asker: string;
  question: string;
  kind: QuestionKind;
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

/** The owner's answer: the picked options by position, in option order, and a comment. A
 * pick, a comment or both are given; a text question takes the comment only. */
export interface QuestionChoice {
  picks: number[];
  comment: string | null;
}

/** The last choice of every single and multi question; it opens the comment box. */
export const COMMENT_LABEL = "Add a comment";
export const ANSWER_BYTES_MAX = 4096;
export const COMMENT_BYTES_MAX = 2048;
export const OPTIONS_MAX = 6;

/** Where a waiting question shows while the owner is on a project, besides the Questions tab:
 * a pop-up on the Conversation tab, a pop-up on every project tab, or the Questions tab only. */
export type QuestionsShown = "conversation" | "every" | "tab";
export const QUESTIONS_SHOWN_DEFAULT: QuestionsShown = "conversation";
export const QUESTIONS_SHOWN_CHOICES: { value: QuestionsShown; label: string }[] = [
  { value: "conversation", label: "Pop-up on Conversation" },
  { value: "every", label: "Pop-up on every tab" },
  { value: "tab", label: "Questions tab only" },
];

/** Reads the stored Questions show choice. Input: the stored text, if any. Output: the
 * choice, or `QUESTIONS_SHOWN_DEFAULT` for anything else. */
export function questionsShown(stored: string | null): QuestionsShown {
  const known = QUESTIONS_SHOWN_CHOICES.find((choice) => choice.value === stored);
  return known ? known.value : QUESTIONS_SHOWN_DEFAULT;
}

/** What the owner reads when a question changed since it was shown. */
export const STALE_LINE = "This question changed. Review it again.";

const REFUSALS: Record<string, string> = {
  questions_stale: STALE_LINE,
  questions_not_found: "This question is no longer there. The list shows what is left.",
  questions_answer_invalid: `Pick an option or add a comment. A comment can be up to ${COMMENT_BYTES_MAX} bytes with a pick, ${ANSWER_BYTES_MAX} on its own.`,
  questions_option_invalid: "That option isn't on this question. Review it again.",
  questions_text_invalid: "The answer has hidden characters CrazyAgent can't keep. Remove them, then answer again.",
  questions_project_missing: "This project is no longer in CrazyAgent.",
  questions_unavailable: "Questions aren't ready in this engine yet.",
  questions_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  questions_in_terminal: "This question was asked in an agent's terminal. Answer it there.",
  questions_delivery_full: "The master's conversation is full right now. Try again after it reads its messages.",
  owner_authentication_failed: "CrazyAgent couldn't confirm this app with the engine. Restart the app, then try again.",
  owner_authority_unavailable: "The engine isn't ready for owner actions yet. Try again in a moment.",
  owner_busy: "The engine is busy with other owner requests. Try again in a moment.",
  owner_grant_stale_or_revoked: "This app's access to the engine changed. Reconnect, then try again.",
  owner_request_failed: "The engine couldn't take this request. Try again.",
  owner_root_replaced_or_unsafe: "The engine's data folder changed or isn't safe. Restart the engine, then try again.",
  owner_root_unavailable: "The engine's data folder isn't available. Restart the engine, then try again.",
  owner_socket_replaced: "The engine was restarted. Reconnect, then try again.",
  owner_socket_unsafe: "The engine connection isn't safe to use. Restart the engine, then try again.",
  peer_owner_mismatch: "The engine runs as another user. Start it as yourself, then try again.",
  versioned_item_invalid: "CrazyAgent couldn't record a change to this question. Review it again.",
  versioned_unavailable: "CrazyAgent couldn't record this right now. Try again in a moment.",
  changes_unavailable: "The engine's records aren't available right now. Try again in a moment.",
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
 * `agent:<team id>`, `engine:research:<kind>` or `terminal:<agent id>`. Input: the asker.
 * Output: the label. */
export function askerLabel(asker: string): string {
  const [kind, ...rest] = asker.split(":");
  const tail = rest.join(":");
  if (kind === "master") return "Master";
  if (kind === "agent" && tail) return `Agent ${displayText(tail)}`;
  if (kind === "terminal" && tail) return `Asked in the terminal of agent ${displayText(tail)}`;
  if (kind === "engine") {
    if (tail === "mapping") return "Project mapping";
    if (tail === "planning") return "Roadmap planning";
    if (tail.startsWith("research:")) return `Research: ${displayText(tail.slice("research:".length).replaceAll("_", " "))}`;
    if (tail.startsWith("landing:")) return "Landing changes";
  }
  return displayText(asker);
}

/** Tells whether a question was asked in an agent's own terminal, where the owner answers it;
 * the engine refuses an answer from the Questions view. Input: the question. Output: the
 * answer. */
export function askedInTerminal(question: Pick<OwnerQuestion, "asker">): boolean {
  return question.asker.startsWith("terminal:");
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

/** The typed answer as it is sent: trimmed, not blank and at most `max` bytes, by default
 * `ANSWER_BYTES_MAX`. Inputs: the field text and the bound. Output: the answer, or null when
 * it cannot be sent. */
export function typedAnswer(text: string, max: number = ANSWER_BYTES_MAX): string | null {
  const answer = text.trim();
  if (answer.length === 0 || new TextEncoder().encode(answer).length > max) return null;
  return answer;
}

/** Whether an option is the question's recommended answer. Inputs: the question and the
 * option's position. Output: true for the recommended one. */
export function isRecommended(question: Pick<OwnerQuestion, "options" | "recommended">, index: number): boolean {
  return question.recommended !== null && question.options[index] === question.recommended;
}

/** What the owner is told after settling a question, by who asked: an agent or the master
 * hears once; an engine step's answer goes once to the project's master, if its conversation
 * has room; nothing is sent to a terminal. Inputs: what the owner did and the asker. Output:
 * the line. */
export function settledNote(kind: "answered" | "dismissed", asker: string): string {
  const [source] = asker.split(":");
  if (source === "terminal" && kind === "dismissed") return "Dismissed. Nothing is sent to the agent's terminal.";
  if (source === "engine") {
    return kind === "answered"
      ? "Answered. The project's master gets your answer once, if its conversation has room."
      : "Dismissed. The project's master is told once, if its conversation has room.";
  }
  return kind === "answered" ? "Answered. The agent that asked gets your answer once." : "Dismissed. The agent that asked is told once.";
}

/** What dismissing a question tells whoever asked, for the confirm dialog. Input: the asker.
 * Output: the line. */
export function dismissNote(asker: string): string {
  const [source] = asker.split(":");
  if (source === "terminal") return "Nothing is sent to the agent's terminal. If the question still waits there, answer it in the terminal.";
  if (source === "engine") return "The CrazyAgent step that asked takes no action on its own. The project's master is told once, if its conversation has room.";
  return "The agent that asked is told you dismissed it, once. It can ask again if it still needs an answer.";
}

/** Reads one page of a project's questions, pending first. Inputs: the project id and the
 * `next` of the page before, or null for the first page. Output: the page. */
export function loadQuestions(projectId: string, after: number | null): Promise<Result<QuestionPage>> {
  return call<QuestionPage>("desktop_questions", { projectId, after });
}

/** Answers a pending question with picks, a comment or both. Inputs: the question id, the
 * version the owner read and the choice. Output: ok once recorded, or the refusal; a
 * question that changed is `questions_stale`. */
export function answerQuestion(id: number, version: number, choice: QuestionChoice): Promise<Result<void>> {
  return call<void>("desktop_answer_question", { id, version, picks: choice.picks, comment: choice.comment });
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
 * covers, from the engine's full per-project count rather than the capped notification list.
 * Input: the engine's per-project unseen counts. Output: the map. */
export function unseenQuestionAlerts(projects: readonly { project_id: string; questions?: number }[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const item of projects) {
    if ((item.questions ?? 0) > 0) counts.set(item.project_id, item.questions ?? 0);
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
