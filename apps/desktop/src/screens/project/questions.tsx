import { useEffect, useId, useState } from "react";
import type { Failure, Result } from "../../api";
import { ANSWER_BYTES_MAX, BULK_DISMISS_MAX, COMMENT_BYTES_MAX, COMMENT_LABEL, OPTIONS_MAX, answerQuestion, askedInTerminal, askerLabel, bulkDismissNote, bulkDismissedLine, dismissNote, dismissQuestion, dismissQuestions, isRecommended, loadQuestions, notReady, orderQuestions, questionsRefusal, settledNote, statusBadge, typedAnswer } from "../../api-questions";
import type { OwnerQuestion, QuestionChoice, QuestionKind, QuestionPage } from "../../api-questions";
import { Button, ConfirmDialog, TextArea } from "../../ui";
import { displayText } from "../conversation";
import { ago } from "./research";
import { Badge, NotReady, ProjectEmpty } from "./shared";
import type { ProjectData } from "./shared";
import "./questions.css";

export const LIST_POLL_MS = 5000;

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? questionsRefusal({ code: failure.code, cause: failure.cause }) : "";
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The most bytes of the comment box: `COMMENT_BYTES_MAX` beside picks, so both fit one
 * answer, `ANSWER_BYTES_MAX` on its own. Input: how many options are picked. Output: the
 * bound. */
export function commentBytesMax(picked: number): number {
  return picked > 0 ? COMMENT_BYTES_MAX : ANSWER_BYTES_MAX;
}

/** The answer to send from the card's state, or null when it would be refused: picks sorted
 * and within the kind's count, the comment trimmed, dropped when blank and within its bound,
 * at least one of them. Inputs: the kind, the picked positions and the comment box, null
 * while it is closed. Output: the choice or null. */
export function choiceOf(kind: QuestionKind, picks: readonly number[], comment: string | null): QuestionChoice | null {
  const sorted = [...new Set(picks)].sort((left, right) => left - right);
  const allowed = kind === "text" ? 0 : kind === "single" ? 1 : OPTIONS_MAX;
  if (sorted.length > allowed || sorted.some((at) => !Number.isInteger(at) || at < 0 || at >= OPTIONS_MAX)) return null;
  const blank = comment === null || comment.trim().length === 0;
  const words = blank ? null : typedAnswer(comment, commentBytesMax(sorted.length));
  if ((!blank && words === null) || (sorted.length === 0 && words === null)) return null;
  assert(sorted.every((at, index) => index === 0 || sorted[index - 1] < at), "picks are sent once each, in option order");
  assert(words === null || words.length > 0, "a sent comment is never blank");
  return { picks: sorted, comment: words };
}

/** The picks after the owner clicks one option: a single question keeps only that option, a
 * multi question adds it, and clicking a picked option clears it, so a comment can stand
 * alone. Inputs: the kind, the picks and the clicked position. Output: the picks, in option
 * order. */
export function picksAfter(kind: QuestionKind, picks: readonly number[], index: number): number[] {
  if (kind === "text") return [];
  assert(Number.isInteger(index) && index >= 0 && index < OPTIONS_MAX, "a clicked option is one of the question's");
  if (picks.includes(index)) return picks.filter((at) => at !== index);
  const next = kind === "single" ? [index] : [...picks, index].sort((left, right) => left - right);
  assert(kind !== "single" || next.length === 1, "a single question holds one pick");
  return next;
}

/** One question: who asked and when, the question, its context and, while it waits, its
 * choices: radios for a single question, checkboxes for a multi one, the recommended option
 * marked, then an Add a comment checkbox outside that group, which opens the comment box and
 * moves the keyboard into it; a text question shows only its answer box. A question asked in an agent's terminal is answered there, so it offers only
 * Dismiss. A settled question shows its status and answer. Inputs: the question, now, whether
 * an action runs and the handlers. Output: the card. */
