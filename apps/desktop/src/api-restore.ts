import { call, nextGeneration, type Result } from "./api";

/** How the owner's last restore of the pinned 2.4 backup stands, the engine's `RestoreState`. */
export type RestoreLast = { kind: "pending" } | { kind: "restored"; copy: string } | { kind: "failed"; code: string }
  | { kind: "restoring"; copy: string | null } | { kind: "broken"; copy: string | null };

/** What the recovery screen offers for a restore that did not finish, the engine's `Recovery`. */
export type Recovery = { kind: "continue" } | { kind: "finished"; copy: string } | { kind: "put_back"; copy: string } | { kind: "stuck"; code: string };

/** The recovery read, the native `RecoveryView`: the offer, null once nothing needs recovery,
 * and the backups folder it names. */
export interface RecoveryRead {
  offer: Recovery | null;
  folder: string;
}

/** Reads what the recovery screen offers, writing nothing. Output: the read, or the refusal. */
export function restoreRecovery(): Promise<Result<RecoveryRead>> {
  return call<RecoveryRead>("desktop_restore_recovery");
}

/** Runs one recovery action: put the named copy back, continue with the 2.5 data, or finish a
 * restore that wrote the 2.4 data. Input: the action. Output: ok, or the refusal. */
export function restoreRecover(action: "put_back" | "continue" | "finish"): Promise<Result<null>> {
  return call<null>("desktop_restore_recover", { action });
}

/** Opens the backups folder that keeps the copies a restore left. Output: ok, or the refusal. */
export function openBackups(): Promise<Result<null>> {
  return call<null>("desktop_restore_open_backups");
}

/** What Settings shows about bringing back the 2.4 data, the native `RestoreView`: until when the
 * pinned backup can be restored, in milliseconds since 1970, and how the last restore ended. */
export interface RestoreView {
  until_ms: number | null;
  last: RestoreLast | null;
}

/** Reads until when the 2.4 data can be brought back and how the last restore ended, without the
 * engine. Output: the view, or a plain error. */
export function restoreStatus(): Promise<Result<RestoreView>> {
  return call<RestoreView>("desktop_restore_status");
}

/** Asks the engine to stop and bring back the pinned 2.4 backup, after the owner's confirm. It is
 * sent once and never again. Output: ok once sent, also when the stopping engine never answered,
 * so the status read tells how it ended; else the plain refusal. */
export function restorePinned(): Promise<Result<null>> {
  return call<null>("desktop_restore_pinned", { generation: nextGeneration() });
}
