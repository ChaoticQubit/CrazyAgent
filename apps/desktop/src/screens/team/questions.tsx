import { useEffect, useId, useRef, useState, type Ref } from "react";
import { answerQuestion, loadQuestions } from "../../api";
import type { Result } from "../../api";
import type { GraphNode, OwnerQuestion, QuestionSeverity, QuestionsInbox, WaitingAgent } from "../../engine-client";
import { Button, LoadError, NotReady, Notice, Skeleton, StatusBadge, TextArea, type Tone } from "../../ui";
import { displayText } from "../conversation";
import { changeNote } from "./controls";

export const QUESTIONS_READ_MS = 5_000;
export const ANSWER_BYTES_MAX = 4000;

const URGENCY: Record<QuestionSeverity, { word: string; tone: Tone; note: string }> = {
  high: { word: "Urgent", tone: "danger", note: "The agent is paused until you answer." },
  normal: { word: "Waiting", tone: "warn", note: "The agent waits for your answer." },
  low: { word: "Carrying on", tone: "info", note: "The agent carries on with its assumption. Answer to overrule it." },
};

/** The badge word, tone and note for one question's urgency (Q14b). Input: the severity.
 * Output: the copy. */
export function urgency(severity: QuestionSeverity): { word: string; tone: Tone; note: string } {
  return URGENCY[severity];
}

/** The terminal panel node for the agent behind one inbox row, made from the inbox's own read:
 * its agent, terminal name and label. Input: the row. Output: the node, or null while the
 * agent has no terminal. */
export function terminalNode(row: Pick<OwnerQuestion | WaitingAgent, "agent" | "label" | "terminal">): GraphNode | null {
  if (row.terminal === null) return null;
  return {
    id: row.terminal, kind: "sub", parent: null, label: row.label, harness: "", status: null,
    status_label: null, account: null, terminal: row.terminal, agent: row.agent,
  };
}

/** Checks an answer with the engine's bounds before it is sent: not blank and at most 4000
 * bytes of UTF-8. Input: the text. Output: the plain reason, or null when it can be sent. */
export function answerProblem(text: string): string | null {
  if (text.trim() === "") return "Write an answer first.";
  if (new TextEncoder().encode(text.trim()).length > ANSWER_BYTES_MAX) return `Keep the answer within ${ANSWER_BYTES_MAX} bytes.`;
  return null;
}

/** What the inbox shows: the read it draws, and why the latest read failed while an older
 * inbox stays on screen. */
export type InboxState = { shown: Result<QuestionsInbox> | undefined; stale: string | null };

/** Takes one inbox read into the view. A failed read after a good one keeps the good inbox,
 * so an answer being written stays, and says why the refresh failed. Inputs: the current
 * state and the read. Output: the next state. */
export function inboxRead(state: InboxState, result: Result<QuestionsInbox>): InboxState {
  if (result.kind === "ok" || state.shown?.kind !== "ok") return { shown: result, stale: null };
  return { shown: state.shown, stale: result.kind === "error" ? result.message : "Questions aren't ready in this build." };
}

/** Reads one project's Questions inbox, again every 5 s. Input: the project id. Output: the
 * inbox state and a reload action that keeps the inbox on screen while it reads. */
