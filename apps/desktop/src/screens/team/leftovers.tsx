import { useState } from "react";
import { settleLeftoverHold } from "../../api";
import type { LeftoverHold, TeamStatus } from "../../engine-client";
import { Button, StatusBadge, type Tone } from "../../ui";
import { changeNote } from "./controls";

const STATE: Record<LeftoverHold["state"], { word: string; tone: Tone }> = {
  held: { word: "Waiting for you", tone: "warn" },
  ending: { word: "Ending", tone: "info" },
  kept: { word: "Kept running", tone: "neutral" },
};

/** The badge word and tone for where one hold stands. Input: the hold's state. Output: the
 * copy. */
export function holdState(state: LeftoverHold["state"]): { word: string; tone: Tone } {
  return STATE[state];
}

/** Says how long ago a hold began, in whole minutes, or "just now" under one minute. Inputs:
 * when it began and now, both in Unix milliseconds. Output: the words. */
export function holdAge(sinceMs: number, nowMs: number): string {
  const minutes = Math.floor(Math.max(0, nowMs - sinceMs) / 60_000);
  if (minutes === 0) return "just now";
  return minutes === 1 ? "1 minute ago" : `${minutes} minutes ago`;
}

/** Which owner buttons one hold offers: End while it is not already ending, Keep while it is
 * not already kept. Input: the hold. Output: the two switches. */
export function holdButtons(hold: Pick<LeftoverHold, "state">): { end: boolean; keep: boolean } {
  return { end: hold.state !== "ending", keep: hold.state !== "kept" };
}

/** Lists the processes the project's agents left running after their stop, each with End and
 * Keep, as the Left running setting asks. Renders nothing while none run. Inputs: the project
 * id, the team status, now in Unix milliseconds and a reload action. */
export function LeftoversPanel({ projectId, status, nowMs, reload }: {
  projectId: string;
  status: Pick<TeamStatus, "leftover_holds">;
  nowMs: number;
  reload: () => void;
}) {
  const [busy, setBusy] = useState<number | null>(null);
  const [note, setNote] = useState("");
  const holds = status.leftover_holds ?? [];
  if (holds.length === 0) return null;

  async function settle(key: number, end: boolean) {
    if (busy !== null) return;
    setBusy(key);
    const result = await settleLeftoverHold(projectId, key, end);
    setNote(changeNote(result, "ending or keeping left running processes"));
    setBusy(null);
    reload();
  }

  return <section className="pj-stack-s" aria-label="Left running">
    <h3>Left running</h3>
    <p className="pj-small pj-muted">Processes an agent started that still run after it stopped.</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <ul className="pj-rows">
      {holds.map((hold) => {
        const state = holdState(hold.state);
        const buttons = holdButtons(hold);
        return <li key={hold.key} className="pj-row" aria-label={`Left by ${hold.who}`}>
          <div>
            <strong>{hold.who}</strong> <StatusBadge tone={state.tone} word={state.word} />
            <p className="pj-small">{hold.names}</p>
            <p className="pj-small pj-muted">{holdAge(hold.since_ms, nowMs)}{hold.holds_account ? ", keeping its account" : ""}</p>
          </div>
          <div className="pj-row">
            {buttons.end && <Button size="small" disabled={busy !== null} pending={busy === hold.key} pendingLabel="Saving..." onClick={() => void settle(hold.key, true)}>End</Button>}
            {buttons.keep && <Button size="small" variant="secondary" disabled={busy !== null} pending={busy === hold.key} pendingLabel="Saving..." onClick={() => void settle(hold.key, false)}>Keep</Button>}
          </div>
        </li>;
      })}
    </ul>
  </section>;
}
