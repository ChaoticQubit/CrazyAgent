import { nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState } from "react";
import { formatRoute } from "../../ui";
import { cancelMasterTurn, endMaster, failureCopy, openMasterProject, readMaster, readMasterTurns, receiptState, sendToMaster, startMaster } from "../../api-projects";
import type { IntentReceipt, MasterState, MasterTurn } from "../../engine-client";
import { harnessName } from "../new-project/team-editor";
import { displayText } from "../conversation";
import { poolName } from "../new-project/wizard";
import { useTeamGraph } from "../team";
import { Console, type ConsoleLine } from "./console";
import { agentMentions } from "./console-model";
import { useProjectFiles } from "./project-files";
import { MESSAGE_BYTES_MAX, NEW_CONVERSATION_NOTE, accountLines, changesLine, delegating, engineLine, masterMissingLine, masterStatus, openedTicket, replyingTurn, resendText, restartForSettings, sendFailureLine, taskLines, thinkingLine, turnLine, turnSettled } from "./research";
import { Badge, NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { Menu, type Tone } from "../../ui";
import { SessionsBar, ownerText, withOwnerLines } from "./sessions";

const READ_ONLY = "Read-only: the master runs live in its terminal now.";
const sentByProject = new Map<string, Record<string, string>>();
const sentAtByProject = new Map<string, Record<string, number>>();

const tones: Record<string, Tone> = { Ready: "neutral", "Thinking...": "info", Replied: "ok", "Not signed in": "warn", Stopped: "neutral", "Needs a pool": "warn" };

function unwrap<T>(result: Result<T>): T {
  if (result.kind === "ok") return result.value;
  throw result.kind === "not_ready" ? { code: "not_ready" } : { code: result.code, cause: result.cause };
}

/** Reads the code and engine cause out of a thrown failure.
 * Input: anything a master action threw. Output: its code, "unknown" when it has none, and its
 * string cause when present. */
function thrown(failure: unknown): { code: string; cause?: string } {
  const code = typeof failure === "object" && failure !== null && "code" in failure ? String(failure.code) : "unknown";
  const cause = typeof failure === "object" && failure !== null && "cause" in failure && typeof failure.cause === "string" ? failure.cause : undefined;
  return cause === undefined ? { code } : { code, cause };
}

function confirm(receipt: IntentReceipt) {
  const state = receiptState(receipt);
  if (state.kind === "rejected") throw { code: state.code };
  if (state.kind === "unconfirmed") throw { code: "master_outcome_pending" };
}

/** The console lines of the master conversation: each turn's opening (the owner's message as
 * this window sent it or as read back from the engine, the engine's line for a turn it asked
 * for, or a placeholder for a message whose text is not read yet), the reply or the turn's
 * status line, its task and account lines, then the messages this window sent that no turn
 * shows yet. Inputs: the turns, the texts this window sent by turn id, when each was sent,
 * the current time and the owner lines read back by input cursor. Output: the lines in
 * order, each with an id that stays the same while it is shown. */
export function consoleLines(turns: readonly MasterTurn[], sent: Record<string, string>, sentAt: Record<string, number> | undefined, now: number, held: Record<string, string> = {}): ConsoleLine[] {
  const lines: ConsoleLine[] = [];
  for (const turn of turns) {
    const engine = engineLine(turn);
    const id = `${turn.id}-owner`;
    const owner = ownerText(turn, sent, held);
    lines.push(owner !== undefined ? { id, kind: "owner", text: owner }
      : { id, kind: "engine", text: engine ?? `Your message ${turn.ordinal} (its text stays in the window that sent it)` });
    if (turn.reply) lines.push({ id: `${turn.id}-reply`, kind: "reply", text: turn.reply.text });
    else {
      const failed = turnSettled(turn) && turn.phase !== "succeeded";
      lines.push({ id: `${turn.id}-status`, kind: failed ? "error" : "engine", text: turn.phase === "running" ? thinkingLine(sentAt?.[turn.id], now) : turnLine(turn) });
    }
    taskLines(turn).forEach((text, index) => lines.push({ id: `${turn.id}-task-${index}`, kind: "delegate", text }));
    const changes = changesLine(turn);
    if (changes !== null) lines.push({ id: `${turn.id}-changes`, kind: "tool", text: changes });
    accountLines(turn).forEach((text, index) => lines.push({ id: `${turn.id}-account-${index}`, kind: "engine", text }));
  }
  for (const [id, text] of Object.entries(sent)) {
    if (turns.some((turn) => turn.id === id)) continue;
    lines.push({ id: `${id}-owner`, kind: "owner", text }, { id: `${id}-status`, kind: "engine", text: thinkingLine(sentAt?.[id], now) });
  }
  return lines;
}

/** Conversation tab: the project's master conversation as a terminal-style console with `/`
 * commands and `@` tags for the team's agents. Inputs: the project data and whether the
 * conversation is only read, as the one from before a live master. Output: the console and,
 * unless read-only, the conversation's actions; it polls every two seconds while a reply is
 * still coming. */
export function ConversationTab({ data, readOnly = false }: { data: ProjectData; readOnly?: boolean }) {
  const [ticket, setTicket] = useState("");
  const [master, setMaster] = useState<MasterState>();
  const [turns, setTurns] = useState<MasterTurn[]>([]);
  const [sent, setSentState] = useState<Record<string, string>>(() => sentByProject.get(data.id) ?? {});
  const [held, setHeld] = useState<Record<string, string>>({});
  const [now, setNow] = useState(() => Date.now());
  const [working, setWorking] = useState(false);
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const [needsPool, setNeedsPool] = useState(false);
  const [notReady, setNotReady] = useState(false);
  const current = useRef(data.id);
  current.current = data.id;
  const [graph] = useTeamGraph(data.id);
  const [tag, setTag] = useState<string | null>(null);
  const files = useProjectFiles(data.id, readOnly ? null : tag);
  const setup = valueOf(data.setup);
  const harnesses = valueOf(data.harnesses);
  const session = master?.ticket === ticket ? master.session : null;
  const active = session?.phase === "configured";
  const waiting = turns.some((turn) => !turnSettled(turn));
  const unseen = Object.keys(sent).some((id) => !turns.some((turn) => turn.id === id));
  const handedOff = delegating(turns);
  const noPool = needsPool || (data.pools?.kind === "ok" && data.assigned === null);
  const status = noPool ? "Needs a pool" : masterStatus(session, turns);

  const setSent = (next: Record<string, string>) => {
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
      const read = unwrap(await readMasterTurns(useTicket, state.session.session_id));
      if (current.current === live) setTurns(read);
      const owned = await withOwnerLines(useTicket, state.session.session_id, read, sent, held);
      if (current.current === live) setHeld((known) => ({ ...known, ...owned }));
    } else setTurns([]);
  }

  function report(failure: unknown, describe: (failure: { code: string; cause?: string }) => string = failureCopy) {
    const named = thrown(failure);
    if (named.code === "not_ready") setNotReady(true);
    else if (named.code === "master_open_unavailable") setNeedsPool(true);
    else setProblem(describe(named));
  }

  async function act(action: () => Promise<void>, describe: (failure: { code: string; cause?: string }) => string = failureCopy) {
    if (working) return;
    setWorking(true);
    setProblem("");
    try {
      await action();
    } catch (failure) {
      report(failure, describe);
    }
    setWorking(false);
  }

  useEffect(() => {
    setMaster(undefined);
    setTurns([]);
    setHeld({});
    setSentState(sentByProject.get(data.id) ?? {});
    setProblem("");
    setNeedsPool(false);
    const live = data.id;
    void act(async () => {
      const opened = openedTicket(unwrap(await openMasterProject(live)), live);
      if (current.current !== live) return;
      setTicket(opened);
      await refresh(opened);
    });
  }, [data.id]);

  useEffect(() => {
    if (!waiting && !unseen && !handedOff) return;
    const timer = window.setInterval(() => {
      setNow(Date.now());
      void refresh().catch((failure) => report(failure));
    }, 2000);
    return () => window.clearInterval(timer);
  }, [waiting, unseen, handedOff, ticket, session?.session_id]);

  async function begin() {
    confirm(unwrap(await startMaster(ticket, nextGeneration())));
    setNeedsPool(false);
    const state = unwrap(await readMaster(ticket));
    setMaster(state);
    if (state.session?.phase !== "configured") throw { code: "master_outcome_pending" };
    setTurns([]);
    return state.session;
  }

  const send = (text: string) => act(async () => {
    const message = text.trim();
    const at = Date.now();
    if (!message || !ticket) return;
    setNote("");
    let fresh = !active;
    let live = active && session ? session : await begin();
    let receipt = unwrap(await sendToMaster(ticket, live.session_id, message, nextGeneration()));
    if (restartForSettings(receipt)) {
      confirm(unwrap(await endMaster(ticket, live.session_id, nextGeneration())));
      live = await begin();
      fresh = true;
      receipt = unwrap(await sendToMaster(ticket, live.session_id, message, nextGeneration()));
      if (receiptState(receipt).kind === "applied") setNote(NEW_CONVERSATION_NOTE);
    }
    if (receiptState(receipt).kind === "rejected") await refresh().catch(() => undefined);
    confirm(receipt);
    sentAtByProject.set(data.id, { ...(fresh ? {} : sentAtByProject.get(data.id)), [receipt.operation_id]: at });
    setSent({ ...(fresh ? {} : sent), [receipt.operation_id]: message });
    await refresh();
  }, sendFailureLine);

  async function stopReply() {
    const turn = replyingTurn(turns);
    if (!turn || !session) return;
    const result = await cancelMasterTurn(ticket, session.session_id, turn.id, nextGeneration());
    if (result.kind === "not_ready") return data.notify("Not ready yet: stopping a reply isn't in this build.");
    if (result.kind === "error") return setProblem(failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind === "rejected") return setProblem(failureCopy(state));
    await refresh().catch((failure) => report(failure));
  }

  const end = () => act(async () => {
    if (!session) return;
    confirm(unwrap(await endMaster(ticket, session.session_id, nextGeneration())));
    await refresh();
  });

  const blocked = working || !ticket || waiting || notReady;
  const again = resendText(turns, sent);
  const masterLine = setup ? `Master: ${harnessName(harnesses, setup.team.master)}, Default model, Tools ${setup.team.master_tools ? "on" : "off"}` : "Master";
  const settings = `/projects/${encodeURIComponent(data.id)}/settings`;
  const missing = setup ? masterMissingLine(harnesses, setup.team.master) : null;
  const told = consoleLines(turns, sent, sentAtByProject.get(data.id), now, held);
  const lines: ConsoleLine[] = told.length > 0 ? told : [{ id: "empty", kind: "engine", text: readOnly
    ? "No earlier conversation. Messages sent to the master before it ran live stay here to read."
    : `No messages yet. Ask the master anything about ${data.name || "this project"}. It has read the brief, documents and research.` }];
  const agents = agentMentions(graph.shown?.kind === "ok" ? graph.shown.value.nodes : []);
  const offer = (text: string) => {
    const bytes = new TextEncoder().encode(text).length;
    if (bytes > MESSAGE_BYTES_MAX) return setProblem(`Too long: ${bytes} of ${MESSAGE_BYTES_MAX} bytes. Press Up to get it back and shorten it.`);
    void send(text);
  };

  return <div className="pj-convo">
    {!readOnly && <div className="pj-convo-head">
      <span>{masterLine}</span>
      <span className="pj-row"><Badge word={status} tone={tones[status] ?? "neutral"} />
        <Menu label="Conversation actions" items={[
          ...(active && !working ? [{ label: "End conversation", icon: "circle-stop" as const, onSelect: () => void end() }] : []),
          { label: "Change master", icon: "settings", onSelect: () => data.navigate(`${settings}?section=team`) },
        ]} /></span>
    </div>}
    {!readOnly && <SessionsBar ticket={ticket} liveId={active && session ? session.session_id : null} onSwitched={() => { setSent({}); return refresh(); }} />}
    {notReady && <NotReady what="the master conversation isn't reachable in this build." />}
    {!readOnly && <>
      {missing && <div className="pj-note warn" role="alert"><p>{displayText(missing)}</p>
        <button type="button" className="btn" onClick={() => data.navigate(`${settings}?section=team`)}>Change master</button></div>}
      {noPool && <div className="pj-note warn" role="alert"><p>Pick a pool to start. The master replies once this project has one.</p>
        <button type="button" className="btn" onClick={() => data.navigate(`${settings}?section=pool`)}>Choose pool</button></div>}
      {status === "Not signed in" && <div className="pj-note warn"><p>The master's accounts{data.assigned ? ` in ${displayText(poolName(data.assigned.pool))}` : ""} aren't signed in.</p>
        <button type="button" className="btn" onClick={() => data.navigate("/settings/accounts")}>Open accounts</button></div>}
      {note && <p className="pj-small pj-muted" role="status">{note}</p>}
      {problem && <p className="pj-error" role="alert">{problem}</p>}
      {again !== null && <div className="pj-row"><button type="button" className="btn" disabled={blocked} onClick={() => void send(again)}>Send again</button></div>}
    </>}
    <Console projectId={data.id} lines={lines} agents={agents} files={files} onTagQuery={setTag}
      canSend={!readOnly && !blocked} status={readOnly ? READ_ONLY : waiting ? "Thinking..." : undefined}
      onSend={offer} onStop={() => void stopReply()} onNavigate={(route) => data.navigate(formatRoute(route).slice(1))} />
  </div>;
}
