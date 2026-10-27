import { useEffect, useRef, useState } from "react";
import { loadTeamGraph, setLiveMaster } from "../../api";
import { failureCopy, receiptState } from "../../api-projects";
import type { GraphNode } from "../../engine-client";
import { Button, Icon, NotReady } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { TerminalPanel } from "../team";
import "../team/team-graph.css";
import { ConversationTab } from "./conversation";
import { valueOf } from "./shared";
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

type Live =
  | { kind: "starting" }
  | { kind: "not_ready" }
  | { kind: "refused"; copy: string }
  | { kind: "running"; node: GraphNode | null };

/** The project's master area: the master harness's own app in its live terminal, filling the
 * area, where Enter sends and Shift+Enter starts a new line. Turning it on asks the engine to
 * keep the master running; the conversation from before stays readable below a toggle.
 * Input: the project data. Output: the area; a harness without a live app keeps the
 * engine-driven conversation. */
export function MasterArea({ data }: { data: ProjectData }) {
  const setup = valueOf(data.setup);
  const harness = setup?.team.master;
  if (setup && !runsLive(harness)) return <ConversationTab data={data} />;
  return <LiveMaster data={data} harness={harness} />;
}

function LiveMaster({ data, harness }: { data: ProjectData; harness: string | undefined }) {
  const [live, setLive] = useState<Live>({ kind: "starting" });
  const [earlier, setEarlier] = useState(false);
  const current = useRef(data.id);
  current.current = data.id;
  const harnesses = valueOf(data.harnesses);
  const name = (id: string) => harnessName(harnesses, id);

  useEffect(() => {
    const id = data.id;
    let timer: number | undefined;
    let stopped = false;
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
      if (turned.kind !== "ok") return setLive({ kind: "refused", copy: failureCopy(turned) });
      const state = receiptState(turned.value);
      if (state.kind === "rejected") return setLive({ kind: "refused", copy: failureCopy(state) });
      await read();
      timer = window.setInterval(() => void read(), POLL_MS);
    })();
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [data.id]);

  return <div className="pj-master-live">
    {live.kind === "not_ready" && <NotReady what="the live master terminal isn't in this build." />}
    {live.kind === "refused" && <p className="pj-error" role="alert">The master terminal couldn't start. {live.copy}</p>}
    {live.kind === "starting" && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Starting the master{harness ? ` (${displayText(name(harness))})` : ""}...</p>}
    {live.kind === "running" && !live.node && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Waiting for the master's terminal. It opens once the master signs in and starts.</p>}
    {live.kind === "running" && live.node && <TerminalPanel key={live.node.terminal ?? live.node.id} node={live.node} harness={name} onClose={() => setEarlier(true)} />}
    <div className="pj-master-earlier">
      <Button variant="quiet" size="small" icon="message-square" onClick={() => setEarlier((open) => !open)}>
        {earlier ? "Hide earlier conversation" : "Show earlier conversation"}
      </Button>
      {earlier && <ConversationTab data={data} />}
    </div>
  </div>;
}
