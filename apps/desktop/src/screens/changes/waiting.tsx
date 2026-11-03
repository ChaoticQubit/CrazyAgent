import { useEffect, useState } from "react";
import { loadChanges } from "../../api-changes";
import { Notice, formatRoute } from "../../ui";
import { waitingCount, waitingLine } from "./model";

const WAITING_POLL_MS = 10_000;

/** The notice that change sets wait for the owner, linking to the project's Changes tab.
 * Inputs: the project id and how many wait. Output: the notice, or nothing when none wait. */
export function WaitingChangesView({ projectId, count }: { projectId: string; count: number }) {
  const line = waitingLine(count);
  if (line === null) return null;
  return <div role="status" className="pj-waiting">
    <Notice tone="info" icon="git-branch">{line}. <a href={formatRoute({ name: "project", id: projectId, tab: "changes" })}>Open Changes</a></Notice>
  </div>;
}

/** Reads the project's change sets every few seconds and shows how many wait for the owner.
 * Input: the project id. Output: the notice, or nothing while none wait or the list can't
 * be read. */
export function WaitingChanges({ projectId }: { projectId: string }) {
  const [count, setCount] = useState(0);
  useEffect(() => {
    let current = true;
    const read = () => void loadChanges(projectId).then((result) => {
      if (current) setCount(result.kind === "ok" ? waitingCount(result.value) : 0);
    });
    read();
    const timer = window.setInterval(read, WAITING_POLL_MS);
    return () => { current = false; window.clearInterval(timer); };
  }, [projectId]);
  return <WaitingChangesView projectId={projectId} count={count} />;
}
