import { useState } from "react";
import { accountSchedule, setAccountSchedule, type AccountPlan, type AccountSchedule, type AccountStanding, type AccountStrategy, type PoolRanking, type PoolSummary, type Result, type ScheduleChange } from "../../api";
import { useAppSession, useHeld, useLoad, type Loaded } from "../../state";
import { Notice, Panel, Select, Skeleton, type SelectOption } from "../../ui";

export const STRATEGIES: { value: AccountStrategy; label: string; hint: string }[] = [
  { value: "use_it_or_lose_it", label: "Use it or lose it", hint: "The account whose weekly limit resets soonest goes first, then your rank." },
  { value: "manual_rank", label: "Manual rank", hint: "Accounts go in the order you rank them." },
  { value: "balanced", label: "Balanced", hint: "The account with the least weekly use goes first." },
];

const DEFAULT_PLAN: AccountPlan = { rank: null, reserve_percent: 0, pin: null };
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** A local time a reset or pause ends at: "14:30" today, else "Mon 09:00".
 * Inputs: the time and now in ms since the epoch. Output: the text. */
export function atText(atMs: number, nowMs: number): string {
  const at = new Date(atMs);
  const now = new Date(nowMs);
  const time = `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  const today = at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth() && at.getDate() === now.getDate();
  return today ? time : `${WEEKDAYS[at.getDay()]} ${time}`;
}

/** Says in a few words why an account stands where it does in its pool's ranking, such as
 * "resets Mon 09:00, 40% left" or "5-hour cap until 14:30". Inputs: the standing and now.
 * Output: the line. */
export function standingLine(standing: AccountStanding, nowMs: number): string {
  const at = (ms: number) => atText(ms, nowMs);
  switch (standing.kind) {
    case "next": {
      const line = standing.weekly_left_percent === null ? "no usage read yet"
        : standing.weekly_resets_ms === null ? `${standing.weekly_left_percent}% left`
          : `resets ${at(standing.weekly_resets_ms)}, ${standing.weekly_left_percent}% left`;
      return standing.use_last ? `${line}, used last` : line;
    }
    case "not_signed_in": return "not signed in";
    case "sign_in_unconfirmed": return "sign-in needs a check";
    case "paused": return `paused until ${at(standing.until_ms)}`;
    case "never_use": return "never use";
    case "five_hour_cap": return standing.until_ms === null ? "5-hour cap reached" : `5-hour cap until ${at(standing.until_ms)}`;
    case "reserve_reached":
      if (standing.reserve_percent > 0) return `reserve reached (${standing.reserve_percent}%)`;
      return standing.until_ms === null ? "weekly limit used up" : `weekly limit used up until ${at(standing.until_ms)}`;
  }
}

/** One pool's next accounts: each account work would try now, in order, then each held
 * account with why. Inputs: the ranking read, the pool, account names and now. */
export function NextAccountsView({ ranking, pool, names, nowMs }: { ranking: Loaded<PoolRanking[]>; pool: string; names: Map<string, string>; nowMs: number }) {
  if (ranking.kind === "loading") return <Skeleton shape="row" count={2} />;
  if (ranking.kind === "not_ready") return null;
  if (ranking.kind === "error") return <Notice tone="warn">Couldn't read the next accounts.</Notice>;
  const found = ranking.value.find((item) => item.pool === pool);
  if (!found || found.accounts.length === 0) return <p className="muted small">No accounts to rank yet.</p>;
  const row = (connection: string, standing: AccountStanding) => <li key={connection} className="row">
    <span className="grow ellipsis">{names.get(connection) ?? connection}</span><span className="small secondary">{standingLine(standing, nowMs)}</span>
  </li>;
  const next = found.accounts.filter((item) => item.standing.kind === "next");
  const held = found.accounts.filter((item) => item.standing.kind !== "next");
  return <div className="stack-small" aria-label="Next accounts">
    <span className="small strong">Next accounts</span>
    {next.length === 0 ? <p className="muted small">No account can take work now.</p>
      : <ol className="stack-small">{next.map((item) => row(item.connection, item.standing))}</ol>}
    {held.length > 0 && <ul className="plain-list">{held.map((item) => row(item.connection, item.standing))}</ul>}
  </div>;
}

/** Metrics: every pool's next accounts under its name. Inputs: the ranking and pools
 * read, account names and now. */
export function NextAccountsPanel({ ranking, pools, names, nowMs }: { ranking: Loaded<PoolRanking[]>; pools: Loaded<PoolSummary[]>; names: Map<string, string>; nowMs: number }) {
  if (ranking.kind === "not_ready") return null;
  const shown = pools.kind === "ok" ? pools.value : [];
  return <Panel title="Next accounts">
    {shown.length === 0 ? <p className="muted">No pools yet.</p>
      : <div className="stack">{shown.map((pool) => <section key={pool.id} className="stack-small" aria-label={`${pool.name} next accounts`}>
        <h3>{pool.name}</h3>
        <NextAccountsView ranking={ranking} pool={pool.id} names={names} nowMs={nowMs} />
      </section>)}</div>}
  </Panel>;
}

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

/** One account order select: it keeps the owner's pick while its save and the read after it run,
 * and shows the saved value again after a refused save. Inputs: the select's parts, the schedule
 * read the value came from, the help line for a value and the change action. Output: the select. */
function StrategySelect({ id, label, value, read, options, hint, save }: {
  id: string;
  label: string;
  value: string;
  read: AccountSchedule;
  options: SelectOption[];
  hint?: (value: string) => string | undefined;
  save: (value: string) => Promise<Result<void>>;
}) {
  const [shown, hold] = useHeld(value, Object.is, read);
  return <Select id={id} label={label} value={shown} options={options} helper={hint?.(shown)} onValue={(next) => {
    const settled = hold(next);
    void save(next).then((result) => settled(result.kind === "ok" ? "" : result.kind));
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
    <StrategySelect id="project-account-order" label="Account order for this project" value={projectStrategy(schedule.value, project) ?? ""} read={schedule.value}
      options={strategyOptions(strategyLabel(inherited))} save={async (value) => {
        const result = await onChange({ kind: "project_strategy", project, strategy: value === "" ? null : value as AccountStrategy });
        setError(result.kind === "ok" ? undefined : result.kind === "not_ready" ? "Account choice isn't ready in this build." : result.message);
        return result;
      }} />
    {error && <Notice tone="danger" alert>{error}</Notice>}
  </div>;
}

/** Project Settings, Pool: the project's own account order. Inputs: the project and its pool. */
export function ProjectAccountOrder({ project, pool }: { project: string; pool: string | null }) {
  const session = useAppSession();
  const [schedule, reload] = useLoad(accountSchedule, [project], "account-schedule");
  return <ProjectAccountOrderView schedule={schedule} project={project} pool={pool} onChange={async (change) => {
    const result = session.noteResult(await setAccountSchedule(change, schedule.kind === "ok" ? schedule.value.version : 0));
    if (result.kind === "ok" || (result.kind === "error" && result.code === "stale_revision")) reload();
    return result;
  }} />;
}
