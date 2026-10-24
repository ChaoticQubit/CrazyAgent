import { useEffect, useState } from "react";
import { nextGeneration } from "../../api";
import { intakeCopy, loadSetupStatus, runSetup } from "../../api-intake";
import { receiptState } from "../../api-projects";
import type { PackEntry, SetupStatus } from "../../engine-client";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, Skeleton, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { NotReady } from "../project/shared";
import { SETUP_POLL_MS, approvalLine, dependencyView, entryName, packStateView, packTicks, setupBusy } from "./intake";
import "../project/projects.css";
import "./intake.css";

function PackGroup({ title, entries, ticks, onTick, locked }: {
  title: string; entries: PackEntry[]; ticks: string[]; onTick: (id: string, on: boolean) => void; locked: boolean;
}) {
  if (entries.length === 0) return null;
  return <fieldset className="pj-field">
    <legend>{title}</legend>
    <ul className="in-list">{entries.map((entry) => {
      const state = packStateView(entry.state);
      return <li className="in-row" key={entry.id}>
        <span className="in-row-main">
          <Checkbox label={entryName(entry)} checked={ticks.includes(entry.id)} disabled={locked} onChecked={(on) => onTick(entry.id, on)} />
          <span className="pj-small pj-muted">{displayText(entry.source)}, {displayText(entry.license)}{entry.on_by_default ? "" : ", off by default"}</span>
          {state.line && entry.state.kind === "failed" && <span className="pj-small pj-error">{state.line}</span>}
        </span>
        <StatusBadge word={state.word} tone={state.tone} />
      </li>;
    })}</ul>
  </fieldset>;
}

/** The tools step as a view of its state: tmux, uv and graphify with their states, the default
 * pack as ticks grouped into skills and MCP servers, and the sentence naming the MCP servers that
 * continuing approves. Inputs: the setup read, the ticked ids and their handler, flags, the last
 * problem and the Run and Continue handlers. Output: the step. */
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
  const approval = approvalLine(pack, ticks);
  const installing = setupBusy(status.value);
  return <div className="pj pj-stack">
    <section className="pj-stack-s" aria-label="Tools">
      <h2>Tools</h2>
      <ul className="in-list">{dependencies.map((dependency) => {
        const row = dependencyView(dependency);
        return <li className="in-row" key={dependency.name}>
          <span className="in-row-main"><strong>{row.title}</strong><span className="pj-small pj-secondary">{row.about}</span>
            {row.line && <span className={`pj-small ${dependency.state.kind === "failed" ? "pj-error" : "pj-muted"}`}>{row.line}</span>}</span>
          <StatusBadge word={row.word} tone={row.tone} />
        </li>;
      })}</ul>
    </section>
    <section className="pj-stack-s" aria-label="Default pack">
      <h2>Default pack</h2>
      <p className="pj-secondary">Skills and plugins every project starts with. Untick anything you don't want; you can change this later.</p>
      <PackGroup title="Skills" entries={pack.filter((entry) => entry.kind === "skill")} ticks={ticks} onTick={onTick} locked={started} />
      <PackGroup title="MCP servers" entries={pack.filter((entry) => entry.kind === "mcp_server")} ticks={ticks} onTick={onTick} locked={started} />
    </section>
    {approval && <p className="pj-note">{approval}</p>}
    {started && <p className="pj-secondary" role="status">{installing ? "Installing. Setup keeps going while you continue." : "Setup finished. Anything that failed shows its reason above."}</p>}
    {problem && <p className="pj-error" role="alert">{problem}</p>}
    <span className="pj-row">
      {started ? <Button variant="primary" onClick={onContinue}>Continue</Button>
        : <Button variant="primary" pending={busy} pendingLabel="Starting..." onClick={onRun}>Set up tools</Button>}
      {!started && <Button variant="quiet" disabled={busy} onClick={onContinue}>Skip for now</Button>}
    </span>
  </div>;
}

/** The tools step of first run: reads setup, installs missing tools and the ticked pack, and reads
 * again every few seconds while anything installs. Input: what to do on Continue. */
export function ToolsSetup({ onContinue }: { onContinue: () => void }) {
  const [status, reload] = useLoad(loadSetupStatus);
  const [ticks, setTicks] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [started, setStarted] = useState(false);
  const [problem, setProblem] = useState("");
  const value = status.kind === "ok" ? status.value : null;
  const installing = value !== null && setupBusy(value);

  useEffect(() => {
    if (ticks === null && value !== null) setTicks(packTicks(value.pack));
  }, [value, ticks]);

  useEffect(() => {
    if (!started || !installing) return;
    const timer = window.setTimeout(reload, SETUP_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [started, installing, status]);

  const run = async () => {
    if (busy || ticks === null) return;
    setBusy(true);
    setProblem("");
    const result = await runSetup(ticks, nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") { setProblem(result.kind === "not_ready" ? "Installing tools isn't in this build." : intakeCopy(result.code)); return; }
    const state = receiptState(result.value);
    if (state.kind === "rejected") { setProblem(intakeCopy(state.code)); return; }
    if (state.kind === "unconfirmed") { setProblem("We couldn't confirm setup started. Check again in a moment."); }
    setStarted(true);
    reload();
  };

  const tick = (id: string, on: boolean) => setTicks((old) => {
    const list = old ?? [];
    return on ? (list.includes(id) ? list : [...list, id]) : list.filter((item) => item !== id);
  });

  return <ToolsSetupView status={status} ticks={ticks ?? []} onTick={tick} busy={busy} started={started} problem={problem} onRun={() => void run()} onContinue={onContinue} />;
}
