import { useEffect, useState } from "react";
import {
  PROJECT_CONNECTORS, WHO_CHOICES, loadAllowedWrites, loadConnectors, notReadyLine, projectRows, revokeWrites, setConnectorGrant, switchWho, writeRows, writesRevoked,
  type AllowedWrites, type ConnectorsView, type ProjectRow, type Who,
} from "../../api-connectors";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, Segmented, Skeleton, Switch } from "../../ui";

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

/** The connectors this project may write with without asking again, each with Revoke, one
 * Revoke at a time. Allowed writes read for another project than the page show as loading.
 * Inputs: the page read, the allowed writes, the connector being revoked and the handlers.
 * Output: the block. */
function AllowedWritesBlock({ page, writes, revoking, onRevoke, onRetry }: {
  page: ConnectorsView;
  writes: Loaded<AllowedWrites>;
  revoking: string | null;
  onRevoke: (connector: string, title: string) => void;
  onRetry: () => void;
}) {
  const top = <><h3>{PROJECT_CONNECTORS.writes}</h3><p className="pj-secondary">{PROJECT_CONNECTORS.writesAbout}</p></>;
  if (writes.kind === "loading" || (writes.kind === "ok" && writes.value.project !== page.project)) return <div className="pj-stack-s">{top}<Skeleton shape="line" count={1} /></div>;
  if (writes.kind === "not_ready") return <div className="pj-stack-s">{top}<NotReady what={PROJECT_CONNECTORS.writes} /></div>;
  if (writes.kind === "error") return <div className="pj-stack-s">{top}<LoadError message={`${PROJECT_CONNECTORS.writesFailed} ${writes.message}`} onRetry={onRetry} /></div>;
  const rows = writeRows(page, writes.value);
  return <div className="pj-stack-s">
    {top}
    {rows.length === 0
      ? <p className="pj-muted">{PROJECT_CONNECTORS.writesNone}</p>
      : <ul className="pj-stack-s" aria-label={PROJECT_CONNECTORS.writes}>{rows.map((row) => <li key={row.name} data-write={row.name} className="pj-row">
        <span>{row.title}</span>
        <Button size="small" aria-label={`Revoke writes for ${row.title}`} disabled={revoking !== null}
          onClick={() => onRevoke(row.name, row.title)}>{PROJECT_CONNECTORS.revoke}</Button>
      </li>)}</ul>}
  </div>;
}

/** Project Settings, Connectors as a view of its state: the title in every state, then each
 * connector this project can switch (every Ready one and every one still On), one line
 * naming the rest, and while Write actions is Ask once per project and connector the
 * connectors allowed to write. Inputs: the page and allowed writes read, the connectors being
 * changed and revoked, the last problem and the handlers. Output: the section. */
export function ProjectConnectorsView({ view, writes, busy, revoking, problem, onChange, onRevoke, onRetry }: {
  view: Loaded<ConnectorsView>;
  writes: Loaded<AllowedWrites>;
  busy: string | null;
  revoking: string | null;
  problem: string;
  onChange: (connector: string, enabled: boolean, who: Who) => void;
  onRevoke: (connector: string, title: string) => void;
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
    {page.settings.write === "ask_once_per_project" && <AllowedWritesBlock page={page} writes={writes} revoking={revoking} onRevoke={onRevoke} onRetry={onRetry} />}
  </div>;
}

/** Project Settings, Connectors: reads this project's switches and allowed writes, turns a
 * connector on or off, or changes who it reaches, one change at a time, and revokes one
 * allowed write at a time, showing what the engine returns. A refused change or revoke
 * reads again. Input: the project id. Output: the section. */
export function ProjectConnectors({ projectId }: { projectId: string }) {
  const session = useAppSession();
  const [view, reload] = useLoad(() => loadConnectors(projectId), [projectId]);
  const [writes, reloadWrites] = useLoad(() => loadAllowedWrites(projectId), [projectId]);
  const [changed, setChanged] = useState<ConnectorsView | null>(null);
  const [revoked, setRevoked] = useState<AllowedWrites | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [problem, setProblem] = useState("");
  useEffect(() => { setChanged(null); }, [view]);
  useEffect(() => { setRevoked(null); }, [writes]);
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
  async function revoke(connector: string, title: string) {
    if (revoking !== null) return;
    setRevoking(connector);
    setProblem("");
    const result = await revokeWrites(projectId, connector);
    setRevoking(null);
    if (result.kind === "ok") {
      setRevoked(result.value);
      session.showToast(writesRevoked(title));
      return;
    }
    setProblem(`${PROJECT_CONNECTORS.notChanged} ${result.kind === "error" ? result.message : NOT_READY_ENGINE}`);
    reloadWrites();
  }
  const shown: Loaded<ConnectorsView> = changed !== null && changed.project === projectId ? { kind: "ok", value: changed } : view;
  const shownWrites: Loaded<AllowedWrites> = revoked !== null && revoked.project === projectId ? { kind: "ok", value: revoked } : writes;
  return <ProjectConnectorsView view={shown} writes={shownWrites} busy={busy} revoking={revoking} problem={problem}
    onChange={(connector, enabled, who) => void change(connector, enabled, who)} onRevoke={(connector, title) => void revoke(connector, title)}
    onRetry={() => { reload(); reloadWrites(); }} />;
}
