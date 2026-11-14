import { useEffect, useState } from "react";
import type { Result } from "../../api";
import { dismissRecovery, loadRecovery, recoveryMessage, retryRecovery } from "../../api-recovery";
import type { RecoveryBanner as Banner, RecoveryNotice, RecoveryRun } from "../../api-recovery";
import { useLoad, type Loaded } from "../../state";
import { Button, Notice } from "../../ui";
import { displayText } from "../conversation";

/** How often the banner reads again while a run still waits to resume. */
export const RECOVERY_POLL_MS = 5_000;

const QUIET_CODES = ["engine_unreachable", "not_connected"];

/** One banner's items by what the owner does with them. */
export interface BannerGroups {
  interrupted: RecoveryRun[];
  resumed: RecoveryRun[];
  failed: RecoveryRun[];
  notices: RecoveryNotice[];
}

/** Splits a banner's runs by where their resume stands. Input: the banner. Output: the groups;
 * a run still resuming stays with the interrupted ones until it runs. */
export function bannerGroups(banner: Banner): BannerGroups {
  const runs = (...states: RecoveryRun["state"][]) => banner.runs.filter((run) => states.includes(run.state));
  return { interrupted: runs("interrupted", "resuming"), resumed: runs("resumed"), failed: runs("failed"), notices: banner.notices };
}

/** Counts what still waits for the owner or for a resume. Input: the banner. Output: the count. */
export function openItems(banner: Banner): number {
  return banner.runs.filter((run) => run.state !== "resumed").length + banner.notices.length + banner.more;
}

/** Whether a run still waits to resume, so the banner reads again. Input: the banner. */
export function stillResuming(banner: Banner): boolean {
  return banner.runs.some((run) => run.state === "interrupted" || run.state === "resuming");
}

/** Whether the banner reads again on its timer: while a run still waits to resume, and after a
 * failed read of a kept banner, so its stale runs refresh once the engine answers. A failed first
 * read offers Try again instead. Inputs: the banner shown and the failure line. Output: true then. */
export function keepsReading(banner: Banner | null, problem: string): boolean {
  return banner !== null && (stillResuming(banner) || problem !== "");
}

/** Describes one interrupted run: who, which turn and what it was asked, and what happens next.
 * Input: the run. Output: one line. */
export function runLine(run: RecoveryRun): string {
  const asked = run.asked ? `: "${displayText(run.asked)}"` : "";
  const next = run.state === "resuming" ? "Resuming now." : run.paused ? "Resumes when you resume the team." : "Resumes on its own.";
  return `${displayText(run.label)} (${run.harness}), turn ${run.turn}${asked}. ${next}`;
}

/** Names what one notice is about. Input: the notice. Output: the subject. */
export function noticeSubject(notice: RecoveryNotice): string {
  const agent = notice.label ? displayText(notice.label) : "An agent";
  if (notice.action_kind === "agent_spawn") return `${agent}'s start`;
  if (notice.action_kind === "worktree_create") return `${agent}'s worktree`;
  if (notice.action_kind === "landing_accept") return `Change set ${displayText(notice.change_set ?? "")}`;
  if (notice.action_kind === "project_delete") return "This project's delete";
  return "This notice";
}

/** Says what one notice shows and what to check before dismissing it. Input: the notice.
 * Output: one sentence. */
export function noticeLine(notice: RecoveryNotice): string {
  const subject = noticeSubject(notice);
  if (notice.action_kind === "project_delete") return "A delete was cut short. Some folders may already be in the Trash.";
  if (notice.shown === "landed") return `${subject} landed while CrazyAgent stopped.`;
  if (notice.shown === "dropped") return `${subject} did not land. Accept it again from Changes.`;
  if (notice.action_kind === "agent_spawn") return `${subject} may or may not have run. Check the agent, then dismiss this to let it start again.`;
  if (notice.action_kind === "worktree_create") return `${subject} may or may not have been added. Check its folder, then dismiss this to let the agent start again.`;
  return `${subject} has an unknown outcome.`;
}

/** One run's row key: its agent and the time it was cut short, which the engine keeps unique
 * per run, since one agent can have several runs. Input: the run. Output: the key. */
export function runKey(run: RecoveryRun): string {
  return `${run.agent}-${run.at_ms}`;
}

/** The busy key of one choice, so only its button shows progress. */
export function choiceKey(choice: { agent: number } | RecoveryNotice): string {
  return "action_kind" in choice ? `dismiss-${choice.action_kind}-${choice.id}` : `retry-${choice.agent}`;
}

/** What an engine stop cut short in one project: what is still interrupted, what resumed and
 * what failed, with Retry on each failed resume, and the notices to check and dismiss; a delete
 * cut short offers Keep the project, which dismisses it, and Delete again. Renders nothing when
 * nothing waits. Inputs: the banner, the choice running, the result of the last choice and the
 * three choices. */
