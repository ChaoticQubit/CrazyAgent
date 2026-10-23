import type { PoolSummary } from "../../api";
import type { PoolChoice } from "../../api-projects";
import { displayText } from "../conversation";
import { poolReadiness } from "../settings/pools";
import { poolName, poolSummary, readyCount } from "./wizard";

/** Radio cards for choosing a project's pool, with readiness and an optional "Choose later".
 * Inputs: the pools, their summaries when known, the chosen id (null for later), whether later
 * is offered and a handler. Output: the radio group. */
export function PoolPicker({ pools, summaries = [], value, later, onChange, name }: {
  pools: PoolChoice[];
  summaries?: PoolSummary[];
  value: string | null;
  later: boolean;
  onChange: (pool: string | null) => void;
  name: string;
}) {
  return <div className="pj-radio-cards" role="radiogroup" aria-label="Pool">
    {pools.map((choice) => ({ choice, summary: summaries.find((item) => item.id === choice.pool.id) })).map(({ choice, summary }) => <label className={`pj-radio-card ${value === choice.pool.id ? "selected" : ""}`} key={choice.pool.id}>
      <input type="radio" name={name} value={choice.pool.id} checked={value === choice.pool.id} onChange={() => onChange(choice.pool.id)} />
      <span className="pj-radio-body">
        <strong>{displayText(poolName(choice.pool))}</strong>
        <span className="pj-secondary">{poolSummary(choice.members)}</span>
        {summary && <span className="pj-small pj-muted">{poolReadiness(summary.members)}</span>}
        {(choice.pool.members.length === 0 || (summary && readyCount(summary) === 0)) && <span className="pj-note warn">No accounts in this pool are ready. Research waits until one signs in.</span>}
      </span>
    </label>)}
    {later && <label className={`pj-radio-card ${value === null ? "selected" : ""}`}>
      <input type="radio" name={name} value="" checked={value === null} onChange={() => onChange(null)} />
      <span className="pj-radio-body">
        <strong>Choose later</strong>
        <span className="pj-secondary">Research waits and shows Needs a pool until you choose one.</span>
      </span>
    </label>}
  </div>;
}