export function QuestionCard({ question, now, busy, onAnswer, onDismiss }: {
  question: OwnerQuestion;
  now: number;
  busy: boolean;
  onAnswer: (question: OwnerQuestion, choice: QuestionChoice) => void;
  onDismiss: (question: OwnerQuestion) => void;
}) {
  const [picks, setPicks] = useState<number[]>([]);
  const [commenting, setCommenting] = useState(false);
  const [text, setText] = useState("");
  const name = useId();
  const field = useId();
  const pending = question.status === "pending";
  const terminal = askedInTerminal(question);
  const typing = question.kind === "text";
  const writing = typing || commenting;
  const choice = choiceOf(question.kind, picks, writing ? text : null);
  const bytesMax = commentBytesMax(picks.length);
  const tooLong = writing && text.trim().length > 0 && typedAnswer(text, bytesMax) === null;
  const badge = statusBadge(question.status, question.closed_by ?? null);
  const answerLabel = question.closed_by === "relay" ? "The master reports: " : "Answer: ";
  const suggested = typing ? question.recommended : null;
  const input = question.kind === "multi" ? "checkbox" : "radio";
  useEffect(() => {
    if (commenting) document.getElementById(field)?.focus();
  }, [commenting, field]);
  return <section className="pj-stack-s qs-card" aria-label={`Question from ${askerLabel(question.asker)}`}>
    <div className="pj-row between">
      <strong>{askerLabel(question.asker)}</strong>
      <Badge word={badge.word} tone={badge.tone} />
    </div>
    <p className="pj-small pj-secondary">{ago(question.created_ms, now)}</p>
    <p className="qs-question">{displayText(question.question)}</p>
    {question.context && <details className="qs-context">
      <summary>Context</summary>
      <p>{displayText(question.context)}</p>
    </details>}
    {pending && terminal && <>
      <p className="pj-small pj-secondary">Answer it in that agent's terminal. It is marked answered here when the question leaves the screen.</p>
      <div className="pj-row">
        <Button variant="quiet" disabled={busy} onClick={() => onDismiss(question)}>Dismiss</Button>
      </div>
    </>}
    {pending && !terminal && <>
      {!typing && <fieldset className="qs-options">
        <legend className="pj-small pj-secondary">{question.kind === "multi" ? "Pick any" : "Pick one"}</legend>
        {question.options.map((option, index) => <label className="qs-option" key={`${index}:${option}`}>
          <input type={input} name={name} checked={picks.includes(index)} disabled={busy} onChange={() => {}}
            onClick={() => setPicks(picksAfter(question.kind, picks, index))} />
          <span>{displayText(option)}</span>
          {isRecommended(question, index) && <span className="qs-recommended pj-small">Recommended</span>}
        </label>)}
      </fieldset>}
      {!typing && <label className="qs-option qs-comment">
        <input type="checkbox" aria-expanded={commenting} aria-controls={commenting ? field : undefined} checked={commenting} disabled={busy}
          onChange={(event) => setCommenting(event.target.checked)} />
        <span>{COMMENT_LABEL}</span></label>}
      {suggested && <div className="pj-row">
        <span className="pj-small pj-secondary">{`Suggested: ${displayText(suggested)}`}</span>
        <Button size="small" disabled={busy} onClick={() => setText(suggested)}>Use suggested answer</Button>
      </div>}
      {writing && <TextArea id={field} label={typing ? "Your answer" : "Your comment"} value={text} rows={3} onValue={setText}
        error={tooLong ? `Write at most ${bytesMax} bytes.` : undefined} />}
      <div className="pj-row">
        <Button variant="primary" disabled={busy || choice === null} onClick={() => { if (choice !== null) onAnswer(question, choice); }}>Answer</Button>
        <Button variant="quiet" disabled={busy} onClick={() => onDismiss(question)}>Dismiss</Button>
      </div>
    </>}
    {!pending && question.answer !== null && <p className="qs-answer"><span className="pj-secondary">{answerLabel}</span>{displayText(question.answer)}</p>}
  </section>;
}