export function RecoveryBannerView({ banner, busy, note, onRetry, onDismiss, onDeleteAgain }: {
  banner: Banner;
  busy: string | null;
  note: string;
  onRetry: (agent: number) => void;
  onDismiss: (notice: RecoveryNotice) => void;
  onDeleteAgain: () => void;
}) {
  const waiting = openItems(banner);
  if (waiting === 0) return null;
  const groups = bannerGroups(banner);
  return <Notice tone="warn" icon="refresh-cw">
    <div className="stack-small recovery-banner">
      <strong>{`Work was interrupted when CrazyAgent stopped. ${waiting} to resolve.`}</strong>
      {groups.interrupted.length > 0 && <section className="stack-small" aria-label="Interrupted">
        <span className="secondary">Interrupted</span>
        <ul>{groups.interrupted.map((run) => <li key={runKey(run)}>{runLine(run)}</li>)}</ul>
      </section>}
      {groups.resumed.length > 0 && <section className="stack-small" aria-label="Resumed">
        <span className="secondary">Resumed</span>
        <ul>{groups.resumed.map((run) => <li key={runKey(run)}>{`${displayText(run.label)} (${run.harness}) runs again.`}</li>)}</ul>
      </section>}
      {groups.failed.length > 0 && <section className="stack-small" aria-label="Failed">
        <span className="secondary">Failed</span>
        <ul>{groups.failed.map((run) => <li key={runKey(run)} className="row">
          <span>{`${displayText(run.label)} (${run.harness}) didn't resume. ${displayText(run.reason ?? "No reason was recorded.")}`}</span>
          <Button size="small" pending={busy === choiceKey(run)} pendingLabel="Retrying..." disabled={busy !== null} onClick={() => onRetry(run.agent)}>Retry</Button>
        </li>)}</ul>
      </section>}
      {groups.notices.length > 0 && <section className="stack-small" aria-label="Check before you start again">
        <span className="secondary">Check before you start again</span>
        <ul>{groups.notices.map((notice) => <li key={choiceKey(notice)} className="row">
          <span>{noticeLine(notice)}</span>
          {notice.action_kind === "project_delete"
            ? <>
              <Button size="small" pending={busy === choiceKey(notice)} pendingLabel="Keeping..." disabled={busy !== null} onClick={() => onDismiss(notice)}>Keep the project</Button>
              <Button size="small" variant="danger" disabled={busy !== null} onClick={onDeleteAgain}>Delete again</Button>
            </>
            : <Button size="small" pending={busy === choiceKey(notice)} pendingLabel="Dismissing..." disabled={busy !== null} onClick={() => onDismiss(notice)}>Dismiss</Button>}
        </li>)}</ul>
      </section>}
      {banner.more > 0 && <p className="secondary">{`${banner.more} more aren't shown. They appear as you resolve these.`}</p>}
      {note && <p role="status">{note}</p>}
    </div>
  </Notice>;
}

/** What the banner shows after the latest read: the banner it returned, or after a failed read
 * the last banner read, kept so its runs stay in view and the reads go on, with the failure as
 * a line. A quiet failure, the engine down or not connected, shows nothing; the session reads
 * again once it reconnects. Inputs: the latest read and the last banner read, if any. Output:
 * the banner or null, and the failure line, empty when there is none to show. */
export function latestBanner(loaded: Loaded<Banner>, last: Banner | null): { banner: Banner | null; problem: string } {
  if (loaded.kind === "ok") return { banner: loaded.value, problem: "" };
  if (loaded.kind === "loading") return { banner: last, problem: "" };
  if (loaded.kind === "error" && QUIET_CODES.includes(loaded.code)) return { banner: null, problem: "" };
  return { banner: last, problem: `Interrupted work couldn't be read. ${recoveryMessage(loaded)}` };
}

/** One project's recovery banner: reads the engine's list, reads again while a run waits to
 * resume, also after a failed read, and sends Retry and Dismiss to the engine, reading the list
 * again after each; Delete again is the page's to open. A failed first read offers Try again.
 * Inputs: the project id and what Delete again does. */
export function RecoveryBanner({ project, onDeleteAgain }: { project: string; onDeleteAgain: () => void }) {
  const [loaded, reload] = useLoad(() => loadRecovery(project), [project]);
  const [last, setLast] = useState<Banner | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState("");
  useEffect(() => { if (loaded.kind === "ok") setLast(loaded.value); }, [loaded]);
  const { banner, problem } = latestBanner(loaded, last);
  const polling = keepsReading(banner, problem);
  useEffect(() => {
    if (!polling) return undefined;
    const timer = window.setInterval(reload, RECOVERY_POLL_MS);
    return () => window.clearInterval(timer);
  }, [polling, project]);

  const choose = async (key: string, run: () => Promise<Result<null>>, done: string) => {
    setBusy(key);
    setNote("");
    const result = await run();
    setBusy(null);
    setNote(result.kind === "ok" ? done : recoveryMessage(result));
    reload();
  };

  if (banner === null) {
    if (!problem) return null;
    return <Notice tone="warn">
      <span className="row"><span>{problem}</span><Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button></span>
    </Notice>;
  }
  return <RecoveryBannerView banner={banner} busy={busy} note={[note, problem].filter(Boolean).join(" ")}
    onRetry={(agent) => void choose(choiceKey({ agent }), () => retryRecovery(agent), "It will try to resume again.")}
    onDismiss={(notice) => void choose(choiceKey(notice), () => dismissRecovery(notice), notice.action_kind === "project_delete" ? "The project stays." : "Dismissed.")}
    onDeleteAgain={onDeleteAgain} />;
}
