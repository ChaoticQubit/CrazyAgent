import type { ReactNode } from "react";
import type { LastWriter } from "../api-process";
import { Button, Notice } from "../ui";

/** A save the engine refused as stale, held so the owner sees what landed before anything is
 * written: the version the save named, the newer version and its value, and who saved it. */
export interface Conflict<T> {
  opened: number;
  version: number;
  current: T;
  writer: LastWriter;
}

/** Names who saved a shared item's current version. Input: the writer. Output: "you in another
 * window", "Agent <id>" for a team agent, another agent's own name, or "another save". */
export function writerName(writer: LastWriter): string {
  if (writer === "owner") return "you in another window";
  if (writer === "nobody") return "another save";
  const team = /^team-([1-9][0-9]*)$/u.exec(writer.agent);
  return team ? `Agent ${team[1]}` : writer.agent;
}

/** The versions Keep mine saves with: over the newer version the conflict showed, naming the
 * version the owner opened so the engine records the owner's choice. Input: the conflict. */
export function keepMine<T>(conflict: Conflict<T>): { version: number; opened: number } {
  return { version: conflict.version, opened: conflict.opened };
}

/** What a stale save shows instead of saving: who changed the item since the owner opened it,
 * the value that landed and the only two choices. Reload puts that value in the editor and
 * writes nothing; Keep mine saves the owner's own over it, which the engine records.
 * Inputs: the conflict, whether a choice is running, the two actions and the value as shown. */
export function ConflictView<T>({ conflict, busy, onReload, onKeepMine, children }: {
  conflict: Conflict<T>;
  busy: boolean;
  onReload: () => void;
  onKeepMine: () => void;
  children: ReactNode;
}) {
  return <Notice tone="warn" alert>
    <div className="stack-small">
      <strong>{`Changed by ${writerName(conflict.writer)} since you opened it.`}</strong>
      <p className="secondary">Saved now:</p>
      {children}
      <p className="secondary">Reload puts this version in the editor. Keep mine saves your text over it, and CrazyAgent records that you chose to.</p>
      <span className="row">
        <Button disabled={busy} onClick={onReload}>Reload</Button>
        <Button variant="primary" pending={busy} pendingLabel="Saving..." disabled={busy} onClick={onKeepMine}>Keep mine</Button>
      </span>
    </div>
  </Notice>;
}
