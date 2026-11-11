import { listAccounts, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useLoad } from "../../state";
import { createContext, useContext, useEffect, useId, useRef, useState, type ReactNode } from "react";
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
import { Button, Drawer, Menu, type Tone } from "../../ui";
import { ACCESS_UNREADABLE, LiveTerminalSetting, MasterIdleSetting, OnSwitchSetting, ReaskSetting, SettingGroup, SettingRow, type SettingGroupInfo } from "../team/controls";
import { DeliverySetting, QuestionsSetting, coordination } from "../team/coordination";
import { AccountSetting, MasterHarnessSetting, useTeam } from "../team/setup-rows";
import { ApplySwitch } from "./settings";
import { SessionsBar, ownerText, switchDivider, withOwnerLines } from "./sessions";

const READ_ONLY = "Read-only: the master runs live in its terminal now.";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** What the Master settings button takes from the drawer above it: the action that opens the
 * drawer and the id the button wears so closing the drawer can find it. A button with no
 * drawer above it fails when clicked. */
const MasterSettingsHost = createContext<{ open: () => void; button?: string }>({
  open: () => assert(false, "the Master settings button needs MasterSettingsDrawer above it"),
});
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

export const QUEUED_LINE = "Opening the conversation. Your message sends once the master is ready.";
export const QUEUE_FAILED = "Not sent: the conversation didn't open. Press Up to get your message back.";

/** What Enter does with the owner's message now: send it, queue it while the conversation is
 * still opening, or hold it in the prompt while the master can't take one (read only, not in
 * this build, a reply still coming, a message already queued, a send in flight, or a
 * conversation that failed to open). Input: the conversation's state. Output: the decision. */
export function sendGate(state: { readOnly: boolean; ticket: string; openFailed: boolean; working: boolean; waiting: boolean; notReady: boolean; queued: boolean }): "send" | "queue" | "hold" {
  if (state.readOnly || state.notReady || state.waiting || state.queued) return "hold";
  if (!state.ticket) return state.openFailed ? "hold" : "queue";
  return state.working ? "hold" : "send";
}

/** The console lines of a message queued while the conversation opens: the owner's words and
 * a line saying it sends once the master is ready, or that it was not sent when the opening
 * failed. Inputs: the queued text, if any, and whether the opening failed. Output: the lines. */
export function queuedLines(queued: string | null, openFailed: boolean): ConsoleLine[] {
  if (queued === null) return [];
  return [
    { id: "queued-owner", kind: "owner", text: queued },
    openFailed ? { id: "queued-status", kind: "error", text: QUEUE_FAILED } : { id: "queued-status", kind: "engine", text: QUEUED_LINE },
  ];
}

/** The console lines of the master conversation: each turn's opening (the owner's message as
 * this window sent it or as read back from the engine, the engine's line for a turn it asked
 * for, or a placeholder for a message whose text is not read yet), the reply or the turn's
 * status line, its task and account lines, then the messages this window sent that no turn
 * shows yet. Inputs: the turns, the texts this window sent by turn id, when each was sent,
 * the current time, the owner lines read back by input cursor and the owner's account names
 * for the account lines. Output: the lines in order, each with an id that stays the same
 * while it is shown. */
