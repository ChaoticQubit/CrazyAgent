import { useEffect, useState } from "react";
import { accountLabels, accountRanking, listAccounts, poolSummaries, type Result } from "../../api";
import { refreshUsage, setManualReset, usageRefusal } from "../../api-usage";
import { useAppSession, useLoad } from "../../state";
import { PageHeader, type SidebarProject } from "../../ui";
import { NextAccountsPanel } from "../settings/account-choice";
import type { LimitsControls } from "./limits";
import { MetricsView } from "./view";

const TICK_MS = 30_000;
const NOT_READY = "This isn't ready in this build yet.";

/** The app Metrics page from the bottom of the sidebar: every project's usage, each account's
 * limits and the next accounts, with a filter for one project. A picked project that leaves
 * the list falls back to every project. Input: the engine's projects. */
export function AppMetrics({ projects }: { projects: SidebarProject[] }) {
  const session = useAppSession();
  const [picked, setPicked] = useState<string | null>(null);
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [tick, setTick] = useState(0);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const [pools] = useLoad(poolSummaries, [], "pool-summaries");
  const [ranking] = useLoad(accountRanking, [tick, revision]);
  useEffect(() => {
    const timer = setInterval(() => { setNowMs(Date.now()); setTick((old) => old + 1); }, TICK_MS);
    return () => clearInterval(timer);
  }, []);

  async function act(id: string, action: () => Promise<Result<void>>, done: string) {
    setFailure("");
    setBusy(id);
    const result = session.noteResult(await action());
    setBusy(null);
    if (result.kind === "ok") session.showToast(done);
    else setFailure(result.kind === "not_ready" ? NOT_READY : usageRefusal(result));
    setNowMs(Date.now());
    setRevision((old) => old + 1);
  }

  const names = accountLabels(accounts.kind === "ok" ? accounts.value : []);
  const limits: LimitsControls = {
    pools: new Map(pools.kind === "ok" ? pools.value.map((row) => [row.id, row.name]) : []),
    failure,
    busy,
    revision,
    next: <NextAccountsPanel ranking={ranking} pools={pools} names={names} nowMs={nowMs} />,
    onRefresh: () => void act("refresh", refreshUsage, "Checking limits now. They update here within a minute."),
    onSetReset: (account, kind, at) => void act(`${account}/${kind}`, () => setManualReset(account, kind, at),
      at === null ? "Using the provider's reset time." : "Reset time saved."),
  };
  const projectId = picked !== null && projects.some((project) => project.id === picked) ? picked : null;
  return <>
    <PageHeader title="Metrics" description="Every project on this engine in one place." />
    <MetricsView projectId={projectId} filter={{ projects, onProject: setPicked, limits }} />
  </>;
}
