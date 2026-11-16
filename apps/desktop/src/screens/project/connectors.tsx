import type { ConnectorsView, Who } from "../../api-connectors";
import type { Loaded } from "../../state";

/** Project Settings, Connectors as a view of its state: each connector this project can
 * switch, with its On switch and, while On, who it reaches. Inputs: the page read, the
 * connector being changed, the last problem and the handlers. Output: the section. */
export function ProjectConnectorsView(_props: {
  view: Loaded<ConnectorsView>;
  busy: string | null;
  problem: string;
  onChange: (connector: string, enabled: boolean, who: Who) => void;
  onRetry: () => void;
}) {
  return null;
}
