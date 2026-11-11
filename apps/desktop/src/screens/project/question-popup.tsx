import { useEffect, useState } from "react";
import { QUESTIONS_SHOWN_CHOICES, loadQuestions, orderQuestions } from "../../api-questions";
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

const plural = (count: number) => `${count} ${count === 1 ? "question" : "questions"} waiting`;

/** The questions pop-up docked on a project tab: open, the first waiting question as the
 * Questions tab card with Hide and a way to the Questions tab; folded, one button counting
 * them; nothing when none waits. Inputs: the waiting questions, whether it is open, now, the
 * running action and its refusal line, and the handlers. Output: the pop-up or nothing. */
export function QuestionPopupView({ waiting, open, now, busy, note, onAnswer, onDismiss, onHide, onShow, onOpenTab }: {
  waiting: readonly OwnerQuestion[];
  open: boolean;
  now: number;
  busy: boolean;
  note: string;
  onAnswer: (question: OwnerQuestion, choice: QuestionChoice) => void;
  onDismiss: (question: OwnerQuestion) => void;
  onHide: () => void;
  onShow: () => void;
  onOpenTab: () => void;
}) {
  if (waiting.length === 0) return null;
  if (!open) {
    return <div className="qp-dock">
      <Button size="small" icon="inbox" aria-expanded={false} onClick={onShow}>{plural(waiting.length)}</Button>
    </div>;
  }
  const first = waiting[0];
  return <section className="qp-dock qp-popup" aria-label="Questions waiting for you">
    <div className="pj-row between">
      <strong>{waiting.length === 1 ? "A question for you" : `Question 1 of ${waiting.length}`}</strong>
      <span className="pj-row">
        <Button size="small" variant="quiet" onClick={onOpenTab}>Questions tab</Button>
        <Button size="small" variant="quiet" aria-expanded={true} onClick={onHide}>Hide</Button>
      </span>
    </div>
    {note && <p className="pj-note warn" role="alert">{note}</p>}
    <QuestionCard key={`${first.id}:${first.version}`} question={first} now={now} busy={busy} onAnswer={onAnswer} onDismiss={onDismiss} />
  </section>;
}

/** The questions pop-up of one project tab: reads the project's questions every few seconds
 * and settles them with the Questions tab's own actions, so an answer here closes the
 * question there too. Input: the project data. Output: the pop-up. */
export function QuestionPopup({ data }: { data: ProjectData }) {
  const [questions, setQuestions] = useState<OwnerQuestion[]>([]);
  const [round, setRound] = useState(0);
  const [hiddenUpTo, setHiddenUpTo] = useState(0);
  const { busy, note, answer, askDismiss, dialog } = useQuestionActions(data, () => setRound((value) => value + 1));
  useEffect(() => {
    let current = true;
    void loadQuestions(data.id, null).then((result) => {
      if (current && result.kind === "ok") setQuestions(result.value.items);
    });
    const timer = setInterval(() => setRound((value) => value + 1), LIST_POLL_MS);
    return () => { current = false; clearInterval(timer); };
  }, [data.id, round]);
  const waiting = popupQuestions(questions);
  const newest = waiting.reduce((most, item) => Math.max(most, item.id), 0);
  return <>
    <QuestionPopupView waiting={waiting} open={popupOpen(waiting, hiddenUpTo)} now={data.now} busy={busy} note={note}
      onAnswer={answer} onDismiss={askDismiss} onHide={() => setHiddenUpTo(newest)} onShow={() => setHiddenUpTo(0)}
      onOpenTab={() => data.navigate(`/projects/${encodeURIComponent(data.id)}/questions`)} />
    {dialog}
  </>;
}

/** The Questions show setting of Settings, Profile, with every option. Inputs: the choice
 * and what to do with a new one. Output: the setting line. */
export function QuestionsShownSetting({ value, onValue }: { value: QuestionsShown; onValue: (value: QuestionsShown) => void }) {
  return <SettingLine label="Questions show" help="Where a question from the master, a sub-agent or a CrazyAgent step shows while you are on a project: a pop-up on the Conversation tab, a pop-up on every project tab, or only the Questions tab. The Questions tab always lists it, and answering in either place answers it in both. Kept in this app only.">
    <Segmented label="Questions show" value={value} onValue={onValue} options={QUESTIONS_SHOWN_CHOICES} />
  </SettingLine>;
}
