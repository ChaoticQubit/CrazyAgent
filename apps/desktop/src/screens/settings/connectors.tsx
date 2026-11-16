import type { RefObject } from "react";
import { CONNECTORS_PAGE, type ConnectorView, type ConnectorsView, type ConnectorTrustView, type NativeConnectors, type SizeCap } from "../../api-connectors";
import type { Problem } from "../../api-app-plugins";
import type { Loaded } from "../../state";

/** What Settings, Connectors has open over the page: nothing, the paste box, a trust screen
 * (for a new manifest, or a review of a listed file), a refused manifest, the key entry for
 * one secret, or the remove confirm. */
export type ConnectorDialog =
  | { kind: "none" }
  | { kind: "add"; yaml: string }
  | { kind: "trust"; yaml: string | null; trust: ConnectorTrustView; review: boolean }
  | { kind: "refused"; yaml: string | null; problem: Problem }
  | { kind: "key"; connector: string; title: string; hosts: string[]; secret: string }
  | { kind: "remove"; connector: string; title: string };

/** Everything the Connectors views ask their container to do. */
export interface ConnectorActions {
  onAdd: () => void;
  onYaml: (yaml: string) => void;
  onPick: () => void;
  onCheck: () => void;
  onConfirm: () => void;
  onEdit: () => void;
  onClose: () => void;
  onReview: (name: string) => void;
  onRemove: (connector: ConnectorView) => void;
  onConfirmRemove: () => void;
  onSetKey: (connector: ConnectorView, secret: string) => void;
  onSaveKey: () => void;
  onClearKey: (name: string, secret: string) => void;
  onPickNative: (native: NativeConnectors) => void;
  onPickSize: (sizeCap: SizeCap) => void;
  onSave: () => void;
  onRetry: () => void;
}

const TOP = <div className="settings-top"><div>
  <h2>{CONNECTORS_PAGE.title}</h2>
  <p>{CONNECTORS_PAGE.purpose}</p>
</div></div>;

/** Settings, Connectors as a view of its state: the title in every state, then the catalog
 * with each connector's state, hosts and page, Add connector, and the connector settings
 * with the locked line; or the open connector's own page; over either the open dialog.
 * Inputs: the page read, the connector the route opened, the open dialog, the picked
 * settings, busy and saving flags, the last failure and save problem, the key field and
 * the actions. Output: the tab. */
export function ConnectorsSettingsView({ view, open, dialog, native, sizeCap, busy, saving, failure, problem, keyRef, actions }: {
  view: Loaded<ConnectorsView>;
  open: string | null;
  dialog: ConnectorDialog;
  native: NativeConnectors | null;
  sizeCap: SizeCap | null;
  busy: boolean;
  saving: boolean;
  failure: string;
  problem: string;
  keyRef?: RefObject<HTMLInputElement | null>;
  actions: ConnectorActions;
}) {
  void view;
  void open;
  void dialog;
  void native;
  void sizeCap;
  void busy;
  void saving;
  void failure;
  void problem;
  void keyRef;
  void actions;
  return <>{TOP}</>;
}

/** Settings, Connectors: reads the page, adds a pasted or picked manifest only after its
 * trust screen is confirmed, reviews a changed file, removes the owner's own, sets and
 * clears each secret's key, and saves the two connector settings at the version read.
 * Input: the connector whose page the route opened, if any. Output: the tab. */
export function ConnectorsSettings({ connector }: { connector?: string }) {
  void connector;
  return <div className="stack">{TOP}</div>;
}
