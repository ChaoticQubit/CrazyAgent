import { useState } from "react";
import { checksRefusal, coveredRows, integrityLines, loadRun, loadRunResults, outcomeLabel, runLabel, runTone, totalsLine, verdictText, whenText } from "../../api-checks";
import type { IntegrityRef, ResultView, ResultsPage, RunDetail, RunPage } from "../../api-checks";
import { useLoad, type Loaded } from "../../state";
import { Button, LoadError, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { Badge, NotReady } from "./shared";
import "./tests.css";

/** The Covered block: every field the run is bound to, as label and value, so the owner sees
 * exactly what a passing run proves. Input: the run's detail. Output: the block. */
export function CoveredBlock({ detail }: { detail: Pick<RunDetail, "source" | "policy"> }) {
  const rows = coveredRows(detail);
  return <section className="tt-block" aria-label="Covered">
    <h4>Covered</h4>
    <p className="pj-small pj-secondary">A passing run proves this exact source under this exact policy, nothing more.</p>
    <dl className="tt-covered">
      {rows.map((row) => <div key={row.field} className="tt-covered-row">
        <dt>{row.label}</dt>
        <dd className={row.field === "policy.argv" || row.field.startsWith("source.") || row.field.endsWith("digest") ? "pj-mono" : undefined}>{displayText(row.value)}</dd>
      </div>)}
    </dl>
  </section>;
}

/** The integrity block: what the engine saw about the tests themselves, warnings marked.
 * Input: the integrity, null when none was recorded or `cut` when too large. Output: the block. */
export function IntegrityBlock({ integrity }: { integrity: IntegrityRef | null | "cut" }) {
  const lines = integrityLines(integrity);
  return <section className="tt-block" aria-label="Integrity">
    <h4>Integrity</h4>
    <ul className="tt-lines">
      {lines.map((line, at) => <li key={at} className={line.warn ? "pj-note warn" : "pj-secondary"}>{displayText(line.text)}</li>)}
    </ul>
  </section>;
}

function ResultRow({ result }: { result: ResultView }) {
  const message = result.withheld === true ? "Message withheld: this case's details are kept from agents."
    : result.message === null ? null : displayText(result.message);
  return <li className="tt-result">
    <span className="pj-row between">
      <span className="pj-mono">{displayText(result.name)}</span>
      <span className="pj-small">{outcomeLabel(result.outcome)}</span>
    </span>
    <span className="pj-small pj-secondary">
      {result.case === null ? "Not linked to a case" : `Case ${result.case}`}
      {result.duration_ms === null ? "" : `, ${result.duration_ms} ms`}
    </span>
    {message !== null && <span className="pj-small tt-message">{message}</span>}
    {result.artifact !== null && <span className="pj-small pj-mono">{displayText(result.artifact)}</span>}
  </li>;
}

/** What a run's detail page shows and the actions it offers, without engine calls. */
export interface RunDetailViewProps {
  page: Loaded<RunPage>;
  more: ResultsPage[];
  busy: boolean;
  onMore: () => void;
  onBack: () => void;
  onRetry: () => void;
}

/** One run as a pure view of its read: its status and counts, the verdict's meaning, the
 * Covered and integrity blocks, its results page by page and the names no case is linked to.
 * Agent text shows hidden characters as escapes. Input: the view props. Output: the page. */
export function RunDetailView({ page, more, busy, onMore, onBack, onRetry }: RunDetailViewProps) {
  const back = <Button variant="quiet" size="small" onClick={onBack}>All runs</Button>;
  if (page.kind === "loading") return <div className="pj-stack">{back}<Skeleton shape="card" /></div>;
  if (page.kind === "not_ready") return <div className="pj-stack">{back}<NotReady what="test runs aren't in this engine yet." /></div>;
  if (page.kind === "error") return <div className="pj-stack">{back}<LoadError message={checksRefusal(page)} onRetry={onRetry} /></div>;
  const { detail } = page.value;
  const { run } = detail;
  const results = [page.value.results, ...more.map((later) => later.results)].flat();
  const cursor = more.length === 0 ? page.value.cursor : more[more.length - 1].cursor;
  const hidden = detail.unlinked_total - detail.unlinked.length;
  return <section className="pj-stack tt-run" aria-label={`Run ${run.id}`}>
    {back}
    <div className="pj-row between">
      <h3>{displayText(run.title)}</h3>
      <Badge word={runLabel(run)} tone={runTone(run)} />
    </div>
    <p className="pj-small pj-secondary">
      <span className="pj-mono">{run.id}</span>, {totalsLine(run.totals)}, started {whenText(run.started_ms)}
      {run.finished_ms === null ? "" : `, finished ${whenText(run.finished_ms)}`}, recorded by {displayText(run.recorded_by)}
    </p>
    {run.state === "finished" && run.verdict !== null && <p className="pj-secondary">{verdictText(run.verdict).line}</p>}
    {run.reason !== null && <p className="pj-note warn">{displayText(run.reason)}</p>}
    <CoveredBlock detail={detail} />
    <IntegrityBlock integrity={detail.integrity} />
    <section className="tt-block" aria-label="Results">
      <h4>Results</h4>
      {results.length === 0 ? <p className="pj-muted">This run recorded no results.</p>
        : <ul className="tt-results">{results.map((result) => <ResultRow key={result.position} result={result} />)}</ul>}
      {cursor !== null && <Button size="small" pending={busy} pendingLabel="Loading..." onClick={onMore}>Load more results</Button>}
    </section>
    {detail.unlinked_total > 0 && <section className="tt-block" aria-label="Unlinked names">
      <h4>Not linked to a case</h4>
      <ul className="tt-lines">{detail.unlinked.map((name, at) => <li key={at} className="pj-mono pj-small">{displayText(name)}</li>)}</ul>
      {hidden > 0 && <p className="pj-small pj-muted">{hidden === 1 ? "1 more unlinked name isn't listed." : `${hidden} more unlinked names aren't listed.`}</p>}
    </section>}
  </section>;
}

/** One run's detail page: reads its first page, then its later results on request. Inputs:
 * the project id, the run id and what Back does. Output: the page. */
export function RunDetailPanel({ projectId, runId, onBack }: { projectId: string; runId: string; onBack: () => void }) {
  const [page, reload] = useLoad(() => loadRun(projectId, runId), [projectId, runId]);
  const [more, setMore] = useState<ResultsPage[]>([]);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const loadMore = async () => {
    if (page.kind !== "ok") return;
    const cursor = more.length === 0 ? page.value.cursor : more[more.length - 1].cursor;
    if (cursor === null) return;
    setBusy(true);
    const next = await loadRunResults(projectId, runId, cursor);
    setBusy(false);
    if (next.kind === "ok") setMore((shown) => [...shown, next.value]);
    else setFailure(next.kind === "error" ? checksRefusal(next) : "");
  };
  return <>
    <RunDetailView page={page} more={more} busy={busy} onMore={() => void loadMore()} onBack={onBack} onRetry={() => { setMore([]); reload(); }} />
    {failure && <p className="pj-error" role="alert">Couldn't load more results. {failure}</p>}
  </>;
}
