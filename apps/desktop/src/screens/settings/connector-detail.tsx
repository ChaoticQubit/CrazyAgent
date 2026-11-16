import { CONNECTORS_PAGE, SIGN_IN_WITH_CHOICES, browserNow, chosenWay, connectorState, originLine, reachesLine, secretsShown, signedIn, type ConnectorView } from "../../api-connectors";
import { Button, Notice, StatusBadge, formatRoute } from "../../ui";
import { secretLine } from "./app-plugins";
import type { ConnectorActions } from "./connectors";

const BACK = formatRoute({ name: "settings", tab: "connectors" });

/** A connector's own page: the way back, title, origin, state, hosts, Sign in with, Sign in,
 * Waiting for the browser with Cancel, or Sign out, its keys, Review and Remove; a refused
 * file shows only its reason. Inputs: the name, the connector when listed, a busy flag,
 * whether its sign-in waits for the browser, the last failure and the actions. Output: the page. */
export function ConnectorDetailView({ name, connector, busy, waiting, failure, actions }: {
  name: string;
  connector: ConnectorView | null;
  busy: boolean;
  waiting: boolean;
  failure: string;
  actions: ConnectorActions;
}) {
  const back = <a href={BACK}>{CONNECTORS_PAGE.back}</a>;
  if (connector === null) return <div className="pj-stack">{back}<p>{`No connector named ${name}.`}</p></div>;
  const state = connectorState(connector);
  const secrets = secretsShown(connector);
  const way = chosenWay(connector);
  const refused = connector.status.kind === "refused";
  const runs = connector.runs;
  const signed = !refused && signedIn(connector);
  const offered = !refused && !signed && browserNow(connector);
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
      {refused && <p className="small">{CONNECTORS_PAGE.refusedFile}</p>}
      {runs && <p className="small">{reachesLine(runs.hosts)}</p>}
      {runs && <p className="small">{`Address: ${runs.url}`}</p>}
      {way !== null && <fieldset className="notice-choices" disabled={busy} data-setting="sign_in_with">
        <legend>{CONNECTORS_PAGE.signInWith}</legend>
        {SIGN_IN_WITH_CHOICES.map((option) => <label key={option.choice} className={option.choice === way ? "on" : undefined}>
          <input type="radio" name="connector-sign-in-with" value={option.choice} checked={option.choice === way} onChange={() => actions.onSignInWith(connector, option.choice)} />
          {option.label}
        </label>)}
      </fieldset>}
      {way !== null && <p className="small muted">{CONNECTORS_PAGE.signInWithHelp}</p>}
      {signed && <div className="pj-row" data-sign-in="signed_in">
        <Button size="small" variant="quiet" aria-label={`Sign out of ${connector.title}`} disabled={busy} onClick={() => actions.onSignOut(connector)}>{CONNECTORS_PAGE.signOut}</Button>
      </div>}
      {signed && <p className="small muted">{CONNECTORS_PAGE.signOutHelp}</p>}
      {offered && waiting && <div className="pj-row" data-sign-in="waiting">
        <span role="status">{CONNECTORS_PAGE.waiting}</span>
        <Button size="small" aria-label={`Cancel the sign-in to ${connector.title}`} onClick={() => actions.onCancelSignIn(connector)}>Cancel</Button>
      </div>}
      {offered && !waiting && <div className="pj-row" data-sign-in="offered">
        <Button size="small" variant="primary" aria-label={`Sign in to ${connector.title}`} disabled={busy} onClick={() => actions.onSignIn(connector)}>{CONNECTORS_PAGE.signIn}</Button>
      </div>}
      {secrets.length > 0 && <ul className="pj-rows" aria-label={`Secrets of ${connector.title}`}>{secrets.map((secret) => <li key={secret.name} className="pj-row between">
        <span>{secretLine(secret)}</span>
        {secret.binding.kind === "none"
          ? <Button size="small" aria-label={`Set key for ${secret.name} of ${connector.title}`} disabled={busy} onClick={() => actions.onSetKey(connector, secret.name)}>Set key</Button>
          : <Button size="small" variant="quiet" aria-label={`Clear key for ${secret.name} of ${connector.title}`} disabled={busy} onClick={() => actions.onClearKey(connector.name, secret.name)}>Clear key</Button>}
      </li>)}</ul>}
      {!refused && (connector.status.kind === "needs_review" || connector.origin === "user") && <div className="pj-row">
        {connector.status.kind === "needs_review" && <Button size="small" aria-label={`Review ${connector.title}`} disabled={busy} onClick={() => actions.onReview(connector.name)}>Review</Button>}
        {connector.origin === "user" && <Button size="small" variant="danger-text" aria-label={`Remove ${connector.title}`} disabled={busy} onClick={() => actions.onRemove(connector)}>Remove</Button>}
      </div>}
    </div>
  </div>;
}
