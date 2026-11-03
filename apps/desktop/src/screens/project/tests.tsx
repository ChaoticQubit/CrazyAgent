import { useEffect, useState } from "react";
import type { Result } from "../../api";
import {
  CASE_KINDS, CASE_STATUSES, PLAN_STATUSES, VERDICTS, cancelChecks, checksRefusal, currentProfile, gapText, gateLine, pathsLine, loadCases, loadChangeSetChecks,
  loadCoverage, loadPlans, loadProfiles, loadRuns, loadVersions, outcomeLabel, runChecks, runLabel, runTone, staleLine, totalsLine, verdictText, whenText,
} from "../../api-checks";
import type { CaseFilter, CaseKind, CaseRow, CaseStatus, ChangeSetChecks as Checks, CoveragePage, CurrentProfile, ListPage, PlanRow, PlanStatus, RunFilter, RunRow, Verdict } from "../../api-checks";
import type { ChangeSet } from "../../api-changes";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, EmptyState, LoadError, Segmented, Select, Skeleton, TextField } from "../../ui";
import { displayText } from "../conversation";
import { Badge, NotReady } from "./shared";
import type { ProjectData } from "./shared";
import { RunDetailPanel } from "./test-run";
import "./tests.css";

/** The Tests tab's sections, in order. */
export const SECTIONS = [
  { id: "cases", label: "Cases" },
  { id: "plans", label: "Plans" },
  { id: "runs", label: "Runs" },
  { id: "coverage", label: "Coverage" },
] as const;
type SectionId = typeof SECTIONS[number]["id"];

const RUN_ID = /^[A-Z]{2,8}-R[1-9][0-9]{0,9}$/;
const SET_ID = /^[A-Z]{2,8}-S[1-9][0-9]{0,9}$/;
const PLAN_ID = /^[A-Z]{2,8}-P[1-9][0-9]{0,9}$/;
const COMMIT = /^[0-9a-f]{40}$/;
const BATCH_BYTES_MAX = 64;
const CHECKS_POLL_MS = 5000;

const KIND_LABEL: Record<CaseKind, string> = { unit: "Unit", integration: "Integration", e2e: "End to end", smoke: "Smoke", scenario: "Scenario", manual: "Manual" };
const STATUS_LABEL: Record<CaseStatus, string> = { draft: "Draft", ready: "Ready", deprecated: "Deprecated" };
const PLAN_LABEL: Record<PlanStatus, string> = { draft: "Draft", active: "Active", closed: "Closed" };

/** The run a Tests tab link opens: the `run` query of the hash when it is a run id. Input:
 * the location hash. Output: the run id, or null. */
export function runFromHash(hash: string): string | null {
  const query = hash.split("?")[1];
  const run = query === undefined ? null : new URLSearchParams(query).get("run");
  return run !== null && RUN_ID.test(run) ? run : null;
}

function pick<T extends string>(options: readonly T[], value: string): T | null {
  return (options as readonly string[]).includes(value) ? value as T : null;
}

function idOrNull(value: string, shape: RegExp): string | null {
  const trimmed = value.trim();
  return shape.test(trimmed) ? trimmed : null;
}

function Pager({ pages, cursor, onPrevious, onNext }: { pages: number; cursor: string | null; onPrevious: () => void; onNext: () => void }) {
  if (pages === 1 && cursor === null) return null;
  return <div className="pj-row">
    <Button size="small" disabled={pages === 1} onClick={onPrevious}>Previous page</Button>
    <span className="pj-small pj-secondary">Page {pages}</span>
    <Button size="small" disabled={cursor === null} onClick={onNext}>Next page</Button>
  </div>;
}

function Failed<T>({ page, what, onRetry }: { page: Loaded<T>; what: string; onRetry: () => void }) {
  if (page.kind === "loading") return <Skeleton shape="row" count={3} />;
  if (page.kind === "not_ready") return <NotReady what={`${what} aren't in this engine yet.`} />;
  if (page.kind === "error") return <LoadError message={checksRefusal(page)} onRetry={onRetry} />;
  return null;
}

