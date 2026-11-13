import { useEffect, useId, useRef, useState, type FocusEvent } from "react";
import { QUESTIONS_SHOWN_CHOICES, askerLabel, loadQuestions, orderQuestions } from "../../api-questions";
import type { OwnerQuestion, QuestionChoice, QuestionsShown } from "../../api-questions";
import { Button, Segmented, SettingLine } from "../../ui";
import { LIST_POLL_MS, QuestionCard, useQuestionActions } from "./questions";
import type { ProjectData } from "./shared";
import "./questions.css";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Whether a project tab shows the questions pop-up under the owner's Questions show choice.
 * The Questions tab never does, since it lists every question itself. Inputs: the choice and
 * the tab shown. Output: true when the pop-up shows there. */
export function popupOn(shown: QuestionsShown, tab: string): boolean {
  if (tab === "questions") return false;
  return shown === "every" || (shown === "conversation" && tab === "conversation");
}

/** The questions the pop-up offers: the waiting ones, the one waiting longest first.
 * Input: the questions read. Output: a new list. */
export function popupQuestions(questions: readonly OwnerQuestion[]): OwnerQuestion[] {
  const waiting = orderQuestions(questions.filter((item) => item.status === "pending"));
  assert(waiting.every((item) => item.status === "pending"), "the pop-up offers only waiting questions");
  return waiting;
}

/** Whether the pop-up is open: some question waits that is newer than the newest one the
 * owner hid. Inputs: the waiting questions and the newest id hidden, 0 for none. Output: the
 * answer. */
export function popupOpen(waiting: readonly OwnerQuestion[], hiddenUpTo: number): boolean {
  assert(Number.isSafeInteger(hiddenUpTo) && hiddenUpTo >= 0, "the hidden mark is a question id or 0");
  return waiting.some((item) => item.id > hiddenUpTo);
}

/** The line a screen reader hears, politely, when the waiting questions change: how many
 * wait and who asked the newest. Input: the waiting questions. Output: the line, empty when
 * none waits. */
export function dockAnnouncement(waiting: readonly OwnerQuestion[]): string {
  if (waiting.length === 0) return "";
  const newest = waiting.reduce((most, item) => (item.id > most.id ? item : most));
  const count = waiting.length === 1 ? "A question waits" : `${waiting.length} questions wait`;
  return `${count} for you. The newest is from ${askerLabel(newest.asker, newest.asker_name)}.`;
}

const plural = (count: number) => `${count} ${count === 1 ? "question" : "questions"} waiting`;

/** The questions pop-up of a project page, in the page's flow above the tab, so it never
 * covers a terminal prompt, a message box or a toast. Open, it shows the first waiting question
 * as the Questions tab card with Hide and a way to the Questions tab; Hide folds it to one
 * button until a newer question waits. It stays mounted across tabs, so Hide and a half-typed
 * answer survive a tab switch. After an answer or dismissal the keyboard goes to the next
 * card's title, or back to where the owner was when none is left. Inputs: the waiting
 * questions, whether this tab shows it, now, the running action, its refusal line, a count
 * of settles and the handlers. Output: the pop-up. */
