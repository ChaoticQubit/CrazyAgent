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

/** The cleanup row for a given read: nothing while loading, without cleanup in this build or
 * engine, or with no finished agent; else the button, the last outcome and the confirm dialog
 * listing every agent with its harness and size. Inputs: the read, the dialog and pending
 * state, the last note, a harness namer and the handlers. Output: the row. */
export function CleanupView({ list, open, pending, note, harness, onOpen, onCancel, onConfirm }: {
  list: Loaded<FinishedAgent[]>;
  open: boolean;
  pending: boolean;
  note: string;
  harness: (id: string) => string;
  onOpen: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  if (list.kind === "loading" || list.kind === "not_ready" || cleanupNotReady(list)) return note ? <p className="pj-note" role="status">{note}</p> : null;
  if (list.kind === "error") return <p className="pj-error" role="alert">Couldn't read finished agents. {cleanupRefusal(list)}</p>;
  const agents = list.value;
  return <>
    {note && <p className="pj-note" role="status">{note}</p>}
    {agents.length > 0 && <div className="pj-row between">
      <span className="pj-secondary pj-small">Finished agents keep their working copy until you remove it.</span>
      <Button onClick={onOpen}>{cleanupLabel(agents)}</Button>
    </div>}
    <Dialog open={open && agents.length > 0} title={`Remove ${agents.length} finished ${agents.length === 1 ? "agent" : "agents"}?`} onClose={onCancel} busy={pending}
      footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>Keep them</Button>
        <Button variant="danger" pending={pending} onClick={onConfirm}>{`Remove and free ${cleanupSize(agents)}`}</Button></>}>
      <p>Each agent's working copy and folder are deleted. Their landed work stays in your project. CrazyAgent checks each one again first and keeps any that changed.</p>
      <ul className="pj-rows" aria-label="Finished agents to remove">{agents.map((agent) => <li className="pj-row between" key={agent.id}>
        <strong>{displayText(agent.label)}</strong>
        <span className="pj-secondary pj-small">{harness(agent.harness)}</span>
        <span className="pj-small">{cleanupSize([agent])}</span>
      </li>)}</ul>
    </Dialog>
  </>;
}

/** The project's cleanup row: reads the finished agents again when `key` changes, removes every
 * listed one after the owner confirms, then reads the list again. Inputs: the project id, a key
 * that changes when agents finish, and a harness namer. Output: the row. */
export function FinishedAgentsCleanup({ projectId, finished, harness }: { projectId: string; finished: number; harness: (id: string) => string }) {
  const [list, reload] = useLoad(() => loadFinishedAgents(projectId), [projectId, finished]);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [note, setNote] = useState("");

  async function confirm() {
    if (list.kind !== "ok" || list.value.length === 0) return;
    const size = cleanupSize(list.value);
    setPending(true);
    const result = await removeFinishedAgents(projectId, list.value.map((agent) => agent.id));
    setPending(false);
    setOpen(false);
    setNote(result.kind === "ok"
      ? `Removed ${result.value.length} finished ${result.value.length === 1 ? "agent" : "agents"} and freed ${size}.`
      : `Couldn't remove every finished agent. ${cleanupRefusal(result.kind === "error" ? result : { code: "cleanup_unavailable" })}`);
    reload();
  }

  return <CleanupView list={list} open={open} pending={pending} note={note} harness={harness}
    onOpen={() => { setNote(""); setOpen(true); }} onCancel={() => setOpen(false)} onConfirm={() => void confirm()} />;
}
