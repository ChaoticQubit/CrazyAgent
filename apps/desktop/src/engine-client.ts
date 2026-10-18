import { invoke } from "@tauri-apps/api/core";

export interface ConnectionStatus {
  connected: boolean;
  pending: boolean;
}

export interface BaselineRow {
  id: string;
  project_id: string;
  approved: boolean;
  revoked: boolean;
  current: boolean;
}

export interface BaselinePage {
  items: BaselineRow[];
  has_more: boolean;
  fresh: boolean;
}

export interface BaselineDetail extends BaselineRow {
  ticket: string;
  origin: string;
  requirements: string;
  checks: {
    id: string;
    kind: string;
    minimum_risk: string;
    content: string;
    encoding: "utf8" | "hex";
  }[];
  rules: { path_prefix: string; minimum_risk: string }[];
  retry_limit: number;
  fresh: boolean;
}

export interface ActionReceipt {
  baseline_id: string;
  outcome: "applied" | "rejected" | "unavailable" | "unknown" | "not_recorded_at_barrier" | "uncertain";
  pending: boolean;
}

export function connect(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("desktop_connect");
}

export function status(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("desktop_status");
}

export function baselines(next = false): Promise<BaselinePage> {
  return invoke<BaselinePage>("desktop_baselines", { next });
}

export function inspectBaseline(id: string): Promise<BaselineDetail> {
  return invoke<BaselineDetail>("desktop_inspect_baseline", { id });
}

export function approveBaseline(ticket: string): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_approve_baseline", { ticket });
}

export function revokeBaseline(ticket: string): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_revoke_baseline", { ticket });
}

export function reconcile(): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_reconcile");
}

export function retry(): Promise<ActionReceipt> {
  return invoke<ActionReceipt>("desktop_retry");
}

export function errorMessage(error: unknown): string {
  const code = typeof error === "object" && error !== null && "code" in error
    ? String(error.code) : "unknown";
  const messages: Record<string, string> = {
    not_configured: "No engine connection was configured for this desktop session.",
    not_connected: "Connect to the engine before reading owner state.",
    busy: "Another owner request is still running. Wait for it to finish.",
    owner_unavailable: "The configured engine could not be authenticated. Check the engine and reconnect.",
    owner_reconnect_failed: "Reconnection was refused. Any pending owner intent remains retained.",
    owner_read_failed: "The owner read failed. Displayed data is stale; reconnect or refresh.",
    resync_required: "The engine requires a fresh snapshot. Refresh the first page.",
    no_next_page: "There is no next page. Refresh to read current state.",
    projection_limit: "This response exceeds the desktop display limit.",
    invalid_baseline: "The selected baseline identity is invalid.",
    stale_selection: "This inspection is no longer current. Inspect the baseline again before deciding.",
    selection_limit: "This native session reached its inspection limit. Restart the desktop.",
    decision_not_applicable: "That decision is unavailable for the inspected baseline state.",
    pending_intent: "An exact owner intent is still pending. Reconcile it before another decision.",
    no_pending_intent: "There is no retained intent in this native process.",
    owner_not_ready: "A fresh connected inspection is required before submitting a decision.",
    invalid_baseline_data: "The engine returned a baseline that could not be validated.",
    unexpected_reply: "The engine returned an unexpected response. Refresh before continuing.",
  };
  return messages[code] ?? "The owner request failed. Refresh before continuing.";
}
