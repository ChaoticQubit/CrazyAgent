import { useState } from "react";
import { accountSchedule, setAccountSchedule, type AccountPin, type AccountPlan, type AccountSchedule, type AccountStrategy, type AccountView, type PoolSummary, type Result, type ScheduleChange } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, Notice, NotReady, Panel, Select, Skeleton, TextField } from "../../ui";

export const STRATEGIES: { value: AccountStrategy; label: string; hint: string }[] = [
  { value: "use_it_or_lose_it", label: "Use it or lose it", hint: "The account whose weekly limit resets soonest goes first, then your rank." },
  { value: "manual_rank", label: "Manual rank", hint: "Accounts go in the order you rank them." },
  { value: "balanced", label: "Balanced", hint: "The account with the least weekly use goes first." },
];

const DEFAULT_PLAN: AccountPlan = { rank: null, reserve_percent: 0, pin: null };

/** The plain name of a strategy. Input: the strategy. Output: its label. */
export function strategyLabel(strategy: AccountStrategy): string {
  return STRATEGIES.find((item) => item.value === strategy)?.label ?? strategy;
}

/** The strategy a pool sets for itself, or null when it follows the app default.
 * Inputs: the schedule and the pool id. Output: the strategy or null. */
export function poolStrategy(schedule: AccountSchedule, pool: string): AccountStrategy | null {
  return schedule.pools.find((item) => item.id === pool)?.strategy ?? null;
}

/** The strategy a project sets for itself, or null when it follows its pool.
 * Inputs: the schedule and the project id. Output: the strategy or null. */
export function projectStrategy(schedule: AccountSchedule, project: string): AccountStrategy | null {
  return schedule.projects.find((item) => item.id === project)?.strategy ?? null;
}

/** An account's saved plan, or the default plan: no rank, no reserve, no pin.
 * Inputs: the schedule and the account id. Output: the plan. */
export function planFor(schedule: AccountSchedule, connection: string): AccountPlan {
  return schedule.accounts.find((item) => item.connection === connection)?.plan ?? DEFAULT_PLAN;
}

/** Reads the rank field: empty for no rank, else a whole number from 1 to 999.
 * Input: the text. Output: the rank, or a plain error. */
export function rankInput(text: string): { kind: "ok"; value: number | null } | { kind: "error"; message: string } {
  const trimmed = text.trim();
  if (trimmed === "") return { kind: "ok", value: null };
  if (!/^\d{1,3}$/.test(trimmed) || Number(trimmed) < 1) return { kind: "error", message: "Use a rank from 1 to 999, or leave it empty." };
  return { kind: "ok", value: Number(trimmed) };
}

/** Reads the reserve field: empty or a whole percent from 0 to 99.
 * Input: the text. Output: the percent, or a plain error. */
export function reserveInput(text: string): { kind: "ok"; value: number } | { kind: "error"; message: string } {
  const trimmed = text.trim().replace(/%$/, "").trim();
  if (trimmed === "") return { kind: "ok", value: 0 };
  if (!/^\d{1,2}$/.test(trimmed)) return { kind: "error", message: "Use a reserve from 0 to 99 percent." };
  return { kind: "ok", value: Number(trimmed) };
}

/** Describes an account's plan in one line, such as "Rank 2, keeps 10% of its week, used last".
 * Input: the plan. Output: the line. */
export function planLine(plan: AccountPlan): string {
  if (plan.pin === "never_use") return "Never used";
  const parts = [plan.rank === null ? "No rank" : `Rank ${plan.rank}`,
    plan.reserve_percent === 0 ? "used to 100%" : `keeps ${plan.reserve_percent}% of its week`];
  if (plan.pin === "use_last") parts.push("used last");
  return parts.join(", ");
}

/** The strategy picker choices for a pool or project: follow the level above, then each strategy.
 * Input: the label of the inherited strategy. Output: the options. */
export function strategyOptions(inherited: string): { value: string; label: string }[] {
  return [{ value: "", label: `Default (${inherited})` }, ...STRATEGIES.map((item) => ({ value: item.value, label: item.label }))];
}

/** One account's rank, reserve and pin with a Save button. Inputs: the account, its plan and
 * the save action. */
function PlanRow({ id, name, plan, onSave }: { id: string; name: string; plan: AccountPlan; onSave: (plan: AccountPlan) => Promise<Result<void>> }) {
  const [rank, setRank] = useState(plan.rank === null ? "" : String(plan.rank));
  const [reserve, setReserve] = useState(String(plan.reserve_percent));
  const [pin, setPin] = useState<string>(plan.pin ?? "");
  const [error, setError] = useState<string>();
  const [saving, setSaving] = useState(false);
  const save = async () => {
    const ranked = rankInput(rank);
    const kept = reserveInput(reserve);
    if (ranked.kind === "error") return setError(ranked.message);
    if (kept.kind === "error") return setError(kept.message);
    setError(undefined);
    setSaving(true);
    const result = await onSave({ rank: ranked.value, reserve_percent: kept.value, pin: pin === "" ? null : pin as AccountPin });
    setSaving(false);
    if (result.kind !== "ok") setError(result.kind === "not_ready" ? "Account choice isn't ready in this build." : result.message);
  };
  return <li className="stack-small">
    <form className="row" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <span className="grow ellipsis">{name}</span>
      <TextField id={`${id}-rank`} label="Rank" value={rank} onValue={setRank} maxLength={3} inputMode="numeric" />
      <TextField id={`${id}-reserve`} label="Reserve %" value={reserve} onValue={setReserve} maxLength={3} inputMode="numeric" />
      <Select id={`${id}-pin`} label="Pin" value={pin} onValue={setPin}
        options={[{ value: "", label: "None" }, { value: "use_last", label: "Use last" }, { value: "never_use", label: "Never use" }]} />
      <Button type="submit" size="small" pending={saving} pendingLabel="Saving...">Save</Button>
    </form>
    <span className="small secondary">{planLine(plan)}</span>
    {error && <Notice tone="danger" alert>{error}</Notice>}
  </li>;
}

