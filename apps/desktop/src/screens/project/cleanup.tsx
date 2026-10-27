import { useState } from "react";
import { cleanupNotReady, cleanupRefusal, loadFinishedAgents, removeFinishedAgents, type FinishedAgent } from "../../api-cleanup";
import { useLoad, type Loaded } from "../../state";
import { Button, Dialog } from "../../ui";
import { displayText } from "../conversation";
import { formatBytes } from "../new-project/wizard";

/** The size the listed agents hold on disk, "at least" when the engine stopped counting one
 * at its bound. Input: the agents. Output: the size words. */
export function cleanupSize(agents: FinishedAgent[]): string {
  const bytes = agents.reduce((sum, agent) => sum + agent.bytes, 0);
  return `${agents.some((agent) => !agent.counted_all) ? "at least " : ""}${formatBytes(bytes)}`;
}

/** The cleanup button's words: "Clean up 2 finished agents (1.5 GB)". Input: the agents.
 * Output: the label. */
export function cleanupLabel(agents: FinishedAgent[]): string {
  return `Clean up ${agents.length} finished ${agents.length === 1 ? "agent" : "agents"} (${cleanupSize(agents)})`;
}

/** Whether a read still lists exactly the agents the dialog asked about, in any order.
 * Inputs: the agents asked about and the latest read. Output: true when they match. */
function sameAgents(asked: FinishedAgent[], read: FinishedAgent[]): boolean {
  const ids = new Set(read.map((agent) => agent.id));
  return ids.size === asked.length && asked.every((agent) => ids.has(agent.id));
}

/** The agents a confirm removes: the ones the dialog showed when it opened, or none when a
 * later read lists other agents and the owner has to look again. A failed read keeps what the
 * owner saw, since the engine checks each one again. Inputs: the dialog's agents and the
 * latest read. Output: the agent ids to send, or null. */
export function cleanupRequest(asked: FinishedAgent[] | null, list: Loaded<FinishedAgent[]>): number[] | null {
  if (asked === null || asked.length === 0) return null;
  if (list.kind === "ok" && !sameAgents(asked, list.value)) return null;
  return asked.map((agent) => agent.id);
}

/** The cleanup row for a given read: nothing while loading, without cleanup in this build or
 * engine, or with no finished agent; else the button, the last outcome and the confirm dialog
 * listing the agents it opened with, with their harness and size. A read that lists other
 * agents behind the open dialog says so and offers the new list instead of removing. Inputs:
 * the read, the dialog's agents or null while closed, the pending state, the last note, a
 * harness namer and the handlers. Output: the row. */
export function CleanupView({ list, asked, pending, note, harness, onOpen, onCancel, onConfirm, onReview }: {
  list: Loaded<FinishedAgent[]>;
  asked: FinishedAgent[] | null;
  pending: boolean;
  note: string;
  harness: (id: string) => string;
  onOpen: () => void;
  onCancel: () => void;
  onConfirm: () => void;
  onReview: () => void;
}) {
  if (list.kind === "loading" || list.kind === "not_ready" || cleanupNotReady(list)) return note ? <p className="pj-note" role="status">{note}</p> : null;
  if (list.kind === "error") return <p className="pj-error" role="alert">Couldn't read finished agents. {cleanupRefusal(list)}</p>;
  const agents = list.value;
  const shown = asked ?? [];
  const changed = shown.length > 0 && cleanupRequest(shown, list) === null;
  return <>
    {note && <p className="pj-note" role="status">{note}</p>}
    {agents.length > 0 && <div className="pj-row between">
      <span className="pj-secondary pj-small">Finished agents keep their working copy until you remove it.</span>
      <Button onClick={onOpen}>{cleanupLabel(agents)}</Button>
    </div>}
    <Dialog open={shown.length > 0} title={`Remove ${shown.length} finished ${shown.length === 1 ? "agent" : "agents"}?`} onClose={onCancel} busy={pending}
      footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>Keep them</Button>
        {changed
          ? <Button onClick={onReview}>Review the new list</Button>
          : <Button variant="danger" pending={pending} onClick={onConfirm}>{`Remove and free ${cleanupSize(shown)}`}</Button>}</>}>
      <p>Each agent's working copy and folder are deleted. Their landed work stays in your project. CrazyAgent checks each one again first and keeps any that changed.</p>
      {changed && <p className="pj-note warn" role="alert">The list changed. Review it again before anything is removed.</p>}
      <ul className="pj-rows" aria-label="Finished agents to remove">{shown.map((agent) => <li className="pj-row between" key={agent.id}>
        <strong>{displayText(agent.label)}</strong>
        <span className="pj-secondary pj-small">{harness(agent.harness)}</span>
        <span className="pj-small">{cleanupSize([agent])}</span>
      </li>)}</ul>
    </Dialog>
  </>;
}

/** The project's cleanup row: reads the finished agents again when `key` changes, keeps the
 * list the dialog opened with, removes exactly those after the owner confirms, then reads the
 * list again. Inputs: the project id, a key that changes when agents finish, and a harness
 * namer. Output: the row. */
export function FinishedAgentsCleanup({ projectId, finished, harness }: { projectId: string; finished: number; harness: (id: string) => string }) {
  const [list, reload] = useLoad(() => loadFinishedAgents(projectId), [projectId, finished]);
  const [asked, setAsked] = useState<FinishedAgent[] | null>(null);
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState("");

  async function confirm() {
    const ids = cleanupRequest(asked, list);
    if (asked === null || ids === null) return;
    const size = cleanupSize(asked);
    setPending(true);
    const result = await removeFinishedAgents(projectId, ids);
    setPending(false);
    setAsked(null);
    setNote(result.kind === "ok"
      ? `Removed ${result.value.length} finished ${result.value.length === 1 ? "agent" : "agents"} and freed ${size}.`
      : `Couldn't remove every finished agent. ${cleanupRefusal(result.kind === "error" ? result : { code: "cleanup_unavailable" })}`);
    reload();
  }

  const latest = list.kind === "ok" ? list.value : [];
  return <CleanupView list={list} asked={asked} pending={pending} note={note} harness={harness}
    onOpen={() => { setNote(""); setAsked(latest); }} onCancel={() => setAsked(null)}
    onConfirm={() => void confirm()} onReview={() => setAsked(latest)} />;
}
