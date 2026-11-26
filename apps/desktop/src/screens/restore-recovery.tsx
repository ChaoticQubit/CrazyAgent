import { useEffect, useRef, useState } from "react";
import { openBackups, restoreRecover, restoreRecovery, type RecoveryRead } from "../api-restore";
import { useLoad, type Loaded } from "../state";
import { BrandMark, Button, Notice, Skeleton } from "../ui";
import { RESTORE_RUNNING } from "./settings/restore";

/** What the recovery screen says when a restore did not finish, the same words in the TUI. */
export const RECOVERY_BROKEN = "Restore did not finish. Your data is safe in a backup.";

/** What the recovery screen says when a restore stopped before it changed anything. */
export const RECOVERY_CONTINUE = "Restore stopped before it changed anything. Your 2.5 data is unchanged.";

/** The action that puts the copy the restore record names back over the data. */
export const PUT_BACK = "Put the backup back";

/** The action that carries on with the unchanged 2.5 data. */
export const CONTINUE = "Continue with your 2.5 data";

/** The action that opens the folder keeping the copies a restore left. */
export const OPEN_FOLDER = "Open the backups folder";

/** The recovery screen as a view of its read: the sentence for the offer, the backups folder
 * unless the data is unchanged, the refusal code of a copy that can't be put back, and the
 * offer's one action. Inputs: the read, a running flag, the last refusal and the handlers.
 * Output: the full-window page, with no way to start the engine. */
export function RestoreRecoveryView({ read, busy, problem, onPutBack, onContinue, onOpenFolder, onRetry }: {
  read: Loaded<RecoveryRead>;
  busy: boolean;
  problem: string | null;
  onPutBack: () => void;
  onContinue: () => void;
  onOpenFolder: () => void;
  onRetry: () => void;
}) {
  const offer = read.kind === "ok" ? read.value.offer : null;
  const unchanged = offer?.kind === "continue" || offer?.kind === "finished";
  const line = offer?.kind === "continue" ? RECOVERY_CONTINUE : offer?.kind === "finished" ? RESTORE_RUNNING : RECOVERY_BROKEN;
  const code = read.kind === "error" ? read.code : offer?.kind === "stuck" ? offer.code : null;
  return <main id="main" className="full-state">
    <div className="box">
      <BrandMark />
      <h1>Restore 2.4</h1>
      <p role="status">{line}</p>
      {read.kind === "loading" && <Skeleton shape="line" count={2} />}
      {read.kind === "ok" && !unchanged && <p className="secondary">Backups folder: {read.value.folder}</p>}
      {code && <p className="secondary">Code: {code}</p>}
      {problem && <Notice tone="danger" alert>Code: {problem}</Notice>}
      <span className="row">
        {offer?.kind === "put_back" && <Button variant="primary" icon="hard-drive" pending={busy} disabled={busy} onClick={onPutBack}>{PUT_BACK}</Button>}
        {offer?.kind === "continue" && <Button variant="primary" pending={busy} disabled={busy} onClick={onContinue}>{CONTINUE}</Button>}
        {read.kind === "ok" && !unchanged && <Button icon="folder" disabled={busy} onClick={onOpenFolder}>{OPEN_FOLDER}</Button>}
        {read.kind !== "loading" && (offer?.kind !== "finished" || problem !== null) && <Button icon="refresh-cw" disabled={busy} onClick={onRetry}>Try again</Button>}
      </span>
    </div>
  </main>;
}

/** Finishes a restore that wrote the 2.4 data, one run at a time. A refused finish clears the
 * running flag, so Try again runs it again rather than the screen retrying on its own.
 * Inputs: the running flag and the finish call. Output: null once it ran, else its code. */
export async function finishOnce(finishing: { current: boolean }, finish: () => Promise<{ kind: string; code?: string }>): Promise<string | null> {
  finishing.current = true;
  const result = await finish();
  if (result.kind === "ok") return null;
  finishing.current = false;
  return result.code ?? "storage_restore_failed";
}

/** The recovery screen for a restore that did not finish: reads the offer, finishes a restore
 * that wrote the 2.4 data on its own, runs the owner's action once, and on success hands back to
 * the start, which brings 2.5 up or shows the restored state. Input: the done handler. */
export function RestoreRecovery({ onDone }: { onDone: () => void }) {
  const [read, reload] = useLoad(restoreRecovery, [], "restore-recovery");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const finishing = useRef(false);
  async function act(run: () => Promise<{ kind: string; code?: string }>, done: boolean) {
    if (busy) return;
    setBusy(true);
    setProblem(null);
    const result = await run();
    setBusy(false);
    if (result.kind === "ok" && done) return onDone();
    if (result.kind === "error") setProblem(result.code ?? "storage_restore_failed");
    reload();
  }
  async function finish() {
    setBusy(true);
    setProblem(null);
    const code = await finishOnce(finishing, () => restoreRecover("finish"));
    setBusy(false);
    if (code === null) return onDone();
    setProblem(code);
  }
  useEffect(() => {
    if (read.kind === "ok" && read.value.offer === null) onDone();
    if (read.kind !== "ok" || read.value.offer?.kind !== "finished" || finishing.current) return;
    void finish();
  }, [read]);
  return <RestoreRecoveryView read={read} busy={busy} problem={problem}
    onPutBack={() => void act(() => restoreRecover("put_back"), true)}
    onContinue={() => void act(() => restoreRecover("continue"), true)}
    onOpenFolder={() => void act(openBackups, false)} onRetry={reload} />;
}