/** Merges the newest page with the older pages the owner opened: the newest page wins for a
 * question on both. Inputs: the newest page and the older questions. Output: the questions. */
export function mergedQuestions(first: QuestionPage, older: readonly OwnerQuestion[]): OwnerQuestion[] {
  const seen = new Set(first.items.map((item) => item.id));
  return orderQuestions([...first.items, ...older.filter((item) => !seen.has(item.id))]);
}

/** The older pages the owner opened below the newest page, and where the next one starts. */
export interface OlderPages {
  items: OwnerQuestion[];
  next: number | null;
}

/** One change to the older pages. */
export type OlderEvent = { kind: "first"; next: number | null } | { kind: "older"; page: QuestionPage } | { kind: "settled" };

/** The older pages after one event: a newest-page read sets where older pages start while
 * none is open; an older-page read adds its questions; an answer or dismissal drops the older
 * pages, which may now show a settled question as waiting, so they are read again from the
 * newest page. Inputs: the older pages and the event. Output: the older pages. */
export function olderAfter(older: OlderPages, event: OlderEvent): OlderPages {
  switch (event.kind) {
    case "first": return older.items.length === 0 ? { items: [], next: event.next } : older;
    case "older": return { items: [...older.items, ...event.page.items], next: event.page.next };
    case "settled": return { items: [], next: null };
  }
}

/** The one way the owner settles a question, wherever its card shows: Answer sends the
 * choice, Dismiss asks first, and either tells the owner what the asker hears or why it was
 * refused. Inputs: the project data and what to do after each settle, such as reading the
 * list again. Output: whether an action runs, the refusal line, the handlers and the dialog. */
export function useQuestionActions(data: ProjectData, after: () => void) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [dismissing, setDismissing] = useState<OwnerQuestion | null>(null);

  async function settle(result: Result<void>, kind: "answered" | "dismissed", question: OwnerQuestion) {
    setBusy(false);
    setDismissing(null);
    if (result.kind === "ok") {
      setNote("");
      data.notify(settledNote(kind, question.asker));
    } else setNote(notReady(result) ? "Questions aren't ready in this engine yet." : refusalOf(result));
    after();
  }

  async function answer(question: OwnerQuestion, choice: QuestionChoice) {
    setBusy(true);
    await settle(await answerQuestion(question.id, question.version, choice), "answered", question);
  }

  async function dismiss(question: OwnerQuestion) {
    setBusy(true);
    await settle(await dismissQuestion(question.id, question.version), "dismissed", question);
  }

  const dialog = <ConfirmDialog open={dismissing !== null} pending={busy} title="Dismiss this question?" confirmLabel="Dismiss" cancelLabel="Keep it"
    onCancel={() => setDismissing(null)} onConfirm={() => { if (dismissing) void dismiss(dismissing); }}>
    {dismissing ? dismissNote(dismissing.asker) : ""}
  </ConfirmDialog>;
  return { busy, setBusy, note, setNote, answer: (question: OwnerQuestion, choice: QuestionChoice) => void answer(question, choice), askDismiss: setDismissing, dialog };
}

/** The waiting questions one "Dismiss all" sends: the pending ones shown, oldest first, at
 * most `BULK_DISMISS_MAX`. Input: the shown questions. Output: the questions. */
export function dismissAllOf(questions: readonly OwnerQuestion[]): OwnerQuestion[] {
  const waiting = questions.filter((question) => question.status === "pending").slice(0, BULK_DISMISS_MAX);
  assert(waiting.length <= BULK_DISMISS_MAX, "one Dismiss all sends at most one page");
  return waiting;
}

