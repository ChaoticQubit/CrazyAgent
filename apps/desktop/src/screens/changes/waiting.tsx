import { Notice, formatRoute } from "../../ui";
import { waitingCount, waitingLine } from "./model";
import { useChangeList } from "./poll";

const WAITING_POLL_MS = 10_000;

/** The notice that change sets wait for the owner, linking to the tab they are reviewed in:
 * Changes, or the IDE once it replaces Changes. Inputs: the project id, how many wait and
 * that tab. Output: the notice, or nothing when none wait. */
export function WaitingChangesView({ projectId, count, place = "changes" }: { projectId: string; count: number; place?: "changes" | "ide" }) {
  const line = waitingLine(count, place);
  if (line === null) return null;
  return <div role="status" className="pj-waiting">
    <Notice tone="info" icon="git-branch">{line}. <a href={formatRoute({ name: "project", id: projectId, tab: place })}>{place === "ide" ? "Open IDE" : "Open Changes"}</a></Notice>
  </div>;
}

/** Reads the project's change sets a few seconds after each reply, one read at a time, and
 * shows how many wait for the owner. Inputs: the project id and the tab change sets are
 * reviewed in. Output: the notice, or nothing while none wait or the list can't be read. */
export function WaitingChanges({ projectId, place }: { projectId: string; place: "changes" | "ide" }) {
  const [list] = useChangeList(projectId, WAITING_POLL_MS);
  const count = list?.kind === "ok" ? waitingCount(list.value) : 0;
  return <WaitingChangesView projectId={projectId} count={count} place={place} />;
}