/** What a paged list shows and the paging it offers. */
interface Paged<T> {
  page: Loaded<ListPage<T>>;
  pages: number;
  onPrevious: () => void;
  onNext: () => void;
  onRetry: () => void;
}

/** The Cases list with its filters: kind, status, set and cases no criterion links. Input:
 * the page, the filter and their handlers. Output: the section. */
export function CasesView(props: Paged<CaseRow> & { filter: CaseFilter; onFilter: (filter: CaseFilter) => void }) {
  const { page, filter, onFilter } = props;
  const [set, setSet] = useState(filter.set ?? "");
  return <section className="pj-stack" aria-label="Cases">
    <div className="tt-filters">
      <Select id="tt-case-kind" label="Kind" value={filter.kind ?? ""} onValue={(value) => onFilter({ ...filter, kind: pick(CASE_KINDS, value) })}
        options={[{ value: "", label: "Any kind" }, ...CASE_KINDS.map((kind) => ({ value: kind, label: KIND_LABEL[kind] }))]} />
      <Select id="tt-case-status" label="Status" value={filter.status ?? ""} onValue={(value) => onFilter({ ...filter, status: pick(CASE_STATUSES, value) })}
        options={[{ value: "", label: "Any status" }, ...CASE_STATUSES.map((status) => ({ value: status, label: STATUS_LABEL[status] }))]} />
      <TextField id="tt-case-set" label="Set" value={set} placeholder="CA-S1"
        onValue={(value) => { setSet(value); const id = idOrNull(value, SET_ID); if (id !== filter.set && (id !== null || value.trim() === "")) onFilter({ ...filter, set: id }); }}
        error={set.trim() !== "" && idOrNull(set, SET_ID) === null ? "A set id looks like CA-S1." : undefined} />
      <Checkbox label="Only cases linked to no criterion" checked={filter.unlinked} onChecked={(unlinked) => onFilter({ ...filter, unlinked })} />
    </div>
    {page.kind !== "ok" ? <Failed page={page} what="Test cases" onRetry={props.onRetry} />
      : page.value.rows.length === 0 ? <EmptyState icon="list-checks" title="No cases" compact>No case matches these filters.</EmptyState>
        : <ul className="tt-rows">{page.value.rows.map((row) => <li key={row.id} className="tt-row">
          <span className="pj-mono pj-small">{row.id}</span>
          <span className="tt-title">{displayText(row.title)}{row.elided ? "..." : ""}</span>
          <span className="pj-small pj-secondary">{[row.kind && KIND_LABEL[row.kind], row.status && STATUS_LABEL[row.status]].filter(Boolean).join(", ")}</span>
          {row.protected && <Badge word="Protected" tone="info" />}
          <span className="pj-small">{row.latest === null ? "Never run" : outcomeLabel(row.latest)}</span>
        </li>)}</ul>}
    {page.kind === "ok" && <Pager pages={props.pages} cursor={page.value.cursor} onPrevious={props.onPrevious} onNext={props.onNext} />}
  </section>;
}

/** The Plans list with its status filter. Input: the page, the status and their handlers.
 * Output: the section. */
