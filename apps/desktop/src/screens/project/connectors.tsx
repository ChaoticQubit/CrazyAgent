import { useEffect, useState } from "react";
import {
  PROJECT_CONNECTORS, WHO_CHOICES, loadConnectors, notReadyLine, projectRows, setConnectorGrant, switchWho,
  type ConnectorsView, type ProjectRow, type Who,
} from "../../api-connectors";
import { useLoad, type Loaded } from "../../state";
import { LoadError, NotReady, Notice, Segmented, Skeleton, Switch } from "../../ui";

const SAVING = "Saving...";
const NOT_READY_ENGINE = "The engine is not ready yet. Try again.";

const TOP = <>
  <h2>{PROJECT_CONNECTORS.title}</h2>
  <p className="pj-secondary">{PROJECT_CONNECTORS.purpose}</p>
</>;

/** One connector a project can switch: its On switch with the word On or Off, its state
 * word when it is not Ready, and while On and Ready who it reaches. Inputs: the row, the
 * Who a switch change sends, whether this row is saving and the change handler. Output:
 * the row. */
function ConnectorRow({ row, switchTo, saving, onChange }: {
  row: ProjectRow;
  switchTo: Who;
  saving: boolean;
  onChange: (connector: string, enabled: boolean, who: Who) => void;
}) {
  const on = row.grant?.enabled === true;
  const who = row.grant?.who ?? switchTo;
  return <li data-connector={row.name} className="pj-stack-s">
    <div className="pj-row">
      <Switch id={`pc-on-${row.name}`} label={row.title} checked={on} disabledReason={saving ? SAVING : undefined}
        onChecked={(next) => onChange(row.name, next, switchTo)} />
      <span>{on ? "On" : "Off"}</span>
      {!row.ready && <span className="pj-muted">{row.word}</span>}
    </div>
    {on && row.ready && <div className="pj-row">
      <span className="pj-secondary">{PROJECT_CONNECTORS.who}</span>
      <Segmented label={`Who can use ${row.title}`} value={who} options={WHO_CHOICES.map((option) => ({ value: option.choice, label: option.label }))}
        onValue={(choice) => { if (choice !== who) onChange(row.name, true, choice); }} />
    </div>}
  </li>;
}

/** Project Settings, Connectors as a view of its state: the title in every state, then each
 * connector this project can switch (every Ready one and every one still On), and one line
 * naming the rest. Inputs: the page read, the connector being changed, the last problem and
 * the handlers. Output: the section. */
export function ProjectConnectorsView({ view, busy, problem, onChange, onRetry }: {
  view: Loaded<ConnectorsView>;
  busy: string | null;
  problem: string;
  onChange: (connector: string, enabled: boolean, who: Who) => void;
  onRetry: () => void;
}) {
  if (view.kind === "loading") return <div className="pj-stack">{TOP}<Skeleton shape="line" count={4} /></div>;
  if (view.kind === "not_ready") return <div className="pj-stack">{TOP}<NotReady what={PROJECT_CONNECTORS.title} /></div>;
  if (view.kind === "error") return <div className="pj-stack">{TOP}<LoadError message={`Couldn't load the connectors. ${view.message}`} onRetry={onRetry} /></div>;
  const page = view.value;
  const { rows, others } = projectRows(page);
  const change = (connector: string, enabled: boolean, who: Who) => { if (busy === null) onChange(connector, enabled, who); };
  return <div className="pj-stack">
    {TOP}
    {problem && <Notice tone="danger" alert>{problem}</Notice>}
    {rows.length > 0 && <ul className="pj-stack-s" aria-label={PROJECT_CONNECTORS.title}>
      {rows.map((row) => <ConnectorRow key={row.name} row={row} switchTo={switchWho(page, row.name)} saving={busy === row.name} onChange={change} />)}
    </ul>}
    {others.length > 0 && <p className="pj-muted">{notReadyLine(others)}</p>}
    {rows.length === 0 && others.length === 0 && <p className="pj-muted">{PROJECT_CONNECTORS.none}</p>}
  </div>;
}

/** Project Settings, Connectors: reads this project's switches and turns a connector on or
 * off, or changes who it reaches, one change at a time, showing the page the engine returns.
 * A refused change reads the page again. Input: the project id. Output: the section. */
export function ProjectConnectors({ projectId }: { projectId: string }) {
  const [view, reload] = useLoad(() => loadConnectors(projectId), [projectId]);
  const [changed, setChanged] = useState<ConnectorsView | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState("");
  useEffect(() => { setChanged(null); }, [view]);
  async function change(connector: string, enabled: boolean, who: Who) {
    if (busy !== null) return;
    setBusy(connector);
    setProblem("");
    const result = await setConnectorGrant(projectId, connector, enabled, who);
    setBusy(null);
    if (result.kind === "ok") {
      setChanged(result.value);
      return;
    }
    setProblem(`${PROJECT_CONNECTORS.notChanged} ${result.kind === "error" ? result.message : NOT_READY_ENGINE}`);
    reload();
  }
  const shown: Loaded<ConnectorsView> = changed !== null && changed.project === projectId ? { kind: "ok", value: changed } : view;
  return <ProjectConnectorsView view={shown} busy={busy} problem={problem} onChange={(connector, enabled, who) => void change(connector, enabled, who)} onRetry={reload} />;
}