export function useQuestions(projectId: string): [InboxState, () => void] {
  const [state, setState] = useState<InboxState>({ shown: undefined, stale: null });
  const [round, setRound] = useState(0);
  useEffect(() => setState({ shown: undefined, stale: null }), [projectId]);
  useEffect(() => {
    let current = true;
    let timer = 0;
    const read = async () => {
      const result = await loadQuestions(projectId);
      if (!current) return;
      setState((previous) => inboxRead(previous, result));
      if (result.kind === "not_ready") return;
      timer = window.setTimeout(read, QUESTIONS_READ_MS);
    };
    void read();
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [projectId, round]);
  return [state, () => setRound((value) => value + 1)];
}

function OpenTerminal({ row, onOpen }: { row: Pick<OwnerQuestion | WaitingAgent, "agent" | "label" | "terminal">; onOpen: (node: GraphNode) => void }) {
  const node = terminalNode(row);
  if (!node) return <span className="pj-muted pj-small">Not running</span>;
  return <Button size="small" variant="quiet" icon="square-terminal" onClick={() => onOpen(node)}>Open terminal</Button>;
}

/** One question in the owner's inbox: its urgency, who asked, whom the asker meant it for
 * (For: Me, or the master's or lead's name), the question, its assumption, Open terminal and
 * Answer. Takes the project, the question, the open and answered callbacks and, for the row
 * the drawer opens on, a ref that makes it take the focus. Returns the row. */
export function QuestionRow({ projectId, question, onOpen, onAnswered, focusRef }: {
  projectId: string;
  question: OwnerQuestion;
  onOpen: (node: GraphNode) => void;
  onAnswered: () => void;
  focusRef?: Ref<HTMLLIElement>;
}) {
  const [text, setText] = useState("");
  const [writing, setWriting] = useState(false);
  const [sending, setSending] = useState(false);
  const [note, setNote] = useState("");
  const ids = useId();
  const copy = urgency(question.severity);
  const label = displayText(question.label);

  async function send() {
    const problem = answerProblem(text);
    if (problem) {
      setNote(problem);
      return;
    }
    setSending(true);
    setNote("");
    const result = await answerQuestion(projectId, question.id, text.trim());
    setSending(false);
    const failed = changeNote(result, "answering questions");
    if (failed) {
      setNote(failed);
      return;
    }
    setText("");
    setWriting(false);
    onAnswered();
  }

  return <li ref={focusRef} className={`tg-question ${question.severity}`} tabIndex={focusRef ? -1 : undefined} data-autofocus={focusRef ? "" : undefined}>
    <div className="pj-row">
      <StatusBadge word={copy.word} tone={copy.tone} />
      <strong>{label}</strong>
      <span className="pj-secondary pj-small">For: {displayText(question.for_label)}</span>
      <span className="pj-secondary pj-small grow">{copy.note}</span>
      <OpenTerminal row={question} onOpen={onOpen} />
    </div>
    <p className="pj-pre">{displayText(question.text)}</p>
    {question.assumption !== null && <p className="pj-secondary"><strong>Assumes:</strong> {displayText(question.assumption)}</p>}
    {question.assumption === null && question.severity === "low" && <p className="pj-muted pj-small">It stated no assumption.</p>}
    {writing
      ? <form className="tg-answer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
        <TextArea id={`${ids}-answer`} label={`Your answer to ${label}`} value={text} onValue={setText} rows={3}
          maxLength={ANSWER_BYTES_MAX} counterAt={ANSWER_BYTES_MAX - 500} error={note || undefined} />
        <div className="pj-row end">
          <Button variant="quiet" disabled={sending} onClick={() => { setWriting(false); setNote(""); }}>Cancel</Button>
          <Button variant="primary" type="submit" icon="send" pending={sending} pendingLabel="Sending...">Send answer</Button>
        </div>
      </form>
      : <div className="pj-row end"><Button icon="message-square" onClick={() => setWriting(true)}>Answer</Button></div>}
  </li>;
}

/** How many things wait on the owner in one inbox read: open questions, the ones the read
 * left out, and agents waiting in their terminal. Input: the inbox. Output: the count. */
export function askCount(inbox: QuestionsInbox): number {
  const count = inbox.questions.length + inbox.more + inbox.waiting.length;
  if (!Number.isSafeInteger(count) || count < 0) throw new Error(`an inbox counts whole questions: ${count}`);
  return count;
}

/** The Agents toolbar's words for what waits on the owner, as the mockup words it: "1 asks
 * you", "3 ask you", or "no one asks you". Input: the count. Output: the words. */
export function asksWords(count: number): string {
  if (!Number.isSafeInteger(count) || count < 0) throw new Error(`asks count a whole number: ${count}`);
  return count === 0 ? "no one asks you" : `${count} ${count === 1 ? "asks" : "ask"} you`;
}

/** The title of the drawer that holds the owner's questions: "Questions for you", with the
 * open questions counted once there are any. Input: the inbox read. Output: the title. */
export function questionsTitle(read: InboxState): string {
  const count = read.shown?.kind === "ok" ? read.shown.value.questions.length + read.shown.value.more : 0;
  return count > 0 ? `Questions for you (${count})` : "Questions for you";
}

/** The owner's Questions inbox, in the drawer the Agents toolbar opens (Q14, Q14b): the open
 * questions agents put to the owner, most urgent first, each with its urgency, the assumption
 * a low one carries on with, Answer and Open terminal while the asker runs; then the agents
 * waiting on the owner in their own terminal, each with Open terminal, which opens the agent
 * in the Agents tab's one terminal panel. The drawer's title names it. Opened from a toast,
 * the keyboard starts on the first question (most urgent first) or the first agent waiting in
 * its terminal, once that row is read. That happens once: a drawer that opens with the row
 * already read starts on it through data-autofocus, and later reads leave the keyboard where
 * the owner put it. Inputs: the project id, the inbox read the toolbar counts from with its
 * reload, the open action and the row kind to focus. Output: the section. */
export function QuestionsPanel({ projectId, read, reload, onOpen, focus = null }: {
  projectId: string;
  read: InboxState;
  reload: () => void;
  onOpen: (node: GraphNode) => void;
  focus?: "questions" | "waiting" | null;
}) {
  const target = useRef<HTMLLIElement>(null);
  const focused = useRef(false);
  useEffect(() => {
    if (focus === null || focused.current || target.current === null) return;
    focused.current = true;
    const drawer = target.current.closest("dialog");
    if (drawer === null || drawer.open) target.current.focus();
  });
  const shown = read.shown;
  if (shown === undefined) return <section className="tg-events" aria-busy="true" aria-label="Questions for you"><Skeleton shape="card" /></section>;
  if (shown.kind === "not_ready") return <NotReady what="Questions for you" compact />;
  if (shown.kind === "error") return <LoadError message={`Couldn't load your questions. ${shown.message}`} onRetry={reload} />;
  const inbox = shown.value;
  const count = inbox.questions.length + inbox.more;
  return <section className="tg-events" aria-label="Questions for you">
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh your questions. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    {count === 0 && inbox.waiting.length === 0 && <p className="tg-terminal-note">No agent is waiting on you.</p>}
    {inbox.questions.length > 0 && <ul className="pj-rows" aria-label="Open questions">
      {inbox.questions.map((question, index) => <QuestionRow key={question.id} projectId={projectId} question={question} onOpen={onOpen} onAnswered={reload}
        focusRef={focus === "questions" && index === 0 ? target : undefined} />)}
    </ul>}
    {inbox.more > 0 && <p className="pj-secondary pj-small">
      {inbox.more} more {inbox.more === 1 ? "question waits" : "questions wait"}. They show here as you answer these.
    </p>}
    {inbox.waiting.length > 0 && <>
      <h4>Waiting on you in their terminal</h4>
      <ul className="pj-rows" aria-label="Waiting on you in their terminal">{inbox.waiting.map((agent, index) => <li key={agent.agent} className="pj-row"
        ref={focus === "waiting" && index === 0 ? target : undefined} tabIndex={focus === "waiting" && index === 0 ? -1 : undefined}
        data-autofocus={focus === "waiting" && index === 0 ? "" : undefined}>
        <StatusBadge word="Waiting on you" tone="warn" />
        <strong>{displayText(agent.label)}</strong>
        <span className="grow">{agent.waiting === "permission" ? "A permission prompt waits in its terminal." : "A question waits in its terminal."}</span>
        <OpenTerminal row={agent} onOpen={onOpen} />
      </li>)}</ul>
    </>}
  </section>;
}
