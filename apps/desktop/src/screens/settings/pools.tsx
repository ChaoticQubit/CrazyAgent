import { poolSummaries, POOL_MEMBER_MAX, type PoolMember, type PoolSummary } from "../../api";
import { useLoad, type Loaded } from "../../state";
import { Button, EmptyState, LoadError, NotReady, Panel, Skeleton } from "../../ui";

/** Describes how many pool members are ready: "3 ready, 1 not signed in", or "No accounts".
 * Input: the members. Output: the line. */
export function poolReadiness(members: PoolMember[]): string {
  if (members.length === 0) return "No accounts";
  const waiting = members.filter((member) => member.status === "not_signed_in").length;
  const ready = members.length - waiting;
  return waiting ? `${ready} ready, ${waiting} not signed in` : `${ready} ready`;
}

/** Describes which projects use a pool. Input: the project count. Output: the line. */
export function poolUse(count: number): string {
  return count === 0 ? "Not used" : count === 1 ? "Used by 1 project" : `Used by ${count} projects`;
}

/** The pool list as a pure view. Inputs: the pools read and the new, edit and retry actions. */
export function PoolsView({ pools, onNew, onRetry }: { pools: Loaded<PoolSummary[]>; onNew: () => void; onRetry: () => void }) {
  if (pools.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (pools.kind === "not_ready") return <NotReady what="Pools" />;
  if (pools.kind === "error") return <LoadError message="Couldn't load pools." onRetry={onRetry} />;
  if (pools.value.length === 0) return <EmptyState icon="layers" title="No pools yet"
    action={<Button variant="primary" icon="plus" onClick={onNew}>New pool</Button>}>
    A pool groups up to 15 accounts for your projects to use.
  </EmptyState>;
  return <div className="stack-small">
    <div className="toolbar"><span className="count">{pools.value.length} {pools.value.length === 1 ? "pool" : "pools"}</span>
      <Button variant="primary" icon="plus" onClick={onNew}>New pool</Button></div>
    <Panel padded={false}><div className="rows">
      {pools.value.map((pool) => <div className="pool-row" key={pool.id}>
        <span className="name">{pool.name}</span>
        <span className="secondary">{pool.summary}</span>
        <span className="mono">{pool.members.length} of {POOL_MEMBER_MAX}</span>
        <span className="secondary">{poolReadiness(pool.members)}</span>
        <span className="secondary">{poolUse(pool.project_count)}</span>
      </div>)}
    </div></Panel>
  </div>;
}

/** Settings, Pools: the pool list and the pool editor.
 * Input: whether a new pool editor opens on arrival. */
export function Pools({ creating }: { creating: boolean }) {
  const [pools, reload] = useLoad(poolSummaries);
  return <PoolsView pools={pools} onNew={() => void creating} onRetry={reload} />;
}
