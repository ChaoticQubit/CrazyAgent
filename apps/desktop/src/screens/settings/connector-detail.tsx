import { CONNECTORS_PAGE, connectorState, originLine, reachesLine, type ConnectorView } from "../../api-connectors";
import { Button, Notice, StatusBadge, formatRoute } from "../../ui";
import { secretLine } from "./app-plugins";
import type { ConnectorActions } from "./connectors";

const BACK = formatRoute({ name: "settings", tab: "connectors" });

/** A connector's own page in Settings, Connectors: the way back, its title, where it came
 * from, its state, every host it reaches, each secret with Set key or Clear key, Review and
 * Remove. Inputs: the name the route opened, the connector when listed, a busy flag, the
 * last failure and the actions. Output: the page. */
export function ConnectorDetailView({ name, connector, busy, failure, actions }: {
  name: string;
  connector: ConnectorView | null;
  busy: boolean;
  failure: string;
  actions: ConnectorActions;
}) {
  const back = <a href={BACK}>{CONNECTORS_PAGE.back}</a>;
  if (connector === null) return <div className="pj-stack">{back}<p>{`No connector named ${name}.`}</p></div>;
  const state = connectorState(connector);
  const keys = connector.sign_in === "key";
  return <div className="pj-stack">
    {back}
    <div className="settings-top"><div>
      <h2>{connector.title}</h2>
      <p className="muted small">{originLine(connector)}</p>
    </div></div>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    <div className="pj-stack-s">
      {connector.description && <p className="secondary">{connector.description}</p>}
      <div className="pj-row"><StatusBadge word={state.word} tone={state.tone} />{state.detail && <span className="small">{state.detail}</span>}</div>
      {connector.runs && <p className="small">{reachesLine(connector.runs.hosts)}</p>}
      {connector.runs && <p className="small">{`Address: ${connector.runs.url}`}</p>}
      {keys && connector.secrets.length > 0 && <ul className="pj-rows" aria-label={`Secrets of ${connector.title}`}>{connector.secrets.map((secret) => <li key={secret.name} className="pj-row between">
        <span>{secretLine(secret)}</span>
        {secret.binding.kind === "none"
          ? <Button size="small" aria-label={`Set key for ${secret.name} of ${connector.title}`} disabled={busy} onClick={() => actions.onSetKey(connector, secret.name)}>Set key</Button>
          : <Button size="small" variant="quiet" aria-label={`Clear key for ${secret.name} of ${connector.title}`} disabled={busy} onClick={() => actions.onClearKey(connector.name, secret.name)}>Clear key</Button>}
      </li>)}</ul>}
      {(connector.status.kind === "needs_review" || connector.origin === "user") && <div className="pj-row">
        {connector.status.kind === "needs_review" && <Button size="small" aria-label={`Review ${connector.title}`} disabled={busy} onClick={() => actions.onReview(connector.name)}>Review</Button>}
        {connector.origin === "user" && <Button size="small" variant="danger-text" aria-label={`Remove ${connector.title}`} disabled={busy} onClick={() => actions.onRemove(connector)}>Remove</Button>}
      </div>}
    </div>
  </div>;
}
