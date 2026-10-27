import { useEffect, useRef, useState } from "react";
import { loadTeamGraph, setLiveMaster } from "../../api";
import type { Result } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { GraphNode, TeamStatus } from "../../engine-client";
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
 * the engine with its code and plain copy, stopped with why the master last could not start,
 * or running with the master's terminal node once the team lists one. */
export type Live =
  | { kind: "starting" }
  | { kind: "not_ready" }
  | { kind: "refused"; code: string; copy: string }
  | { kind: "stopped"; problem: string | null }
  | { kind: "running"; node: GraphNode | null };

const NO_TERMINAL = "team_master_no_terminal";
const STOPPED = "The live master is stopped. Its terminal opens again once you start it.";

/** The live master switch as the engine last told it. Inputs: the latest team status read
 * and the project shown. Output: undefined before a read of this project arrives; on null
 * when the switch was never set or the read failed, which opening the area turns on; else
 * the switch and why the master last could not start. */
export function liveSwitch(team: Result<TeamStatus> | undefined, projectId: string): { on: boolean | null; problem: string | null } | undefined {
  if (team === undefined) return undefined;
  if (team.kind !== "ok") return { on: null, problem: null };
  if (team.value.project_id !== projectId) return undefined;
  return { on: team.value.live_master ?? null, problem: team.value.live_problem ?? null };
}

/** The project's master area. A Codex or Claude Code master is its own app in a live
 * terminal filling the area, where Enter sends and Shift+Enter starts a new line; the first
 * open of a project whose switch was never set turns it on, and the header stops and starts
 * it. Input: the project data. Output: the area;
 * any other master, a project without a team plan and a plan that couldn't be read keep the
 * engine-driven conversation. */
export function MasterArea({ data }: { data: ProjectData }) {
  if (data.setup === undefined) return <p className="pj-small pj-muted" role="status">Loading the master...</p>;
  const harness = valueOf(data.setup)?.team.master;
  if (harness === undefined || !runsLive(harness)) return <ConversationTab data={data} />;
  return <LiveMaster key={data.id} data={data} harness={harness} />;
}

function LiveMaster({ data, harness }: { data: ProjectData; harness: string }) {
  const [live, setLive] = useState<Live>({ kind: "starting" });
  const [want, setWant] = useState<boolean | null>(null);
  const asked = useRef(false);
  const current = useRef(data.id);
  current.current = data.id;
  const engine = liveSwitch(data.team.result, data.id);
  const on = want ?? engine?.on;

  const turn = async (next: boolean) => {
    const id = data.id;
    asked.current = true;
    setWant(next);
    setLive(next ? { kind: "starting" } : { kind: "stopped", problem: null });
    const turned = await setLiveMaster(id, next);
    if (current.current !== id) return;
    const state = turned.kind === "ok" ? receiptState(turned.value) : null;
    const failed: Live | null = turned.kind === "not_ready" ? { kind: "not_ready" }
      : turned.kind !== "ok" ? { kind: "refused", code: turned.code, copy: failureCopy(turned) }
      : state?.kind === "rejected" ? { kind: "refused", code: state.code, copy: failureCopy(state) }
      : null;
    if (failed) {
      setWant(null);
      if (next) setLive(failed);
      else data.notify(`Couldn't stop the live master. ${failed.kind === "refused" ? failed.copy : "It isn't in this build."}`);
    }
    data.reloadTeam();
  };

  useEffect(() => {
    if (want !== null && engine?.on === want) setWant(null);
  }, [engine?.on, want]);

  useEffect(() => {
    if (on === null && !asked.current) void turn(true);
  }, [on]);

  useEffect(() => {
    if (on !== true) return;
    const id = data.id;
    let stopped = false;
    const read = async () => {
      const graph = await loadTeamGraph(id);
      if (stopped || current.current !== id) return;
      if (graph.kind === "not_ready") return setLive({ kind: "not_ready" });
      if (graph.kind !== "ok") return;
      const node = masterNode(graph.value.nodes);
      setLive((prior) => prior.kind === "refused" || prior.kind === "not_ready" ? prior : { kind: "running", node: node?.terminal ? node : null });
    };
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [data.id, on === true]);

  const shown: Live = live.kind === "refused" || live.kind === "not_ready" ? live
    : on === false ? { kind: "stopped", problem: want === false ? null : engine?.problem ?? null }
    : live;
  return <LiveMasterView data={data} harness={harness} live={shown} onSwitch={(next) => void turn(next)} />;
}

/** The live master area in one state: a header with the switch, then the master's terminal
 * filling the area, a line while it starts, can't or is stopped, or the earlier engine-driven
 * conversation, read-only, behind a toggle. An engine that says the master has no terminal
 * app gets today's conversation instead. Inputs: the project data, the master harness id, the
 * state and the switch action. Output: the area. */
export function LiveMasterView({ data, harness, live, onSwitch }: { data: ProjectData; harness: string; live: Live; onSwitch: (on: boolean) => void }) {
  const [earlier, setEarlier] = useState(false);
  const harnesses = valueOf(data.harnesses);
  const name = (id: string) => harnessName(harnesses, id);
  if (live.kind === "refused" && live.code === NO_TERMINAL) return <ConversationTab data={data} />;
  const off = live.kind === "stopped" || live.kind === "refused";

  return <div className="pj-master-live">
    <div className="pj-master-head">
      <span>{earlier ? "Earlier conversation" : displayText(`Master: ${name(harness)}, ${live.kind === "stopped" ? "stopped" : "live"}`)}</span>
      {!earlier && !off && live.kind !== "not_ready" && <span className="pj-small pj-muted pj-master-note">Live master starts when you open this tab.</span>}
      <span className="pj-row">
        {!earlier && live.kind !== "not_ready" && <Button variant="quiet" size="small" icon={off ? "square-terminal" : "circle-stop"} onClick={() => onSwitch(off)}>
          {off ? "Start live master" : "Stop live master"}
        </Button>}
        <Button variant="quiet" size="small" icon={earlier ? "square-terminal" : "message-square"} onClick={() => setEarlier((open) => !open)}>
          {earlier ? "Back to the live master" : "Earlier conversation"}
        </Button>
      </span>
    </div>
    {earlier && <ConversationTab data={data} readOnly />}
    {!earlier && live.kind === "stopped" && <div className={`pj-note${live.problem ? " warn" : ""}`} role={live.problem ? "alert" : "status"}>
      <p>{displayText(live.problem ?? STOPPED)}</p>
    </div>}
    {!earlier && live.kind === "not_ready" && <NotReady what="the live master terminal isn't in this build." />}
    {!earlier && live.kind === "refused" && <div className="pj-note warn" role="alert">
      <p>The master terminal couldn't start. {live.copy}</p>
      <Button size="small" icon="refresh-cw" onClick={() => onSwitch(true)}>Try again</Button>
    </div>}
    {!earlier && live.kind === "starting" && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Starting the master ({displayText(name(harness))})...</p>}
    {!earlier && live.kind === "running" && !live.node && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Waiting for the master's terminal. It opens once the master signs in and starts.</p>}
    {!earlier && live.kind === "running" && live.node && <TerminalPanel key={live.node.terminal ?? live.node.id} node={live.node} harness={name} />}
  </div>;
}
