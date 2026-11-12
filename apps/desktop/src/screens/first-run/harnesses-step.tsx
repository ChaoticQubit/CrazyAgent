import { useEffect, useState } from "react";
import { harnessCatalog, type Result } from "../../api";
import { harnessVersions, installHarnesses, queuedHarnesses, updateCopy, UPDATE_POLL_MS, UPDATE_POLLS_MAX } from "../../api-harness-updates";
import { installDependency, loadSetupStatus } from "../../api-intake";
import { openLink } from "../../api-projects";
import { Button, StatusBadge } from "../../ui";
import { HARNESS_WORDS, OFFERED, harnessRow, installable, nodeNeeded, nodeOffer, progress, progressLine, retry, rowBadge, rowLine, tickable, type HarnessRow, type NodeOffer } from "./harnesses";
import "../intake/intake.css";

type Rows = Readonly<Record<string, HarnessRow>>;

/** The Harnesses step as a view of its state: one row per offered harness with its tick, the
 * line saying what installing it runs or why it can't, Try again on a failed, refused or unrunnable
 * row, Install Node.js while a harness needs Node, the progress line once installs start, and
 * Install, Skip or Continue. Inputs: the rows by id, picks, flags, the problem, the Node offer and
 * the handlers. Output: the step. */
export function HarnessesStepView({ rows, picks, started, busy, problem, node = { kind: "none" }, onPick, onInstall, onRetry, onContinue, onGuide, onInstallNode = () => undefined }: {
  rows: Rows;
  picks: readonly string[];
  started: boolean;
  busy: boolean;
  problem: string;
  node?: NodeOffer;
  onPick: (id: string, on: boolean) => void;
  onInstall: () => void;
  onRetry: (id: string) => void;
  onContinue: () => void;
  onGuide?: (url: string) => void;
  onInstallNode?: () => void;
}) {
  const sending = installable(picks);
  const counts = progress(sending, rows);
  const offer = sending.length > 0 && !started;
  return <div className="pj pj-stack">
    <ul className="in-list" aria-label="Harnesses">{OFFERED.map(({ id, name }) => {
      const row = rows[id] ?? { kind: "reading" };
      const locked = row.kind === "bundled";
      const again = retry(row);
      const badge = rowBadge(row);
      const guide = row.kind === "refused" ? row.guide : null;
      return <li className="in-row" key={id}>
        <span className="in-row-main">
          <label className="checkbox">
            <input type="checkbox" aria-label={name} checked={locked || picks.includes(id)} disabled={locked || started || busy || !tickable(row)}
              onChange={(event) => onPick(id, event.target.checked)} />
            <span>{name}</span>
          </label>
          <span className={`pj-small ${row.kind === "failed" || row.kind === "refused" || row.kind === "blocked" ? "pj-error" : "pj-secondary"}`}>{rowLine(row)}</span>
          {guide !== null && <span className="pj-small"><a href={guide} onClick={(event) => { event.preventDefault(); onGuide?.(guide); }}>{HARNESS_WORDS.guide}</a></span>}
          {again !== null && <span><Button aria-label={`${HARNESS_WORDS.tryAgain}: ${name}`} disabled={busy} onClick={() => onRetry(id)}>{HARNESS_WORDS.tryAgain}</Button></span>}
        </span>
        <StatusBadge word={badge.word} tone={badge.tone} />
      </li>;
    })}</ul>
    {node.kind !== "none" && <span className="pj-row">
      {node.kind === "installing" && <span className="pj-small pj-secondary" role="status">{HARNESS_WORDS.installingNode}</span>}
      {node.kind === "failed" && <span className="pj-small pj-error">{node.reason}</span>}
      {node.kind !== "installing" && <Button icon="wrench" disabled={busy} onClick={onInstallNode}>{HARNESS_WORDS.installNode}</Button>}
    </span>}
    {started && counts.total > 0 && <p className="pj-secondary" role="status">{progressLine(counts)}</p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span className="pj-row">
      {offer ? <Button variant="primary" icon="wrench" pending={busy} pendingLabel="Starting..." onClick={onInstall}>{HARNESS_WORDS.install}</Button>
        : <Button variant="primary" icon="arrow-right" disabled={busy} onClick={onContinue}>{HARNESS_WORDS.next}</Button>}
      {offer && <Button variant="quiet" disabled={busy} onClick={onContinue}>{HARNESS_WORDS.skip}</Button>}
    </span>
  </div>;
}

/** Reads every offered harness: the catalog row, the versions read and the install queue, so a
 * failed read becomes a refused row with Try again instead of a row that never loads, and setup's
 * Node row for the Node.js offer. Output: the rows by id, the Node offer and the problem line,
 * empty when every read worked. */
async function readRows(): Promise<{ rows: Record<string, HarnessRow>; node: NodeOffer; problem: string }> {
  const ids = OFFERED.map(({ id }) => id).filter((id) => id !== "rcli");
  const [catalog, queued, setup, ...reads] = await Promise.all([harnessCatalog(false), queuedHarnesses(), loadSetupStatus(), ...ids.map((id) => harnessVersions(id, false))]);
  assert(reads.length === ids.length, "one versions read per offered harness");
  const entries = catalog.kind === "ok" ? catalog.value : [];
  const waiting = queued.kind === "ok" ? queued.value : [];
  const rows: Record<string, HarnessRow> = { rcli: harnessRow("rcli", undefined, null, waiting) };
  ids.forEach((id, index) => {
    const read = reads[index];
    rows[id] = read.kind === "ok" ? harnessRow(id, entries.find((entry) => entry.id === id), read.value, waiting)
      : { kind: "refused", reason: failure(read), guide: null };
  });
  const problem = catalog.kind !== "ok" ? failure(catalog) : queued.kind !== "ok" ? failure(queued) : "";
  const found = setup.kind === "ok" ? setup.value.dependencies.find((item) => item.name === "node") : undefined;
  const needed = nodeNeeded(reads.map((read) => read.kind === "ok" ? read.value : null));
  return { rows, node: nodeOffer(needed, found?.state ?? null), problem };
}

function failure(result: Exclude<Result<unknown>, { kind: "ok" }>): string {
  return result.kind === "not_ready" ? "Installing harnesses isn't in this build." : result.message;
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The Harnesses step: reads the offered harnesses, installs the picked ones in the engine one
 * after another and Node.js alone when a harness needs it, reads again a few seconds after each
 * read lands while any install waits or runs, and lets the owner continue at any time while the
 * engine keeps installing. Input: what to do on Continue. */
export function HarnessesStep({ onContinue }: { onContinue: () => void }) {
  const [rows, setRows] = useState<Rows>({});
  const [picks, setPicks] = useState<string[]>([]);
  const [started, setStarted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [node, setNode] = useState<NodeOffer>({ kind: "none" });
  const [reads, setReads] = useState(0);
  const [reading, setReading] = useState(false);
  const [polls, setPolls] = useState(0);
  const pending = Object.values(rows).some((row) => row.kind === "waiting" || row.kind === "installing") || (started && progress(installable(picks), rows).running)
    || node.kind === "installing";

  useEffect(() => {
    let live = true;
    setReading(true);
    void readRows().then((read) => {
      if (!live) return;
      setRows(read.rows);
      setNode(read.node);
      if (read.problem) setProblem(read.problem);
      setReading(false);
    });
    return () => { live = false; };
  }, [reads]);

  useEffect(() => {
    if (!pending || reading || polls >= UPDATE_POLLS_MAX) return;
    const timer = window.setTimeout(() => {
      setPolls((count) => count + 1);
      setReads((count) => count + 1);
    }, UPDATE_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [pending, reading, polls]);

  const send = async (ids: string[]) => {
    if (busy || ids.length === 0) return;
    setBusy(true);
    setProblem("");
    const result = await installHarnesses(ids);
    setBusy(false);
    if (result.kind !== "ok") { setProblem(result.kind === "error" && result.code.startsWith("harness_") ? updateCopy(result.code) : failure(result)); return; }
    setStarted(true);
    setPicks((old) => [...old, ...ids.filter((id) => !old.includes(id))]);
    setPolls(0);
    setReads((count) => count + 1);
  };

  const installNode = async () => {
    if (busy) return;
    setBusy(true);
    setProblem("");
    const result = await installDependency("node");
    setBusy(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Installing tools isn't in this build." : result.message); return; }
    setNode({ kind: "installing" });
    setPolls(0);
    setReads((count) => count + 1);
  };

  const again = (id: string) => {
    const row = rows[id];
    if (row === undefined) return;
    if (retry(row) === "install") void send([id]);
    else { setProblem(""); setReads((count) => count + 1); }
  };

  return <HarnessesStepView rows={rows} picks={picks} started={started} busy={busy} problem={problem} node={node}
    onInstallNode={() => void installNode()} onPick={(id, on) => setPicks((old) => on ? [...old.filter((item) => item !== id), id] : old.filter((item) => item !== id))}
    onInstall={() => void send(installable(picks))} onRetry={again} onContinue={onContinue} onGuide={(url) => void openLink(url)} />;
}
