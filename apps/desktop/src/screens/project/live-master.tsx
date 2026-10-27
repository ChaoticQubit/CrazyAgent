import { useEffect, useRef, useState } from "react";
import { loadTeamGraph, setLiveMaster } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { GraphNode } from "../../engine-client";
import { Button, Icon } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { TerminalPanel } from "../team";
import "../team/team-graph.css";
import { ConversationTab } from "./conversation";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

const LIVE_HARNESSES = ["codex", "claude-code"];
const POLL_MS = 2000;

/** Whether a master harness runs as its own app in a live terminal. Input: the team plan's
 * master harness id. Output: true for Codex and Claude Code, whose apps the engine reads. */
export function runsLive(harness: string | undefined): boolean {
  return harness !== undefined && LIVE_HARNESSES.includes(harness);
}

/** Finds the master's node in a team graph. Input: the graph nodes. Output: the master node,
 * or null before the engine lists one. */
export function masterNode(nodes: GraphNode[]): GraphNode | null {
  return nodes.find((node) => node.kind === "master") ?? null;
}

/** The live master's state as this area knows it: starting, not in this build, refused by
 * the engine with its code and plain copy, or running with the master's terminal node once
 * the team lists one. */
export type Live =
  | { kind: "starting" }
  | { kind: "not_ready" }
  | { kind: "refused"; code: string; copy: string }
  | { kind: "running"; node: GraphNode | null };

const NO_TERMINAL = "team_master_no_terminal";

/** The project's master area. A Codex or Claude Code master is its own app in a live
 * terminal filling the area, where Enter sends and Shift+Enter starts a new line; opening
 * the area asks the engine to keep it running. Input: the project data. Output: the area;
 * any other master, a project without a team plan and a plan that couldn't be read keep the
 * engine-driven conversation. */
export function MasterArea({ data }: { data: ProjectData }) {
  if (data.setup === undefined) return <p className="pj-small pj-muted" role="status">Loading the master...</p>;
  const harness = valueOf(data.setup)?.team.master;
  if (harness === undefined || !runsLive(harness)) return <ConversationTab data={data} />;
  return <LiveMaster data={data} harness={harness} />;
}

function LiveMaster({ data, harness }: { data: ProjectData; harness: string }) {
  const [live, setLive] = useState<Live>({ kind: "starting" });
  const [round, setRound] = useState(0);
  const current = useRef(data.id);
  current.current = data.id;

  useEffect(() => {
    const id = data.id;
    let timer: number | undefined;
    let stopped = false;
    setLive({ kind: "starting" });
    const read = async () => {
      const graph = await loadTeamGraph(id);
      if (stopped || current.current !== id) return;
      if (graph.kind === "not_ready") return setLive({ kind: "not_ready" });
      if (graph.kind !== "ok") return;
      const node = masterNode(graph.value.nodes);
      setLive({ kind: "running", node: node?.terminal ? node : null });
    };
    void (async () => {
      const turned = await setLiveMaster(id, true);
      if (stopped || current.current !== id) return;
      if (turned.kind === "not_ready") return setLive({ kind: "not_ready" });
      if (turned.kind !== "ok") return setLive({ kind: "refused", code: turned.code, copy: failureCopy(turned) });
      const state = receiptState(turned.value);
      if (state.kind === "rejected") return setLive({ kind: "refused", code: state.code, copy: failureCopy(state) });
      await read();
      timer = window.setInterval(() => void read(), POLL_MS);
    })();
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [data.id, round]);

  return <LiveMasterView data={data} harness={harness} live={live} onRetry={() => setRound((value) => value + 1)} />;
}

/** The live master area in one state: the master's terminal filling the area, a line while
 * it starts or can't, or the earlier engine-driven conversation, read-only, behind a toggle.
 * An engine that says the master has no terminal app gets today's conversation instead.
 * Inputs: the project data, the master harness id, the state and the retry action. Output:
 * the area. */
export function LiveMasterView({ data, harness, live, onRetry }: { data: ProjectData; harness: string; live: Live; onRetry: () => void }) {
  const [earlier, setEarlier] = useState(false);
  const harnesses = valueOf(data.harnesses);
  const name = (id: string) => harnessName(harnesses, id);
  if (live.kind === "refused" && live.code === NO_TERMINAL) return <ConversationTab data={data} />;

  return <div className="pj-master-live">
    <div className="pj-master-head">
      <span>{earlier ? "Earlier conversation" : displayText(`Master: ${name(harness)}, live`)}</span>
      <Button variant="quiet" size="small" icon={earlier ? "square-terminal" : "message-square"} onClick={() => setEarlier((open) => !open)}>
        {earlier ? "Back to the live master" : "Earlier conversation"}
      </Button>
    </div>
    {earlier && <ConversationTab data={data} readOnly />}
    {!earlier && live.kind === "not_ready" && <NotReady what="the live master terminal isn't in this build." />}
    {!earlier && live.kind === "refused" && <div className="pj-note warn" role="alert">
      <p>The master terminal couldn't start. {live.copy}</p>
      <Button size="small" icon="refresh-cw" onClick={onRetry}>Try again</Button>
    </div>}
    {!earlier && live.kind === "starting" && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Starting the master ({displayText(name(harness))})...</p>}
    {!earlier && live.kind === "running" && !live.node && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Waiting for the master's terminal. It opens once the master signs in and starts.</p>}
    {!earlier && live.kind === "running" && live.node && <TerminalPanel key={live.node.terminal ?? live.node.id} node={live.node} harness={name} />}
  </div>;
}
