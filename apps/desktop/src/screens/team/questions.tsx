import { useEffect, useId, useState } from "react";
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
 * Answer. Takes the project, the question and the open and answered callbacks. Returns the
 * row. */
export function QuestionRow({ projectId, question, onOpen, onAnswered }: {
  projectId: string;
  question: OwnerQuestion;
  onOpen: (node: GraphNode) => void;
  onAnswered: () => void;
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

  return <li className={`tg-question ${question.severity}`}>
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

/** The owner's Questions inbox on the Agents tab (Q14, Q14b): the open questions agents put
 * to the owner, most urgent first, each with its urgency, the assumption a low one carries on
 * with, Answer and Open terminal while the asker runs; then the agents waiting on the owner in
 * their own terminal, each with Open terminal, which opens the agent in the Agents tab's one
 * terminal panel. Reads again every 5 s and after each answer. Inputs: the project id and the
 * open action. Output: the section. */
export function QuestionsPanel({ projectId, onOpen }: { projectId: string; onOpen: (node: GraphNode) => void }) {
  const [read, reload] = useQuestions(projectId);
  const ids = useId();
  const shown = read.shown;
  if (shown === undefined) return <section className="tg-events" aria-busy="true" aria-label="Questions for you"><Skeleton shape="card" /></section>;
  if (shown.kind === "not_ready") return <NotReady what="Questions for you" compact />;
  if (shown.kind === "error") return <LoadError message={`Couldn't load your questions. ${shown.message}`} onRetry={reload} />;
  const inbox = shown.value;
  const count = inbox.questions.length + inbox.more;
  return <section className="tg-events" aria-labelledby={`${ids}-title`}>
    <h3 id={`${ids}-title`}>Questions for you{count > 0 ? ` (${count})` : ""}</h3>
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh your questions. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    {count === 0 && inbox.waiting.length === 0 && <p className="tg-terminal-note">No agent is waiting on you.</p>}
    {inbox.questions.length > 0 && <ul className="pj-rows" aria-label="Open questions">
      {inbox.questions.map((question) => <QuestionRow key={question.id} projectId={projectId} question={question} onOpen={onOpen} onAnswered={reload} />)}
    </ul>}
    {inbox.more > 0 && <p className="pj-secondary pj-small">
      {inbox.more} more {inbox.more === 1 ? "question waits" : "questions wait"}. They show here as you answer these.
    </p>}
    {inbox.waiting.length > 0 && <>
      <h4>Waiting on you in their terminal</h4>
      <ul className="pj-rows" aria-label="Waiting on you in their terminal">{inbox.waiting.map((agent) => <li key={agent.agent} className="pj-row">
        <StatusBadge word="Waiting on you" tone="warn" />
        <strong>{displayText(agent.label)}</strong>
        <span className="grow">{agent.waiting === "permission" ? "A permission prompt waits in its terminal." : "A question waits in its terminal."}</span>
        <OpenTerminal row={agent} onOpen={onOpen} />
      </li>)}</ul>
    </>}
  </section>;
}