export function PlansView(props: Paged<PlanRow> & { status: PlanStatus | null; onStatus: (status: PlanStatus | null) => void }) {
  const { page } = props;
  return <section className="pj-stack" aria-label="Plans">
    <div className="tt-filters">
      <Select id="tt-plan-status" label="Status" value={props.status ?? ""} onValue={(value) => props.onStatus(pick(PLAN_STATUSES, value))}
        options={[{ value: "", label: "Any status" }, ...PLAN_STATUSES.map((status) => ({ value: status, label: PLAN_LABEL[status] }))]} />
    </div>
    {page.kind !== "ok" ? <Failed page={page} what="Test plans" onRetry={props.onRetry} />
      : page.value.rows.length === 0 ? <EmptyState icon="list-checks" title="No plans" compact>No plan matches this filter.</EmptyState>
        : <ul className="tt-rows">{page.value.rows.map((row) => <li key={row.id} className="tt-row">
          <span className="pj-mono pj-small">{row.id}</span>
          <span className="tt-title">{displayText(row.name)}</span>
          <span className="pj-small pj-secondary">{PLAN_LABEL[row.status]}</span>
          <span className="pj-small">{row.items === 1 ? "1 item" : `${row.items} items`}</span>
        </li>)}</ul>}
    {page.kind === "ok" && <Pager pages={props.pages} cursor={page.value.cursor} onPrevious={props.onPrevious} onNext={props.onNext} />}
  </section>;
}

function RunFilters({ filter, onFilter }: { filter: RunFilter; onFilter: (filter: RunFilter) => void }) {
  const [plan, setPlan] = useState(filter.plan ?? "");
  const [batch, setBatch] = useState(filter.batch ?? "");
  const [commit, setCommit] = useState(filter.commit ?? "");
  const typed = (value: string, shape: RegExp | null, apply: (id: string | null) => void) => {
    const trimmed = value.trim();
    if (trimmed === "") apply(null);
    else if (shape === null ? trimmed.length <= BATCH_BYTES_MAX : shape.test(trimmed)) apply(trimmed);
  };
  return <div className="tt-filters">
    <Select id="tt-run-verdict" label="Verdict" value={filter.verdict ?? ""} onValue={(value) => onFilter({ ...filter, verdict: pick(VERDICTS, value) })}
      options={[{ value: "", label: "Any verdict" }, ...VERDICTS.map((verdict: Verdict) => ({ value: verdict, label: verdictText(verdict).label }))]} />
    <TextField id="tt-run-plan" label="Plan" value={plan} placeholder="CA-P1" error={plan.trim() !== "" && !PLAN_ID.test(plan.trim()) ? "A plan id looks like CA-P1." : undefined}
      onValue={(value) => { setPlan(value); typed(value, PLAN_ID, (id) => onFilter({ ...filter, plan: id })); }} />
    <TextField id="tt-run-batch" label="Change set batch" value={batch} helper="The batch a change set's Run checks started."
      onValue={(value) => { setBatch(value); typed(value, null, (id) => onFilter({ ...filter, batch: id })); }} />
    <TextField id="tt-run-commit" label="Commit" value={commit} error={commit.trim() !== "" && !COMMIT.test(commit.trim()) ? "A commit is 40 hex digits." : undefined}
      onValue={(value) => { setCommit(value); typed(value, COMMIT, (id) => onFilter({ ...filter, commit: id })); }} />
  </div>;
}

/** The Runs list newest first with its filters: verdict, plan, a change set's batch and the
 * source commit. Input: the page, the filter and their handlers. Output: the section. */
export function RunsView(props: Paged<RunRow> & { filter: RunFilter; onFilter: (filter: RunFilter) => void; onOpen: (id: string) => void }) {
  const { page } = props;
  return <section className="pj-stack" aria-label="Runs">
    <RunFilters filter={props.filter} onFilter={props.onFilter} />
    {page.kind !== "ok" ? <Failed page={page} what="Test runs" onRetry={props.onRetry} />
      : page.value.rows.length === 0 ? <EmptyState icon="list-checks" title="No runs" compact>No run matches these filters.</EmptyState>
        : <ul className="tt-rows">{page.value.rows.map((row) => <li key={row.id}>
          <button type="button" className="tt-row tt-open" onClick={() => props.onOpen(row.id)}>
            <span className="pj-mono pj-small">{row.id}</span>
            <span className="tt-title">{displayText(row.title)}{row.elided ? "..." : ""}</span>
            <Badge word={runLabel({ ...row, reason: null })} tone={runTone(row)} />
            <span className="pj-small pj-secondary">{totalsLine(row.totals)}, {whenText(row.started_ms)}</span>
          </button>
        </li>)}</ul>}
    {page.kind === "ok" && <Pager pages={props.pages} cursor={page.value.cursor} onPrevious={props.onPrevious} onNext={props.onNext} />}
  </section>;
}

