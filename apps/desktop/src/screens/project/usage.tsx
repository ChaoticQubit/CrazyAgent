import { loadUsage, usageNotReady, usageRefusal, type UsageReport } from "../../api-usage";
import type { TeamAgent } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { Panel, Skeleton } from "../../ui";
import { agentLabel, compactCount, offsetMinutes, rangeStart, totalTokens } from "../settings/usage";

const TICK_MS = 30_000;

/** What one agent used and changed in one range. */
export interface AgentSpan {
  tokens: number;
  added: number;
  removed: number;
}

/** One agent's row on a project's Usage card, keyed like the engine's usage rows. */
export interface AgentUsage {
  key: string;
  name: string;
  today: AgentSpan;
  week: AgentSpan;
}

type Named = Pick<TeamAgent, "id" | "role" | "label">;

function span(report: UsageReport, key: string): AgentSpan {
  const row = report.agents.find((item) => item.key === key);
  const lines = report.agent_lines.find((item) => item.agent === key);
  return { tokens: row ? totalTokens(row.tokens) : 0, added: lines?.added ?? 0, removed: lines?.removed ?? 0 };
}

function agentName(key: string, agents: Named[]): string {
  if (key.startsWith("master-") || key === "master") return agents.find((agent) => agent.role === "master")?.label ?? "Master";
  const team = /^team-(\d+)$/.exec(key);
  return (team && agents.find((agent) => agent.id === Number(team[1]))?.label) || agentLabel(key);
}

/** Builds a project's Usage card rows: every agent with tokens or lines changed in the last
 * 7 days, most tokens first, named as the team names it ("Ada (@login-page)", the master by
 * its team label) or by its usage key. Inputs: today's and the 7-day project reports and the
 * team's agents. Output: the rows. */
export function agentUsage(today: UsageReport, week: UsageReport, agents: Named[]): AgentUsage[] {
  const keys = new Set([...week.agents.map((row) => row.key), ...week.agent_lines.map((row) => row.agent)]);
  return [...keys].map((key) => ({ key, name: agentName(key, agents), today: span(today, key), week: span(week, key) }))
    .sort((a, b) => b.week.tokens - a.week.tokens || (b.week.added + b.week.removed) - (a.week.added + a.week.removed) || a.name.localeCompare(b.name));
}

function linesText(value: AgentSpan): string {
  return `+${value.added} -${value.removed}`;
}

/** The project's Usage card for given reports: a row per agent with tokens and lines changed
 * today and over 7 days. Nothing shows when this build or engine has no usage. Inputs: today's
 * and the 7-day reads and the team's agents. Output: the card. */
export function AgentUsageView({ today, week, agents }: { today: Loaded<UsageReport>; week: Loaded<UsageReport>; agents: Named[] }) {
  if ([today, week].some((read) => read.kind !== "loading" && usageNotReady(read))) return null;
  return <Panel title="Usage" padded={false}>{cardBody(today, week, agents)}</Panel>;
}

function cardBody(today: Loaded<UsageReport>, week: Loaded<UsageReport>, agents: Named[]) {
  const failed = today.kind === "error" ? today : week.kind === "error" ? week : null;
  if (failed) return <p className="pj-error" role="alert">Couldn't load usage. {usageRefusal(failed)}</p>;
  if (today.kind !== "ok" || week.kind !== "ok") return <Skeleton shape="card" count={1} />;
  return <AgentUsageTable rows={agentUsage(today.value, week.value, agents)} />;
}

function AgentUsageTable({ rows }: { rows: AgentUsage[] }) {
  if (rows.length === 0) return <p className="pj-muted">No agent used tokens or changed lines in the last 7 days.</p>;
  return <div className="pj-table-wrap usage-table"><table className="pj-table">
    <thead><tr><th scope="col">Agent</th><th scope="col">Tokens today</th><th scope="col">Lines today</th><th scope="col">Tokens, 7 days</th><th scope="col">Lines, 7 days</th></tr></thead>
    <tbody>{rows.map((row) => <tr key={row.key}>
      <th scope="row">{row.name}</th>
      <td>{compactCount(row.today.tokens)}</td>
      <td>{linesText(row.today)}</td>
      <td>{compactCount(row.week.tokens)}</td>
      <td>{linesText(row.week)}</td>
    </tr>)}</tbody>
  </table></div>;
}

/** A project's Usage card: reads today's and the last 7 days' usage for the project, again
 * every 30 seconds as `nowMs` moves on. Inputs: the project id, the team's agents and now in
 * milliseconds. Output: the card. */
export function AgentUsageCard({ projectId, agents, nowMs }: { projectId: string; agents: Named[]; nowMs: number }) {
  const tick = Math.floor(nowMs / TICK_MS);
  const [today] = useLoad(() => {
    const now = new Date();
    return loadUsage(rangeStart("today", now), offsetMinutes(now), projectId);
  }, [projectId, tick]);
  const [week] = useLoad(() => {
    const now = new Date();
    return loadUsage(rangeStart("7d", now), offsetMinutes(now), projectId);
  }, [projectId, tick]);
  return <AgentUsageView today={today} week={week} agents={agents} />;
}
