import { useEffect, useState } from "react";
import { cursorOf, entryKey, entryText, kindWord, loadAgentMessages, messagesProblem, resultLine, speakerName, withNewest, withOlder } from "../../api-messages";
import type { Timeline } from "../../api-messages";
import { agentOf } from "../../api-process";
import type { AgentMessage, GraphNode, ResultsShown } from "../../engine-client";
import { displayText } from "../conversation";
import { useLinkOpener } from "../project/links";
import { MarkdownView } from "../project/markdown";
import { localClock } from "../project/research";
import { eventState } from "./wake";
import "./agent-messages.css";

function Entry({ entry, name, results, onOpenLink }: { entry: AgentMessage; name: string; results: ResultsShown; onOpenLink: (href: string) => void }) {
  const [open, setOpen] = useState(false);
  const what = entry.kind === "failed" ? "problem" : "result";
  return <li className="am-row" data-from={entry.from}>
    <span className="am-head">
      <span className="am-time">{localClock(entry.at_ms)}</span>
      <span className="am-speaker">{displayText(speakerName(entry.from, name))}</span>
      <span className="am-kind">{kindWord(entry.kind)}</span>
      {entry.state !== null && <span className="am-state">{eventState(entry.state)}</span>}
    </span>
    <span className="am-text">{displayText(entryText(entry))}</span>
    {entry.result !== null && results === "full" && <ResultView name={name} what={what} source={entry.result} plain onOpenLink={onOpenLink} />}
    {entry.result !== null && results === "first_line" && <>
      <span className="am-result">{displayText(resultLine(entry.result))}</span>
      <span><button type="button" className="btn quiet small" aria-expanded={open} onClick={() => setOpen(!open)}>{open ? `Hide ${what}` : `Show ${what}`}</button></span>
      {open && <ResultView name={name} what={what} source={entry.result} onOpenLink={onOpenLink} />}
    </>}
    {entry.result_more && <span className="am-note">Shortened here. The whole {what} is in the Tasks list.</span>}
  </li>;
}

/** A result or problem in a frame named for the agent, so what it holds, a rule, a bold
 * "Master" or a line laid out like a row, never reads as another author's row. Inputs: the
 * agent's name, whether it is a result or a problem, its text, whether to show it as plain
 * text instead of Markdown, and the link handler. Output: the framed result. */
export function ResultView({ name, what, source, plain = false, onOpenLink }: { name: string; what: "result" | "problem"; source: string; plain?: boolean; onOpenLink: (href: string) => void }) {
  return <figure className="am-md">
    <figcaption className="am-from">{`${what === "problem" ? "Problem" : "Result"} from ${displayText(name)}`}</figcaption>
    {plain
      ? <span className="am-result am-full">{displayText(source)}</span>
      : <div className="pj-md"><MarkdownView source={source} onOpenLink={onOpenLink} /></div>}
  </figure>;
}

/** One sub-agent's Messages timeline, newest first: each entry's time, who it comes from by the
 * engine's field, what it is and its text as plain text; a task result shows its first line
 * until opened, or all of it, as the project's setting says. Load older reads the next page.
 * Inputs: the agent's name, the timeline or null while the first read runs, a failure line,
 * whether Load older runs, the Load older action and the link handler. Output: the section. */
export function AgentMessagesView({ name, timeline, problem, busy, onOlder, onOpenLink }: {
  name: string;
  timeline: Timeline | null;
  problem: string;
  busy: boolean;
  onOlder: () => void;
  onOpenLink: (href: string) => void;
}) {
  return <section className="am" aria-label={`Messages with ${displayText(name)}`}>
    <h3 className="am-title">Messages</h3>
    {timeline === null && !problem && <p className="am-note" role="status">Reading this agent's messages...</p>}
    {problem && <p className="am-note warn" role="alert">{problem}</p>}
    {timeline !== null && timeline.entries.length === 0 && <p className="am-note">No messages yet. Its tasks, what the master and you tell it, and its questions and results show here.</p>}
    {timeline !== null && timeline.entries.length > 0 && <ol className="am-rows" aria-label="Messages, newest first">
      {timeline.entries.map((entry) => <Entry key={entryKey(entry)} entry={entry} name={name} results={timeline.settings.results} onOpenLink={onOpenLink} />)}
    </ol>}
    {timeline?.older === true && <span><button type="button" className="btn small" disabled={busy} onClick={onOlder}>{busy ? "Loading..." : "Load older"}</button></span>}
  </section>;
}

/** Reads and shows one sub-agent's Messages: the newest page now and again whenever `refresh`
 * changes, which the caller ties to its team read while the agent is live, keeping the older
 * pages Load older read. Inputs: the project id, the agent's team id, its name, the refresh
 * value and an optional notify for link notes. Output: the timeline view. */
export function AgentMessages({ projectId, agent, name, refresh, notify }: {
  projectId: string;
  agent: number;
  name: string;
  refresh: unknown;
  notify?: (message: string) => void;
}) {
  const [timeline, setTimeline] = useState<Timeline | null>(null);
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const openLink = useLinkOpener(notify ?? setNote);

  useEffect(() => {
    setTimeline(null);
    setProblem("");
  }, [projectId, agent]);

  useEffect(() => {
    let live = true;
    void loadAgentMessages(projectId, agent, null).then((result) => {
      if (!live) return;
      if (result.kind !== "ok") return setProblem(messagesProblem(result));
      setTimeline((shown) => withNewest(shown, result.value));
      setProblem("");
    });
    return () => { live = false; };
  }, [projectId, agent, refresh]);

  async function older() {
    const last = timeline?.entries.at(-1);
    if (last === undefined || busy) return;
    setBusy(true);
    const result = await loadAgentMessages(projectId, agent, cursorOf(last));
    setBusy(false);
    if (result.kind !== "ok") return setProblem(messagesProblem(result));
    setTimeline((shown) => shown === null ? shown : withOlder(shown, result.value));
  }

  return <>
    <AgentMessagesView name={name} timeline={timeline} problem={problem} busy={busy} onOlder={() => void older()} onOpenLink={openLink} />
    {note && <p className="am-note" role="status">{note}</p>}
  </>;
}

/** The Messages of the open graph card's sub-agent, beside its instructions. Inputs: the
 * project id, the card's node and the refresh value, the graph read while it is live. Output:
 * the timeline, or nothing for the master's card or a card without an agent. */
export function NodeMessages({ projectId, node, refresh }: { projectId: string; node: GraphNode; refresh: unknown }) {
  const agent = agentOf(node);
  if (agent === null || agent === undefined) return null;
  return <AgentMessages projectId={projectId} agent={agent} name={node.label} refresh={refresh} />;
}