export function consoleLines(turns: readonly MasterTurn[], sent: Record<string, string>, sentAt: Record<string, number> | undefined, now: number, held: Record<string, string> = {},
  accounts: { id: string; name: string }[] = []): ConsoleLine[] {
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
    accountLines(turn, accounts).forEach((text, index) => lines.push({ id: `${turn.id}-account-${index}`, kind: "engine", text }));
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
  const [openFailed, setOpenFailed] = useState(false);
  const [queued, setQueued] = useState<string | null>(null);
  const current = useRef(data.id);
  current.current = data.id;
  const [graph] = useTeamGraph(data.id);
  const [accounts] = useLoad(listAccounts, [], "accounts");
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
    setTicket("");
    setMaster(undefined);
    setTurns([]);
    setHeld({});
    setSentState(sentByProject.get(data.id) ?? {});
    setProblem("");
    setNeedsPool(false);
    setOpenFailed(false);
    setQueued(null);
    const live = data.id;
    void act(async () => {
      try {
        const opened = openedTicket(unwrap(await openMasterProject(live)), live);
        if (current.current !== live) return;
        setTicket(opened);
        await refresh(opened);
      } catch (failure) {
        if (current.current === live) setOpenFailed(true);
        throw failure;
      }
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
  const told = [...consoleLines(turns, sent, sentAtByProject.get(data.id), now, held, accounts.kind === "ok" ? accounts.value : []), ...queuedLines(queued, openFailed && !ticket)];
  const switched = readOnly ? switchDivider(data.team.result) : null;
  const lines: ConsoleLine[] = [...(told.length > 0 ? told : [{ id: "empty", kind: "engine" as const, text: readOnly
    ? "No earlier conversation. Messages sent to the master before it ran live stay here to read."
    : `No messages yet. Ask the master anything about ${data.name || "this project"}. It has read the brief, documents and research.` }]),
  ...(switched ? [{ id: "harness-switch", kind: "engine" as const, text: switched }] : [])];
  const agents = agentMentions(graph.shown?.kind === "ok" ? graph.shown.value.nodes : []);
  const gate = sendGate({ readOnly, ticket, openFailed, working, waiting, notReady, queued: queued !== null });
  const offer = (text: string) => {
    const bytes = new TextEncoder().encode(text).length;
    if (bytes > MESSAGE_BYTES_MAX) return setProblem(`Too long: ${bytes} of ${MESSAGE_BYTES_MAX} bytes. Press Up to get it back and shorten it.`);
    if (gate === "queue") return setQueued(text);
    void send(text);
  };

  useEffect(() => {
    if (queued === null || !ticket || working || waiting || notReady) return;
    setQueued(null);
    void send(queued);
  }, [queued, ticket, working, waiting, notReady]);

  return <div className="pj-convo">
    {!readOnly && <div className="pj-convo-head">
      <span>{masterLine}</span>
      <span className="pj-row"><Badge word={status} tone={tones[status] ?? "neutral"} />
        <MasterSettingsButton />
        <Menu label="Conversation actions" items={[
          ...(active && !working ? [{ label: "End conversation", icon: "circle-stop" as const, onSelect: () => void end() }] : []),
          { label: "Change master", icon: "settings", onSelect: () => data.navigate(`${settings}?section=team`) },
        ]} /></span>
    </div>}
    {!readOnly && <SessionsBar ticket={ticket} liveId={active && session ? session.session_id : null} mark={`${turns.length}:${turns.filter(turnSettled).length}`} onSwitched={() => { setSent({}); return refresh(); }} />}
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
      canSend={gate !== "hold"} status={readOnly ? READ_ONLY : waiting ? "Thinking..." : undefined}
      onSend={offer} onStop={() => void stopReply()} onNavigate={(route) => data.navigate(formatRoute(route).slice(1))} />
  </div>;
}

const MASTER_GROUPS: readonly SettingGroupInfo[] = [
  { id: "master", title: "Master", icon: "brain", rows: ["Harness", "Account", "Live terminal"] },
  { id: "conversation", title: "Conversation", icon: "message-square", rows: ["On master harness switch", "Stop the master when idle after", "Apply a master harness change"] },
  { id: "messages", title: "Messages", icon: "send", rows: ["Sub-agent questions go to", "Messages to Claude Code agents", "Re-ask a sub-agent's empty reply"] },
  { id: "access", title: "Access", icon: "shield-check", rows: ["Full access"] },
];

/** The master's settings in the Agent settings layout, grouped as the approved mockup's drawer
 * shows them: Master (its harness and account, picked in place and saved as project Settings
 * saves them), Conversation, Messages and Access (Full access, kept in Agent settings). The
 * shared rows save exactly as they do in Agent settings. Inputs: the project data, whether its
 * master runs in a live terminal and what to do before a button leaves the drawer for another
 * page. Output: the groups, or the team read's state. */
export function MasterSettings({ data, liveHarness, onLeave = () => {} }: { data: ProjectData; liveHarness: boolean; onLeave?: () => void }) {
  const harnessRows = useTeam(data);
  const team = data.team.result;
  if (team === undefined) return <p className="pj-muted" role="status">Loading...</p>;
  if (team.kind === "not_ready") return <NotReady what="the live team isn't in this build." />;
  if (team.kind === "error") return <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(team)}</p>;
  const status = team.value;
  const claude = coordination(status).harnesses.find((row) => row.harness === "claude-code");
  const base = `/projects/${encodeURIComponent(data.id)}`;
  const prefix = `master-set-${data.id}`;
  const go = (path: string) => { onLeave(); data.navigate(path); };
  const [masterGroup, talk, messages, access] = MASTER_GROUPS;
  return <div className="set-groups">
    <SettingGroup group={masterGroup} prefix={prefix} level={3}>
      <MasterHarnessSetting rows={harnessRows} data={data} label="Harness" id={`${prefix}-harness`} helper="A change reaches the running master as Apply a master harness change says." />
      <AccountSetting data={data} id={`${prefix}-account`} />
      <LiveTerminalSetting projectId={data.id} status={status} reload={data.reloadTeam} liveHarness={liveHarness} />
    </SettingGroup>
    <SettingGroup group={talk} prefix={prefix} level={3}>
      <OnSwitchSetting projectId={data.id} status={status} reload={data.reloadTeam} />
      <MasterIdleSetting projectId={data.id} status={status} reload={data.reloadTeam} />
      <SettingRow label="Apply a master harness change"><ApplySwitch data={data} /></SettingRow>
    </SettingGroup>
    <SettingGroup group={messages} prefix={prefix} level={3}>
      <QuestionsSetting projectId={data.id} status={status} reload={data.reloadTeam} />
      {claude && <DeliverySetting projectId={data.id} row={claude} status={status} reload={data.reloadTeam} />}
      <ReaskSetting projectId={data.id} status={status} reload={data.reloadTeam} />
    </SettingGroup>
    <SettingGroup group={access} prefix={prefix} level={3}>
      <SettingRow label="Full access">
        <span className="set-label">Full access</span>
        <span className="set-ctl"><Button size="small" onClick={() => go(`${base}/agents?section=access`)}>Change in Agent settings</Button></span>
        <span className="tg-terminal-note">{status.master_lands === "merges" ? "Full access: the master resolves conflicts and merges into main itself."
          : status.master_lands === "waits" ? "Full access: the master resolves conflicts, then waits for your Accept."
            : status.access_unreadable ? ACCESS_UNREADABLE
              : "Not at Full access: the master's conflict fixes wait for your Accept."}</span>
      </SettingRow>
    </SettingGroup>
  </div>;
}

/** Holds the project's Master settings drawer above the view that opens it, so a setting that
 * swaps that view leaves the drawer and the owner's place in it alone, and closing the drawer
 * returns to the button of the view now shown (DEC-117 Q4). Inputs: the project data, whether
 * its master runs in a live terminal and the view. Output: the view and its drawer. */
export function MasterSettingsDrawer({ data, liveHarness, children }: { data: ProjectData; liveHarness: boolean; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const button = useId();
  return <MasterSettingsHost.Provider value={{ open: () => setOpen(true), button }}>
    {children}
    <Drawer open={open} title="Master settings" onClose={() => setOpen(false)} returnFocus={() => document.getElementById(button)}>
      <MasterSettings data={data} liveHarness={liveHarness} onLeave={() => setOpen(false)} />
    </Drawer>
  </MasterSettingsHost.Provider>;
}

/** The Master settings button of a conversation head; it opens the drawer held above the
 * view. Output: the button. */
export function MasterSettingsButton() {
  const host = useContext(MasterSettingsHost);
  return <Button id={host.button} size="small" icon="settings" onClick={host.open}>Master settings</Button>;
}
