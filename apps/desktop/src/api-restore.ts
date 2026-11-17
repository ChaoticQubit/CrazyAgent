import { call, nextGeneration, type Result } from "./api";

/** How the owner's last restore of the pinned 2.4 backup stands, the engine's `RestoreState`. */
export type RestoreLast = { kind: "pending" } | { kind: "restored"; copy: string } | { kind: "failed"; code: string };

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
