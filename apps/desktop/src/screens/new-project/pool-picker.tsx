import type { PoolChoice } from "../../api-projects";
import { poolSummary } from "./wizard";

/** Radio cards for choosing a project's pool, with an optional "Choose later".
 * Inputs: the pools, the chosen id (null for later), whether later is offered and a handler.
 * Output: the radio group. */
export function PoolPicker({ pools, value, later, onChange, name }: {
  pools: PoolChoice[];
  value: string | null;
  later: boolean;
  onChange: (pool: string | null) => void;
  name: string;
}) {
  return <div className="pj-radio-cards" role="radiogroup" aria-label="Pool">
    {pools.map((choice) => <label className={`pj-radio-card ${value === choice.pool.id ? "selected" : ""}`} key={choice.pool.id}>
      <input type="radio" name={name} value={choice.pool.id} checked={value === choice.pool.id} onChange={() => onChange(choice.pool.id)} />
      <span className="pj-radio-body">
        <strong>{choice.pool.id}</strong>
        <span className="pj-secondary">{poolSummary(choice.members)}</span>
        <span className="pj-small pj-muted">{choice.pool.members.length} of 15</span>
        {choice.pool.members.length === 0 && <span className="pj-note warn">No accounts in this pool are ready. Research waits until one signs in.</span>}
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
