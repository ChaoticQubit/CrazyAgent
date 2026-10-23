import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState } from "react";
import { endMaster, failureCopy, openMasterProject, readMaster, readMasterTurns, receiptState, sendToMaster, startMaster } from "../../api-projects";
import type { IntentReceipt, MasterState, MasterTurn } from "../../engine-client";
import { harnessName } from "../new-project/team-editor";
import { displayText } from "../conversation";
import { MarkdownView } from "./markdown";
import { masterStatus, nearBottom, resendText, sendFailureLine, thinkingLine, turnLine, turnSettled } from "./research";
import { Badge, NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import type { Tone } from "../../ui";
import { useLinkOpener } from "./links";

const sentByProject = new Map<string, Record<number, string>>();
const sentAtByProject = new Map<string, Record<number, number>>();
const MESSAGE_BYTES_MAX = 8192;

const tones: Record<string, Tone> = { Ready: "neutral", "Thinking...": "info", Replied: "ok", "Not signed in": "warn", Stopped: "neutral", "Needs a pool": "warn" };

function unwrap<T>(result: Result<T>): T {
  if (result.kind === "ok") return result.value;
  throw result.kind === "not_ready" ? { code: "not_ready" } : { code: result.code };
}

function confirm(receipt: IntentReceipt) {
  const state = receiptState(receipt);
  if (state.kind === "rejected") throw { code: state.code };
  if (state.kind === "unconfirmed") throw { code: "master_outcome_pending" };
}

/** Conversation tab: the project's master conversation with replies as safe Markdown.
 * Input: the project data. Output: the thread and composer; it polls every two seconds
 * while a reply is still coming. */
export function ConversationTab({ data }: { data: ProjectData }) {
  const [ticket, setTicket] = useState("");
  const [master, setMaster] = useState<MasterState>();
  const [turns, setTurns] = useState<MasterTurn[]>([]);
  const [sent, setSentState] = useState<Record<number, string>>(() => sentByProject.get(data.id) ?? {});
  const [text, setText] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const [working, setWorking] = useState(false);
  const [problem, setProblem] = useState("");
  const [needsPool, setNeedsPool] = useState(false);
  const [notReady, setNotReady] = useState(false);
  const current = useRef(data.id);
  current.current = data.id;
  const thread = useRef<HTMLDivElement>(null);
  const open = useLinkOpener(data.notify);
  const setup = valueOf(data.setup);
  const harnesses = valueOf(data.harnesses);
  const session = master?.ticket === ticket ? master.session : null;
  const active = session?.phase === "configured";
  const waiting = turns.some((turn) => !turnSettled(turn));
  const unseen = Object.keys(sent).some((ordinal) => !turns.some((turn) => turn.ordinal === Number(ordinal)));
  const status = needsPool ? "Needs a pool" : masterStatus(session, turns);

  const setSent = (next: Record<number, string>) => {
    sentByProject.set(data.id, next);
    setSentState(next);
  };

  async function refresh(useTicket = ticket) {
    if (!useTicket) return;
    const live = data.id;
    const state = unwrap(await readMaster(useTicket));
    if (current.current !== live) return;
    setMaster(state);
    if (state.session) {
      const page = unwrap(await readMasterTurns(useTicket, state.session.session_id));
      if (current.current === live) setTurns([...page.page.turns].sort((a, b) => a.ordinal - b.ordinal));
    } else setTurns([]);
  }

  async function act(action: () => Promise<void>, describe: (code: string) => string = failureCopy) {
    if (working) return;
    setWorking(true);
    setProblem("");
    try {
      await action();
    } catch (failure) {
      const code = typeof failure === "object" && failure !== null && "code" in failure ? String(failure.code) : "unknown";
      if (code === "not_ready") setNotReady(true);
      else if (code === "master_open_unavailable") setNeedsPool(true);
      else setProblem(describe(code));
    }
    setWorking(false);
  }

  useEffect(() => {
    setMaster(undefined);
    setTurns([]);
    setSentState(sentByProject.get(data.id) ?? {});
    setProblem("");
    setNeedsPool(false);
    const live = data.id;
    void act(async () => {
      const opened = unwrap(await openMasterProject(live));
      if (current.current !== live) return;
      setTicket(opened.ticket);
      await refresh(opened.ticket);
    });
  }, [data.id]);

  useEffect(() => {
    if (!waiting && !unseen) return;
    const timer = window.setInterval(() => {
      setNow(Date.now());
      void refresh().catch(() => setProblem(failureCopy("owner_read_failed")));
    }, 2000);
    return () => window.clearInterval(timer);
  }, [waiting, unseen, ticket, session?.session_id]);

  useEffect(() => {
    const element = thread.current;
    if (element && nearBottom(element)) element.scrollTop = element.scrollHeight;
  }, [turns.length, Object.keys(sent).length]);

  const send = (again?: string) => act(async () => {
    const message = (again ?? text).trim();
    const at = Date.now();
    if (!message || !ticket) return;
    let live = session;
    if (!active) {
      confirm(unwrap(await startMaster(ticket, nextGeneration())));
      setNeedsPool(false);
      const state = unwrap(await readMaster(ticket));
      setMaster(state);
      live = state.session;
      if (live?.phase !== "configured") throw { code: "master_outcome_pending" };
      setTurns([]);
    }
    const ordinal = (active ? turns : []).reduce((most, turn) => Math.max(most, turn.ordinal), 0) + 1;
    confirm(unwrap(await sendToMaster(ticket, live!.session_id, message, nextGeneration())));
    sentAtByProject.set(data.id, { ...(active ? sentAtByProject.get(data.id) : {}), [ordinal]: at });
    setSent({ ...(active ? sent : {}), [ordinal]: message });
    if (again === undefined) setText("");
    await refresh();
  }, sendFailureLine);

  const end = () => act(async () => {
    if (!session) return;
    confirm(unwrap(await endMaster(ticket, session.session_id, nextGeneration())));
    await refresh();
  });

  const bytes = new TextEncoder().encode(text).length;
  const blocked = working || !ticket || waiting || notReady;
  const again = resendText(turns, sent);
  const latest = turns.reduce((most, turn) => Math.max(most, turn.ordinal), 0);
  const masterLine = setup ? `Master: ${harnessName(harnesses, setup.team.master)}, Default model, Tools ${setup.team.master_tools ? "on" : "off"}` : "Master";
  const settings = `/projects/${encodeURIComponent(data.id)}/settings`;

  return <div className="pj-convo">
    <div className="pj-convo-head">
      <span>{masterLine}</span>
      <span className="pj-row"><Badge word={status} tone={tones[status] ?? "neutral"} />
        {active && <button type="button" className="btn" disabled={working} onClick={() => void end()}>End conversation</button>}
        <button type="button" className="btn quiet" onClick={() => data.navigate(`${settings}?section=team`)}>Change master</button></span>
    </div>
    {notReady && <NotReady what="the master conversation isn't reachable in this build." />}
    <div className="pj-thread" ref={thread} aria-live="polite">
      {turns.length === 0 && Object.keys(sent).length === 0 && <p className="pj-muted">Ask the master anything about {displayText(data.name || "this project")}. It has read the brief, documents and research.</p>}
      {turns.map((turn) => <div key={turn.id} className="pj-turn" data-testid="master-turn">
        {sent[turn.ordinal] !== undefined
          ? <div className="pj-msg-owner"><p>{displayText(sent[turn.ordinal])}</p></div>
          : <p className="pj-small pj-muted">Your message {turn.ordinal} (its text stays in the window that sent it)</p>}
        {turn.reply ? <div className="pj-msg-master" data-testid="master-reply"><MarkdownView source={turn.reply.text} onOpenLink={open} /></div>
          : <p className={`pj-small ${turnSettled(turn) && turn.phase !== "succeeded" ? "pj-warn-text" : "pj-muted"}`}>{turn.phase === "running" ? thinkingLine(sentAtByProject.get(data.id)?.[turn.ordinal], now) : turnLine(turn)}</p>}
        {again !== null && turn.ordinal === latest && <button type="button" className="btn" disabled={blocked} onClick={() => void send(again)}>Send again</button>}
      </div>)}
      {Object.entries(sent).filter(([ordinal]) => !turns.some((turn) => turn.ordinal === Number(ordinal))).map(([ordinal, message]) =>
        <div key={`pending-${ordinal}`} className="pj-turn"><div className="pj-msg-owner"><p>{displayText(message)}</p></div><p className="pj-small pj-muted">{thinkingLine(sentAtByProject.get(data.id)?.[Number(ordinal)], now)}</p></div>)}
    </div>
    {needsPool && <div className="pj-note warn" role="alert"><p>This project needs a pool before the master can reply.</p>
      <button type="button" className="btn" onClick={() => data.navigate(`${settings}?section=pool`)}>Choose pool</button></div>}
    {status === "Not signed in" && <div className="pj-note warn"><p>The master's accounts{data.assigned ? ` in ${data.assigned.pool.id}` : ""} aren't signed in.</p>
      <button type="button" className="btn" onClick={() => data.navigate("/settings/accounts")}>Open accounts</button></div>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <form className="pj-composer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
      <label htmlFor="pj-composer" className="pj-sr">Message the master</label>
      <textarea className="pj-input" id="pj-composer" rows={3} value={text} aria-describedby="pj-composer-help" data-testid="master-message"
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => { if (event.key === "Enter" && event.metaKey) { event.preventDefault(); void send(); } }} />
      <div className="pj-row between">
        <span className="pj-help" id="pj-composer-help">{bytes > MESSAGE_BYTES_MAX ? `Too long: ${bytes} of ${MESSAGE_BYTES_MAX} bytes.` : "Cmd+Enter to send"}</span>
        <button type="submit" className="btn primary" disabled={blocked || !text.trim() || bytes > MESSAGE_BYTES_MAX} data-testid="send-master-message">{waiting ? "Thinking..." : "Send"}</button>
      </div>
    </form>
  </div>;
}