/** The account choice panel as a pure view: the app strategy, each pool's strategy and each
 * member's rank, reserve and pin. Inputs: the schedule and pools read, account names and the
 * change action. */
export function AccountChoiceView({ schedule, pools, names, onChange, onRetry }: {
  schedule: Loaded<AccountSchedule>;
  pools: Loaded<PoolSummary[]>;
  names: Map<string, string>;
  onChange: (change: ScheduleChange) => Promise<Result<void>>;
  onRetry: () => void;
}) {
  const [error, setError] = useState<string>();
  if (schedule.kind === "loading" || pools.kind === "loading") return <Skeleton shape="row" count={3} />;
  if (schedule.kind === "not_ready") return <NotReady what="Account choice" />;
  if (schedule.kind === "error" || pools.kind !== "ok") return <LoadError message="Couldn't load account choice." onRetry={onRetry} />;
  const current = schedule.value;
  const send = async (change: ScheduleChange) => {
    const result = await onChange(change);
    setError(result.kind === "ok" ? undefined : result.kind === "not_ready" ? "Account choice isn't ready in this build." : result.message);
    return result;
  };
  const app = STRATEGIES.find((item) => item.value === current.strategy);
  return <Panel title="Account choice">
    <div className="stack">
      <Select id="schedule-app" label="How every pool picks an account" value={current.strategy}
        options={STRATEGIES.map((item) => ({ value: item.value, label: item.label }))}
        helper={app?.hint} onValue={(value) => void send({ kind: "app_strategy", strategy: value as AccountStrategy })} />
      {error && <Notice tone="danger" alert>{error}</Notice>}
      {pools.value.map((pool) => <div className="stack-small" key={pool.id}>
        <Select id={`schedule-${pool.id}`} label={`${pool.name}: account order`} value={poolStrategy(current, pool.id) ?? ""}
          options={strategyOptions(strategyLabel(current.strategy))}
          onValue={(value) => void send({ kind: "pool_strategy", pool: pool.id, strategy: value === "" ? null : value as AccountStrategy })} />
        {pool.members.length === 0 ? <p className="secondary">No accounts in this pool.</p>
          : <ul className="plain-list">{pool.members.map((member) => {
            const plan = planFor(current, member.id);
            return <PlanRow key={`${member.id}-${JSON.stringify(plan)}`} id={`plan-${pool.id}-${member.id}`} name={names.get(member.id) ?? member.provider}
              plan={plan} onSave={(next) => send({ kind: "account", connection: member.id, plan: next })} />;
          })}</ul>}
      </div>)}
    </div>
  </Panel>;
}

/** Settings, Pools: the account choice panel. Inputs: the pools and accounts the page read. */
export function AccountChoice({ pools, accounts }: { pools: Loaded<PoolSummary[]>; accounts: Loaded<AccountView[]> }) {
  const session = useAppSession();
  const [schedule, reload] = useLoad(accountSchedule);
  const names = new Map(accounts.kind === "ok" ? accounts.value.map((account) => [account.id, account.name]) : []);
  return <AccountChoiceView schedule={schedule} pools={pools} names={names} onRetry={reload} onChange={async (change) => {
    const result = session.noteResult(await setAccountSchedule(change));
    if (result.kind === "ok") reload();
    return result;
  }} />;
}

/** The project override as a pure view: a strategy picker whose default follows the pool.
 * Inputs: the schedule read, the project and its pool, and the change action. */
export function ProjectAccountOrderView({ schedule, project, pool, onChange }: {
  schedule: Loaded<AccountSchedule>;
  project: string;
  pool: string | null;
  onChange: (change: ScheduleChange) => Promise<Result<void>>;
}) {
  const [error, setError] = useState<string>();
  if (schedule.kind !== "ok") return null;
  const inherited = (pool === null ? null : poolStrategy(schedule.value, pool)) ?? schedule.value.strategy;
  return <div className="stack-small">
    <Select id="project-account-order" label="Account order for this project" value={projectStrategy(schedule.value, project) ?? ""}
      options={strategyOptions(strategyLabel(inherited))} onValue={(value) => void (async () => {
        const result = await onChange({ kind: "project_strategy", project, strategy: value === "" ? null : value as AccountStrategy });
        setError(result.kind === "ok" ? undefined : result.kind === "not_ready" ? "Account choice isn't ready in this build." : result.message);
      })()} />
    {error && <Notice tone="danger" alert>{error}</Notice>}
  </div>;
}

/** Project Settings, Pool: the project's own account order. Inputs: the project and its pool. */
export function ProjectAccountOrder({ project, pool }: { project: string; pool: string | null }) {
  const session = useAppSession();
  const [schedule, reload] = useLoad(accountSchedule, [project]);
  return <ProjectAccountOrderView schedule={schedule} project={project} pool={pool} onChange={async (change) => {
    const result = session.noteResult(await setAccountSchedule(change));
    if (result.kind === "ok") reload();
    return result;
  }} />;
}
