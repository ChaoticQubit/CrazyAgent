import { useEffect, useState } from "react";
import { intakeCopy, loadSetupStatus, runSetup } from "../../api-intake";
import type { PackEntry, SetupStatus } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, Icon, Skeleton, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { NotReady } from "../project/shared";
import { SETUP_POLL_MS, approvalLine, dependencyView, entryIcon, entryName, installed, packGroups, packStateView, packTicks, setupBusy, setupProgress, toolsChecking } from "./intake";
import "../project/projects.css";
import "./intake.css";

function PackRow({ entry, ticked, onTick, locked }: { entry: PackEntry; ticked: boolean; onTick: (id: string, on: boolean) => void; locked: boolean }) {
  const state = packStateView(entry.state);
  const name = <span className="row"><Icon name={entryIcon(entry)} size={14} />{entryName(entry)}{entry.kind === "mcp_server" && <span className="pj-small pj-muted">MCP server</span>}</span>;
  return <li className="in-row">
    <span className="in-row-main">
      {installed(entry) ? <>{name}<span className="pj-small pj-secondary">Already installed. Switch it on or off in Settings, Pack.</span></>
        : <Checkbox label={name} checked={ticked} disabled={locked} onChecked={(on) => onTick(entry.id, on)} />}
      <span className="pj-small pj-muted">{displayText(entry.source)}, {displayText(entry.license)}{entry.on_by_default ? "" : ", off by default"}</span>
      {state.line && <span className="pj-small pj-error">{state.line}</span>}
    </span>
    <StatusBadge word={state.word} tone={state.tone} />
  </li>;
}

/** The desktop setup screen as a view of its state: tmux, uv and graphify with their state and
 * version, the default pack as ticks grouped like the manifest, the sentence naming the ticked MCP
 * servers that continuing approves, and progress while setup installs. Inputs: the setup read, the
 * ticked ids and their handler, flags, the last problem and the Run and Continue handlers. */
export function ToolsSetupView({ status, ticks, onTick, busy, started, problem, onRun, onContinue }: {
  status: Loaded<SetupStatus>;
  ticks: string[];
  onTick: (id: string, on: boolean) => void;
  busy: boolean;
  started: boolean;
  problem: string;
  onRun: () => void;
  onContinue: () => void;
}) {
  if (status.kind === "loading") return <Skeleton shape="row" count={4} />;
  if (status.kind === "not_ready") return <div className="pj-stack-s">
    <NotReady what="tool setup isn't in this build." />
    <span><Button variant="primary" onClick={onContinue}>Continue</Button></span>
  </div>;
  if (status.kind === "error") return <div className="pj-stack-s">
    <p className="pj-error" role="alert">{status.message}</p>
    <span><Button onClick={onContinue}>Continue</Button></span>
  </div>;
  const { dependencies, pack } = status.value;
  const running = setupBusy(status.value);
  const locked = started || running;
  const approval = approvalLine(pack, ticks);
  const progress = setupProgress(status.value, ticks);
  return <div className="pj pj-stack">
    <section className="pj-stack-s" aria-label="Tools">
      <h2>Tools</h2>
      <ul className="in-list">{dependencies.map((dependency) => {
        const row = dependencyView(dependency);
        return <li className="in-row" key={dependency.name}>
          <span className="in-row-main"><span className="pj-row"><Icon name="wrench" size={14} /><strong>{row.title}</strong></span><span className="pj-small pj-secondary">{row.about}</span>
            {row.line && <span className={`pj-small ${dependency.state.state === "failed" ? "pj-error" : "pj-muted"}`}>{row.line}</span>}</span>
          <StatusBadge word={row.word} tone={row.tone} />
        </li>;
      })}</ul>
    </section>
    <section className="pj-stack-s" aria-label="Default pack">
      <h2>Default pack</h2>
      <p className="pj-secondary">Skills and plugins every project starts with, each from a pinned source. Untick anything you don't want; you can change this later in Settings, Pack.</p>
      {packGroups(pack).map((group) => <fieldset className="pj-field" key={group.title}>
        <legend>{group.title}</legend>
        {group.about && <p className="pj-help">{group.about}</p>}
        <ul className="in-list">{group.entries.map((entry) => <PackRow key={entry.id} entry={entry} ticked={ticks.includes(entry.id)} onTick={onTick} locked={locked} />)}</ul>
      </fieldset>)}
    </section>
    {approval && <p className="pj-note">{approval}</p>}
    {(started || running) && <p className="pj-secondary" role="status">
      {running ? `Installing: ${progress.done} of ${progress.total} done. You can continue; setup keeps going.`
        : `Setup finished: ${progress.done} of ${progress.total} done${progress.failed ? `, ${progress.failed} failed. Each failure shows its reason above` : ""}.`}
    </p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span className="pj-row">
      {started || running ? <Button variant="primary" icon="arrow-right" onClick={onContinue}>Continue</Button>
        : <Button variant="primary" icon="wrench" pending={busy} pendingLabel="Starting..." onClick={onRun}>Set up tools</Button>}
      {!started && !running && <Button variant="quiet" disabled={busy} onClick={onContinue}>Skip for now</Button>}
    </span>
  </div>;
}

/** The desktop setup screen: reads setup, installs missing tools and the ticked pack, and reads
 * again every few seconds while setup runs. Input: what to do on Continue. */
export function ToolsSetup({ onContinue }: { onContinue: () => void }) {
  const [loaded, reload] = useLoad(loadSetupStatus, [], "setup-status");
  const [fresh, setFresh] = useState<SetupStatus | null>(null);
  const [ticks, setTicks] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [started, setStarted] = useState(false);
  const [problem, setProblem] = useState("");
  const status: Loaded<SetupStatus> = fresh ? { kind: "ok", value: fresh } : loaded;
  const value = status.kind === "ok" ? status.value : null;
  const running = value !== null && setupBusy(value);
  const checking = value !== null && toolsChecking(value);

  useEffect(() => {
    if (loaded.kind === "ok") setFresh(loaded.value);
  }, [loaded]);

  useEffect(() => {
    if (ticks === null && value !== null) setTicks(packTicks(value.pack));
  }, [value, ticks]);

  useEffect(() => {
    if (!running && !checking) return;
    const timer = window.setTimeout(reload, SETUP_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [running, checking, value]);

  const run = async () => {
    if (busy || ticks === null) return;
    setBusy(true);
    setProblem("");
    const result = await runSetup(ticks);
    setBusy(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Installing tools isn't in this build." : intakeCopy(result)); return; }
    setFresh(result.value);
    setStarted(true);
  };

  const tick = (id: string, on: boolean) => setTicks((old) => {
    const list = old ?? [];
    return on ? (list.includes(id) ? list : [...list, id]) : list.filter((item) => item !== id);
  });

  return <ToolsSetupView status={status} ticks={ticks ?? []} onTick={tick} busy={busy} started={started} problem={problem} onRun={() => void run()} onContinue={onContinue} />;
}
