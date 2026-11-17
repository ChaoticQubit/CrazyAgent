import { useState } from "react";
import { plainError } from "../../api";
import { restorePinned, restoreStatus, type RestoreView } from "../../api-restore";
import { useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, NotReady, Notice, SettingSection, Skeleton } from "../../ui";
import "../project/projects.css";

/** What bringing back the 2.4 data does, said before it runs (plan 3.4 item 4, DEC-192); the TUI
 * says the same lines. */
export const RESTORE_CONFIRM: readonly string[] = [
  "Everything written since the update to 2.5 is replaced. CrazyAgent keeps it in a dated copy in its backups folder.",
  "Install CrazyAgent 2.4.2 again to use the 2.4 data. The next start of 2.5 updates it again.",
  "Grants you revoked and plugins you stopped trusting after the update come back.",
];

/** The section's line when there is nothing to bring back, the same words in the TUI. */
export const RESTORE_NONE = "There's no 2.4 backup to bring back. CrazyAgent keeps it for 30 days after the update to 2.5.";

/** The section's line while a restore runs, the same words in the TUI. */
export const RESTORE_RUNNING = "Your 2.4 data is being brought back. CrazyAgent's engine stays stopped until it's done.";

/** What the section shows for a status read: whether the button is offered, the line under the
 * title and the last restore's failure, if any. Input: the view. Output: the three parts. */
export function restoreShown(view: RestoreView): { offer: boolean; line: string; failure: string | null } {
  const running = view.last?.kind === "pending";
  const failure = view.last?.kind === "failed" ? plainError(view.last.code) : null;
  if (running) return { offer: false, line: RESTORE_RUNNING, failure: null };
  if (view.last?.kind === "restored") return { offer: false, line: plainError("engine_restored"), failure: null };
  if (view.until_ms === null) return { offer: false, line: RESTORE_NONE, failure };
  const until = new Date(view.until_ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  return { offer: true, line: `You can bring back your 2.4 data until ${until}.`, failure };
}

const RESTORE_TOP = <div className="settings-top"><div>
  <h2>Restore 2.4</h2>
  <p>Bring back your data as it was before the update to 2.5.</p>
</div></div>;

/** Settings, Restore 2.4 as a view of its state: the title in every state, the line, the last
 * failure, and the button while the pinned backup lasts, which asks first with the three things a
 * restore does. Inputs: the status read, the open confirm, a sending flag, the last problem and
 * the handlers. Output: the tab. */
export function RestoreSettingsView({ status, asking, sending, problem, onAsk, onCancel, onConfirm, onRetry }: {
  status: Loaded<RestoreView>;
  asking: boolean;
  sending: boolean;
  problem: string;
  onAsk: () => void;
  onCancel: () => void;
  onConfirm: () => void;
  onRetry: () => void;
}) {
  if (status.kind === "loading") return <>{RESTORE_TOP}<Skeleton shape="line" count={2} /></>;
  if (status.kind === "not_ready") return <>{RESTORE_TOP}<NotReady what="Restore 2.4" /></>;
  if (status.kind === "error") return <>{RESTORE_TOP}<LoadError message={status.message} onRetry={onRetry} /></>;
  const shown = restoreShown(status.value);
  return <>
    {RESTORE_TOP}
    <div className="pj set-groups">
      <SettingSection title="2.4 backup">
        <div className="set-body">
          <p role="status">{shown.line}</p>
          {shown.failure && <Notice tone="danger">{shown.failure}</Notice>}
          {problem && <Notice tone="danger" alert>{problem}</Notice>}
          {shown.offer && <span><Button variant="danger" icon="hard-drive" onClick={onAsk}>Bring back 2.4 data</Button></span>}
        </div>
      </SettingSection>
    </div>
    <Dialog open={asking && shown.offer} title="Bring back your 2.4 data?" onClose={onCancel} busy={sending}
      footer={<><Button disabled={sending} onClick={onCancel} data-autofocus>Cancel</Button>
        <Button variant="danger" pending={sending} pendingLabel="Stopping the engine..." onClick={onConfirm}>Bring back 2.4 data</Button></>}>
      <ul>{RESTORE_CONFIRM.map((line) => <li key={line}>{line}</li>)}</ul>
    </Dialog>
  </>;
}

/** Settings, Restore 2.4: reads until when the 2.4 data can be brought back, and on the owner's
 * confirm sends the restore once, then reads how it stands. */
export function RestoreSettings() {
  const [status, reload] = useLoad(restoreStatus, [], "restore-status");
  const [asking, setAsking] = useState(false);
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState("");
  async function confirm() {
    if (sending) return;
    setSending(true);
    setProblem("");
    const result = await restorePinned();
    setSending(false);
    setAsking(false);
    if (result.kind === "error") setProblem(result.message);
    if (result.kind === "not_ready") setProblem("Restore 2.4 isn't in this build.");
    reload();
  }
  return <div className="stack">
    <RestoreSettingsView status={status} asking={asking} sending={sending} problem={problem}
      onAsk={() => { setAsking(true); setProblem(""); }} onCancel={() => setAsking(false)} onConfirm={() => void confirm()} onRetry={reload} />
  </div>;
}
