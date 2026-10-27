import { useEffect, useId, useState } from "react";
import type { Failure, Result } from "../../api";
import { ANSWER_BYTES_MAX, answerQuestion, askedInTerminal, askerLabel, dismissNote, dismissQuestion, isRecommended, loadQuestions, notReady, orderQuestions, questionsRefusal, settledNote, statusBadge, typedAnswer } from "../../api-questions";
import type { OwnerQuestion, QuestionChoice, QuestionPage } from "../../api-questions";
import { Button, ConfirmDialog, TextArea } from "../../ui";
import { displayText } from "../conversation";
import { ago } from "./research";
import { Badge, NotReady, ProjectEmpty } from "./shared";
import type { ProjectData } from "./shared";
import "./questions.css";

const LIST_POLL_MS = 5000;

function refusalOf(failure: Failure): string {
  return failure.kind === "error" ? questionsRefusal({ code: failure.code, cause: failure.cause }) : "";
}

/** The owner's pick on one pending question: an option by position or typed text, never
 * both. Inputs: the picked option or null and the typed text. Output: the choice to send,
 * or null while there is nothing to send. */
export function choiceOf(picked: number | null, text: string): QuestionChoice | null {
  if (picked !== null) return { option: picked };
  const answer = typedAnswer(text);
  return answer === null ? null : { text: answer };
}

/** One question: who asked and when, the question, its context and, while it waits, the
 * options with the recommended one marked, a field to type an answer, Answer and Dismiss; a
 * question asked in an agent's terminal is answered there, so it offers only Dismiss. A
 * settled question shows its status and answer. Inputs: the question, now, whether an
 * action runs and the handlers. Output: the card. */
export function QuestionCard({ question, now, busy, onAnswer, onDismiss }: {
  question: OwnerQuestion;
  now: number;
  busy: boolean;
  onAnswer: (question: OwnerQuestion, choice: QuestionChoice) => void;
  onDismiss: (question: OwnerQuestion) => void;
}) {
  const [picked, setPicked] = useState<number | null>(null);
  const [text, setText] = useState("");
  const name = useId();
  const field = useId();
  const pending = question.status === "pending";
  const terminal = askedInTerminal(question);
  const choice = choiceOf(picked, text);
  const tooLong = text.trim().length > 0 && typedAnswer(text) === null;
  const badge = statusBadge(question.status);
  const suggested = question.options.length === 0 ? question.recommended : null;
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
      {question.options.length > 0 && <fieldset className="qs-options">
        <legend className="pj-small pj-secondary">Pick one</legend>
        {question.options.map((option, index) => <label className="qs-option" key={`${index}:${option}`}>
          <input type="radio" name={name} checked={picked === index} disabled={busy}
            onChange={() => { setPicked(index); setText(""); }} />
          <span>{displayText(option)}</span>
          {isRecommended(question, index) && <span className="qs-recommended pj-small">Recommended</span>}
        </label>)}
      </fieldset>}
      {suggested && <div className="pj-row">
        <span className="pj-small pj-secondary">{`Suggested: ${displayText(suggested)}`}</span>
        <Button size="small" disabled={busy} onClick={() => { setPicked(null); setText(suggested); }}>Use suggested answer</Button>
      </div>}
      <TextArea id={field} label={question.options.length > 0 ? "Or write your own answer" : "Your answer"} value={text} rows={3}
        onValue={(value) => { setText(value); if (value.length > 0) setPicked(null); }}
        error={tooLong ? `Write at most ${ANSWER_BYTES_MAX} bytes.` : undefined} />
      <div className="pj-row">
        <Button variant="primary" disabled={busy || choice === null} onClick={() => { if (choice !== null) onAnswer(question, choice); }}>Answer</Button>
        <Button variant="quiet" disabled={busy} onClick={() => onDismiss(question)}>Dismiss</Button>
      </div>
    </>}
    {!pending && question.answer !== null && <p className="qs-answer"><span className="pj-secondary">Answer: </span>{displayText(question.answer)}</p>}
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

/** Questions tab: every question the project's agents and engine steps asked the owner,
 * pending first, with Answer and Dismiss. Input: the project data. Output: the tab; the list
 * rereads every few seconds while it is open. */
export function QuestionsTab({ data }: { data: ProjectData }) {
  const [first, setFirst] = useState<Result<QuestionPage>>();
  const [older, setOlder] = useState<OlderPages>({ items: [], next: null });
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [dismissing, setDismissing] = useState<OwnerQuestion | null>(null);
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

  async function settle(result: Result<void>, kind: "answered" | "dismissed", question: OwnerQuestion) {
    setBusy(false);
    setDismissing(null);
    if (result.kind === "ok") {
      setNote("");
      data.notify(settledNote(kind, question.asker));
    } else setNote(notReady(result) ? "Questions aren't ready in this engine yet." : refusalOf(result));
    setOlder((shown) => olderAfter(shown, { kind: "settled" }));
    setRound((value) => value + 1);
  }

  async function answer(question: OwnerQuestion, choice: QuestionChoice) {
    setBusy(true);
    await settle(await answerQuestion(question.id, question.version, choice), "answered", question);
  }

  async function dismiss(question: OwnerQuestion) {
    setBusy(true);
    await settle(await dismissQuestion(question.id, question.version), "dismissed", question);
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
  return <div className="pj-stack">
    {first.kind === "error" && <p className="pj-error" role="alert">Couldn't load questions. {refusalOf(first)}</p>}
    {note && <p className="pj-note warn" role="alert">{note}</p>}
    {first.kind === "ok" && questions.length === 0 && <ProjectEmpty title="No questions yet">
      When the master, a sub-agent or a CrazyAgent step needs a decision from you, its question waits here.
    </ProjectEmpty>}
    {questions.length > 0 && <div className="pj-stack-s qs-list">
      {questions.map((question) => <QuestionCard key={`${question.id}:${question.version}`} question={question} now={data.now} busy={busy}
        onAnswer={(asked, choice) => void answer(asked, choice)} onDismiss={setDismissing} />)}
    </div>}
    {older.next !== null && <span><Button disabled={busy} onClick={() => void showOlder()}>Show older questions</Button></span>}
    <ConfirmDialog open={dismissing !== null} pending={busy} title="Dismiss this question?" confirmLabel="Dismiss" cancelLabel="Keep it"
      onCancel={() => setDismissing(null)} onConfirm={() => { if (dismissing) void dismiss(dismissing); }}>
      {dismissing ? dismissNote(dismissing.asker) : ""}
    </ConfirmDialog>
  </div>;
}
