import type { ConnectorView } from "../../api-connectors";
import type { ConnectorActions } from "./connectors";

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
  void name;
  void connector;
  void busy;
  void failure;
  void actions;
  return null;
}