/** Questions tab: every question the project's agents and engine steps asked the owner,
 * pending first, with Answer and Dismiss, and Dismiss all while two or more wait. Input: the
 * project data. Output: the tab; the list rereads every few seconds while it is open. */
export function QuestionsTab({ data }: { data: ProjectData }) {
  const [first, setFirst] = useState<Result<QuestionPage>>();
  const [older, setOlder] = useState<OlderPages>({ items: [], next: null });
  const [round, setRound] = useState(0);
  const [dismissingAll, setDismissingAll] = useState<OwnerQuestion[] | null>(null);
  const reread = () => {
    setOlder((shown) => olderAfter(shown, { kind: "settled" }));
    setRound((value) => value + 1);
  };
  const { busy, setBusy, note, setNote, answer, askDismiss, dialog } = useQuestionActions(data, reread);
  useEffect(() => {
    let current = true;
    void loadQuestions(data.id, null).then((result) => {
      if (!current) return;
      setFirst(result);
      if (result.kind === "ok") setOlder((shown) => olderAfter(shown, { kind: "first", next: result.value.next }));
    });
    const timer = setInterval(() => setRound((value) => value + 1), LIST_POLL_MS);
    return () => { current = false; clearInterval(timer); };
  }, [data.id, round]);

  async function dismissAll(waiting: OwnerQuestion[]) {
    setBusy(true);
    const result = await dismissQuestions(waiting);
    setBusy(false);
    setDismissingAll(null);
    if (result.kind === "ok") {
      setNote("");
      data.notify(bulkDismissedLine(result.value));
    } else setNote(notReady(result) ? "Questions aren't ready in this engine yet." : refusalOf(result));
    reread();
  }

  async function showOlder() {
    if (older.next === null) return;
    setBusy(true);
    const result = await loadQuestions(data.id, older.next);
    setBusy(false);
    if (result.kind !== "ok") return setNote(refusalOf(result));
    setOlder((shown) => olderAfter(shown, { kind: "older", page: result.value }));
  }

  if (first === undefined) return <p className="pj-muted pj-small" role="status">Loading questions...</p>;
  if (notReady(first)) return <NotReady what="questions from agents aren't in this engine yet." />;
  const questions = first.kind === "ok" ? mergedQuestions(first.value, older.items) : [];
  const waiting = dismissAllOf(questions);
  return <div className="pj-stack">
    {first.kind === "error" && <p className="pj-error" role="alert">Couldn't load questions. {refusalOf(first)}</p>}
    {note && <p className="pj-note warn" role="alert">{note}</p>}
    {first.kind === "ok" && questions.length === 0 && <ProjectEmpty title="No questions yet">
      When the master, a sub-agent or a CrazyAgent step needs a decision from you, its question waits here.
    </ProjectEmpty>}
    {waiting.length >= 2 && <div className="pj-row between">
      <span className="pj-small pj-secondary">{`${waiting.length} questions wait for you.`}</span>
      <Button variant="quiet" disabled={busy} onClick={() => setDismissingAll(waiting)}>Dismiss all</Button>
    </div>}
    {questions.length > 0 && <div className="pj-stack-s qs-list">
      {questions.map((question) => <QuestionCard key={`${question.id}:${question.version}`} question={question} now={data.now} busy={busy}
        onAnswer={answer} onDismiss={askDismiss} />)}
    </div>}
    {older.next !== null && <span><Button disabled={busy} onClick={() => void showOlder()}>Show older questions</Button></span>}
    {dialog}
    <ConfirmDialog open={dismissingAll !== null} pending={busy} title={`Dismiss ${dismissingAll?.length ?? 0} questions?`} confirmLabel="Dismiss all" cancelLabel="Keep them"
      onCancel={() => setDismissingAll(null)} onConfirm={() => { if (dismissingAll) void dismissAll(dismissingAll); }}>
      {dismissingAll ? bulkDismissNote(dismissingAll.length) : ""}
    </ConfirmDialog>
  </div>;
}