export function QuestionDock({ waiting, shown, now, busy, note, settled, onAnswer, onDismiss, onOpenTab }: {
  waiting: readonly OwnerQuestion[];
  shown: boolean;
  now: number;
  busy: boolean;
  note: string;
  settled: number;
  onAnswer: (question: OwnerQuestion, choice: QuestionChoice) => void;
  onDismiss: (question: OwnerQuestion) => void;
  onOpenTab: () => void;
}) {
  const [hiddenUpTo, setHiddenUpTo] = useState(0);
  const card = useId();
  const returnTo = useRef<HTMLElement | null>(null);
  const owned = useRef(false);
  const moved = useRef<"show" | "title" | null>(null);
  const open = popupOpen(waiting, hiddenUpTo);
  const first = waiting[0];
  const firstKey = first ? `${first.id}:${first.version}` : "";
  const newest = waiting.reduce((most, item) => Math.max(most, item.id), 0);

  const focusNext = () => {
    const title = document.getElementById(`${card}-title`);
    if (title && open) title.focus();
    else if (returnTo.current?.isConnected) returnTo.current.focus();
  };
  useEffect(() => {
    if (!owned.current) return;
    if (note) owned.current = false;
    focusNext();
  }, [settled]);
  useEffect(() => {
    if (!owned.current || busy) return;
    owned.current = false;
    focusNext();
  }, [firstKey, waiting.length]);
  useEffect(() => {
    const target = moved.current === "show" ? document.getElementById(`${card}-show`) : moved.current === "title" ? document.getElementById(`${card}-title`) : null;
    moved.current = null;
    target?.focus();
  }, [open]);

  const entered = (event: FocusEvent<HTMLDivElement>) => {
    const from = event.relatedTarget;
    if (from instanceof HTMLElement && !event.currentTarget.contains(from) && !from.closest("dialog")) returnTo.current = from;
  };
  const answer = (question: OwnerQuestion, choice: QuestionChoice) => { owned.current = true; onAnswer(question, choice); };
  const dismiss = (question: OwnerQuestion) => { owned.current = true; onDismiss(question); };

  return <div className="qp-dock" hidden={!shown} onFocus={entered}>
    <p className="visually-hidden" role="status">{shown ? dockAnnouncement(waiting) : ""}</p>
    {first && !open && <span>
      <Button id={`${card}-show`} size="small" icon="inbox" aria-expanded={false} aria-controls={card}
        onClick={() => { moved.current = "title"; setHiddenUpTo(0); }}>{plural(waiting.length)}</Button>
    </span>}
    {first && <section id={card} className="qp-popup" aria-labelledby={`${card}-title`} hidden={!open}>
      <div className="pj-row between">
        <h2 id={`${card}-title`} className="qp-title" tabIndex={-1}>{waiting.length === 1 ? "A question for you" : `Question 1 of ${waiting.length}`}</h2>
        <span className="pj-row">
          <Button size="small" variant="quiet" onClick={onOpenTab}>Questions tab</Button>
          <Button size="small" variant="quiet" aria-expanded={open} aria-controls={card}
            onClick={() => { moved.current = "show"; setHiddenUpTo(newest); }}>Hide</Button>
        </span>
      </div>
      {note && <p className="pj-note warn" role="alert">{note}</p>}
      <QuestionCard key={firstKey} question={first} now={now} busy={busy} onAnswer={answer} onDismiss={dismiss} />
    </section>}
  </div>;
}

/** The questions pop-up of one project page: reads the project's questions every few seconds
 * and settles them with the Questions tab's own actions, so an answer here closes the
 * question there too. Inputs: the project data and whether the tab shown shows it. Output:
 * the pop-up. */
export function QuestionPopup({ data, shown }: { data: ProjectData; shown: boolean }) {
  const [questions, setQuestions] = useState<OwnerQuestion[]>([]);
  const [round, setRound] = useState(0);
  const [settled, setSettled] = useState(0);
  const { busy, note, answer, askDismiss, dialog } = useQuestionActions(data, () => {
    setSettled((value) => value + 1);
    setRound((value) => value + 1);
  });
  useEffect(() => {
    let current = true;
    void loadQuestions(data.id, null).then((result) => {
      if (current && result.kind === "ok") setQuestions(result.value.items);
    });
    const timer = setInterval(() => setRound((value) => value + 1), LIST_POLL_MS);
    return () => { current = false; clearInterval(timer); };
  }, [data.id, round]);
  return <>
    <QuestionDock waiting={popupQuestions(questions)} shown={shown} now={data.now} busy={busy} note={note} settled={settled}
      onAnswer={answer} onDismiss={askDismiss} onOpenTab={() => data.navigate(`/projects/${encodeURIComponent(data.id)}/questions`)} />
    {dialog}
  </>;
}

/** The Questions show setting of Settings, Profile, with every option. Inputs: the choice
 * and what to do with a new one. Output: the setting line. */
export function QuestionsShownSetting({ value, onValue }: { value: QuestionsShown; onValue: (value: QuestionsShown) => void }) {
  return <SettingLine label="Questions show" help="Where a question from the master, a sub-agent or a CrazyAgent step shows while you are on a project: a pop-up on the Conversation tab, a pop-up on every project tab, or only the Questions tab. The pop-up sits above the tab and never covers the terminal or the message box. The Questions tab always lists it, and answering in either place answers it in both. Kept in this app only.">
    <Segmented label="Questions show" value={value} onValue={onValue} options={QUESTIONS_SHOWN_CHOICES} />
  </SettingLine>;
}