/** Coverage per version: each criterion of the version's tickets with its cases and their
 * latest results. Input: the versions, the chosen one, its page and their handlers. Output:
 * the section. */
export function CoverageView(props: Omit<Paged<never>, "page"> & { versions: Loaded<string[]>; version: string | null; page: Loaded<CoveragePage> | null; onVersion: (version: string) => void }) {
  const { versions, page } = props;
  if (versions.kind !== "ok") return <Failed page={versions} what="Versions" onRetry={props.onRetry} />;
  if (versions.value.length === 0) return <EmptyState icon="list-checks" title="No versions" compact>Add a version on the Roadmap to see what its tickets' tests cover.</EmptyState>;
  return <section className="pj-stack" aria-label="Coverage">
    <div className="tt-filters">
      <Select id="tt-coverage-version" label="Version" value={props.version ?? ""} onValue={(value) => props.onVersion(value)}
        options={[{ value: "", label: "Pick a version" }, ...versions.value.map((name) => ({ value: name, label: displayText(name) }))]} />
    </div>
    {page === null ? <p className="pj-muted">Pick a version to see its coverage.</p>
      : page.kind !== "ok" ? <Failed page={page} what="Coverage" onRetry={props.onRetry} />
        : <>
          <p className="pj-secondary">{page.value.uncovered_total} of {page.value.criteria_total} criteria have no passing case yet.</p>
          <ul className="tt-rows">{page.value.rows.map((row) => <li key={`${row.criterion.ticket}-${row.criterion.position}`} className="tt-criterion">
            <span className="pj-row between"><span className="pj-mono pj-small">{row.criterion.ticket} #{row.criterion.position}</span><span className="pj-small">{row.state === "met" ? "Met" : row.state === "waived" ? "Waived" : "Open"}</span></span>
            <span>{displayText(row.text)}</span>
            {row.cases.length === 0 ? <span className="pj-small pj-note warn">No case covers this criterion.</span>
              : <ul className="tt-lines">{row.cases.map((item) => <li key={item.case} className="pj-small">
                <span className="pj-mono">{item.case}</span> {displayText(item.title)}{item.proves ? "" : " (related)"}: {item.latest === null ? "never run" : `${outcomeLabel(item.latest.outcome)} in ${item.latest.run}`}
              </li>)}</ul>}
            {row.cases_total > row.cases.length && <span className="pj-small pj-muted">{row.cases_total - row.cases.length} more cases aren't listed.</span>}
          </li>)}</ul>
          <Pager pages={props.pages} cursor={page.value.cursor} onPrevious={props.onPrevious} onNext={props.onNext} />
        </>}
  </section>;
}

function usePages<T>(read: (cursor: string | null) => Promise<Result<ListPage<T>>>, keys: unknown[]) {
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  const [seen, setSeen] = useState(JSON.stringify(keys));
  const shown = JSON.stringify(keys);
  if (shown !== seen) {
    setSeen(shown);
    setCursors([null]);
  }
  const cursor = cursors[cursors.length - 1];
  const [page, reload] = useLoad(() => read(cursor), [shown, cursor]);
  const next = page.kind === "ok" ? page.value.cursor : null;
  return {
    page, reload, pages: cursors.length,
    onPrevious: () => setCursors((list) => list.length > 1 ? list.slice(0, -1) : list),
    onNext: () => { if (next !== null) setCursors((list) => [...list, next]); },
  };
}

function CasesSection({ projectId }: { projectId: string }) {
  const [filter, setFilter] = useState<CaseFilter>({ kind: null, status: null, set: null, unlinked: false });
  const paged = usePages((cursor) => loadCases(projectId, filter, cursor), [projectId, filter]);
  return <CasesView {...paged} onRetry={paged.reload} filter={filter} onFilter={setFilter} />;
}

function PlansSection({ projectId }: { projectId: string }) {
  const [status, setStatus] = useState<PlanStatus | null>(null);
  const paged = usePages((cursor) => loadPlans(projectId, status, cursor), [projectId, status]);
  return <PlansView {...paged} onRetry={paged.reload} status={status} onStatus={setStatus} />;
}

function RunsSection({ projectId, onOpen }: { projectId: string; onOpen: (id: string) => void }) {
  const [filter, setFilter] = useState<RunFilter>({ verdict: null, plan: null, batch: null, commit: null });
  const paged = usePages((cursor) => loadRuns(projectId, filter, cursor), [projectId, filter]);
  return <RunsView {...paged} onRetry={paged.reload} filter={filter} onFilter={setFilter} onOpen={onOpen} />;
}

function CoverageSection({ projectId }: { projectId: string }) {
  const [versions, reloadVersions] = useLoad(() => loadVersions(projectId), [projectId]);
  const [version, setVersion] = useState<string | null>(null);
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  const cursor = cursors[cursors.length - 1];
  const [page, reload] = useLoad<CoveragePage | null>(async () => version === null ? { kind: "ok", value: null } : loadCoverage(projectId, version, cursor), [projectId, version, cursor]);
  const shown = page.kind === "ok" && page.value === null ? null : page as Loaded<CoveragePage>;
  const next = page.kind === "ok" && page.value !== null ? page.value.cursor : null;
  return <CoverageView versions={versions} version={version} page={shown} pages={cursors.length} onRetry={() => { reloadVersions(); reload(); }}
    onVersion={(name) => { setVersion(name === "" ? null : name); setCursors([null]); }}
    onPrevious={() => setCursors((list) => list.length > 1 ? list.slice(0, -1) : list)}
    onNext={() => { if (next !== null) setCursors((list) => [...list, next]); }} />;
}

/** Tests tab: the project's cases, plans, runs and coverage per version as the owner sees
 * them, and one run's detail when a run is opened here or from a change set. Input: the
 * project data. Output: the tab. */
export function TestsTab({ data }: { data: ProjectData }) {
  const [section, setSection] = useState<SectionId>("runs");
  const [run, setRun] = useState<string | null>(() => runFromHash(typeof window === "undefined" ? "" : window.location.hash));
  if (run !== null) return <RunDetailPanel projectId={data.id} runId={run} onBack={() => setRun(null)} />;
  return <div className="pj-stack tt-tab">
    <Segmented label="Tests sections" value={section} options={SECTIONS.map((item) => ({ value: item.id, label: item.label }))} onValue={setSection} />
    {section === "cases" && <CasesSection projectId={data.id} />}
    {section === "plans" && <PlansSection projectId={data.id} />}
    {section === "runs" && <RunsSection projectId={data.id} onOpen={setRun} />}
    {section === "coverage" && <CoverageSection projectId={data.id} />}
  </div>;
}

/** What a change set's checks block shows and the actions it offers, without engine calls. */
export interface ChangeSetChecksViewProps {
  checks: Loaded<Checks>;
  head: string;
  current: CurrentProfile | null;
  busy: boolean;
  note: string;
  onRun: () => void;
  onCancel: () => void;
  onOpen: (run: string) => void;
  onRetry: () => void;
}

/** A change set's checks as a pure view: the gate's line when the engine sends it, the latest
 * run of each check with its verdict, whether it still counts for Accept, holdout and
 * protected marks, and Run checks or Cancel. Input: the view props. Output: the block. */
export function ChangeSetChecksView(props: ChangeSetChecksViewProps) {
  const { checks, head, current, busy, note } = props;
  if (checks.kind !== "ok") return <section className="pj-stack-s tt-checks" aria-label="Checks"><h4>Checks</h4><Failed page={checks} what="Checks" onRetry={props.onRetry} /></section>;
  const { jobs, latest, gate } = checks.value;
  const active = jobs.filter((job) => job.state === "queued" || job.state === "running").length;
  const line = gateLine(gate);
  return <section className="pj-stack-s tt-checks" aria-label="Checks">
    <div className="pj-row between">
      <h4>Checks</h4>
      <span className="pj-row">
        <Button size="small" pending={busy} pendingLabel="Asking..." onClick={props.onRun}>Run checks</Button>
        {active > 0 && <Button size="small" variant="quiet" disabled={busy} onClick={props.onCancel}>Cancel checks</Button>}
      </span>
    </div>
    <p className={`tt-gate ${line.tone}`}>{line.text}</p>
    {gate.gate !== "off" && gate.gaps.length > 0 && <ul className="tt-lines" aria-label="Gaps">{gate.gaps.map((gap, at) => <li key={at} className="pj-small">
      {gapText(gap.code)}{gap.check === null ? "" : ` Check: ${displayText(gap.check)}.`}
      {gap.paths.length > 0 && <span className="pj-mono">{` ${displayText(pathsLine(gap))}`}</span>}
    </li>)}</ul>}
    {active > 0 && <p className="pj-small pj-secondary" role="status">{active === 1 ? "1 check is queued or running." : `${active} checks are queued or running.`}</p>}
    {latest.length === 0 ? <p className="pj-muted pj-small">No check has run on this change yet.</p>
      : <ul className="tt-rows">{latest.map((run) => {
        const stale = staleLine(run, head, current);
        return <li key={run.check} className="tt-row tt-check">
          <span className="tt-title">{displayText(run.check)}</span>
          <Badge word={runLabel({ ...run, reason: null })} tone={runTone(run)} />
          {run.holdout && <Badge word="Holdout: agents see only its outcome" tone="info" />}
          {run.protected_touched > 0 && <span className="pj-small pj-note warn">{run.protected_touched === 1 ? "Touched a protected path" : `Touched ${run.protected_touched} protected paths`}</span>}
          {stale !== null && <span className="pj-small pj-note warn">{stale}</span>}
          <Button size="small" variant="quiet" onClick={() => props.onOpen(run.run_id)}>Open run</Button>
        </li>;
      })}</ul>}
    {note && <p className="pj-small pj-error" role="alert">{note}</p>}
  </section>;
}

/** A change set's checks block: reads its jobs and latest runs and the profile in use, asks
 * for a run or cancels one, and rereads every few seconds while checks are queued or running.
 * Inputs: the project id, the change set and how to open a run. Output: the block. */
export function ChangeSetChecks({ projectId, set, navigate }: { projectId: string; set: ChangeSet; navigate: (path: string) => void }) {
  const [checks, reload] = useLoad(() => loadChangeSetChecks(projectId, set.id), [projectId, set.id, set.head]);
  const [profiles] = useLoad(() => loadProfiles(projectId, null), [projectId]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const active = checks.kind === "ok" && checks.value.jobs.some((job) => job.state === "queued" || job.state === "running");
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(reload, CHECKS_POLL_MS);
    return () => clearInterval(timer);
  }, [active]);
  const act = async (call: () => Promise<Result<unknown>>) => {
    setBusy(true);
    const result = await call();
    setBusy(false);
    setNote(result.kind === "error" ? checksRefusal(result) : result.kind === "not_ready" ? "Running checks isn't in this engine yet." : "");
    reload();
  };
  const current = profiles.kind === "ok" ? currentProfile(profiles.value.rows) : null;
  return <ChangeSetChecksView checks={checks} head={set.head} current={current} busy={busy} note={note}
    onRun={() => void act(() => runChecks(projectId, set.id))} onCancel={() => void act(() => cancelChecks(projectId, set.id))}
    onOpen={(run) => navigate(`/projects/${encodeURIComponent(projectId)}/tests?run=${encodeURIComponent(run)}`)} onRetry={reload} />;
}
