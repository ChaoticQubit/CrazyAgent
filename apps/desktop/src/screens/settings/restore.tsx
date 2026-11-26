import { useEffect, useRef, useState } from "react";
import { getLogCompaction, plainError, saveLogCompaction, type LogCompactionView } from "../../api";
import { restorePinned, restoreStatus, type RestoreView } from "../../api-restore";
import { useLoad, type Loaded } from "../../state";
import { Button, Dialog, LoadError, NotReady, Notice, SettingLine, SettingSection, Skeleton, Switch } from "../../ui";
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
  if (view.last?.kind === "restoring" || view.last?.kind === "broken") return { offer: false, line: plainError("engine_restore_broken"), failure: null };
  if (view.last?.kind === "restored") return { offer: false, line: plainError("engine_restored"), failure: null };
  if (view.until_ms === null) return { offer: false, line: RESTORE_NONE, failure };
  const until = new Date(view.until_ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  return { offer: true, line: `You can bring back your 2.4 data until ${until}.`, failure };
}

/** The compaction row's label, the engine's `log_compaction::LABEL`, the same words in the TUI. */
export const COMPACTION_LABEL = "Compact the event log";

/** The compaction row's help line, the engine's `log_compaction::HELP`, the same words in the TUI. */
export const COMPACTION_HELP = "Dev builds only. Writes a snapshot every 10,000 events and clears the saved data of older log entries; the entries stay.";

/** The compaction switch as a row, shown only when the engine's read says this is a dev build.
 * Inputs: the read, a saving flag, the last problem and the switch handler. Output: the row, or
 * nothing outside a dev build or before the read. */
export function CompactionRowView({ compaction, saving, problem, onSwitch }: {
  compaction: Loaded<LogCompactionView>;
  saving: boolean;
  problem: string;
  onSwitch: (on: boolean) => void;
}) {
  if (compaction.kind !== "ok" || !compaction.value.dev_build) return null;
  return <SettingSection title="Event log">
    <SettingLine label={COMPACTION_LABEL} help={COMPACTION_HELP} htmlFor="log-compaction">
      <Switch id="log-compaction" label={COMPACTION_LABEL} hideLabel checked={compaction.value.choice === "on"}
        disabledReason={saving ? "Saving..." : undefined} onChecked={(on) => onSwitch(on)} />
    </SettingLine>
    {problem && <Notice tone="danger" alert>{problem}</Notice>}
  </SettingSection>;
}

/** Waits for a submitted save to finish, then hands its result to the page only while the page
 * is still open; the save always runs to its end. Inputs: the pending save, the page's alive flag
 * and what the page does with the result. Output: done once the save has ended. */
export async function settleAfter<T>(pending: Promise<T>, alive: { readonly current: boolean }, settle: (result: T) => void): Promise<void> {
  const result = await pending;
  if (alive.current) settle(result);
}

/** Settings, the compaction row: reads the switch, and on a change saves it once, then reads
 * it again. */
function CompactionRow() {
  const [compaction, reload] = useLoad(getLogCompaction, [], "log-compaction");
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState("");
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);
  async function save(on: boolean) {
    if (saving) return;
    setSaving(true);
    setProblem("");
    await settleAfter(saveLogCompaction(on ? "on" : "off"), alive, (result) => {
      setSaving(false);
      if (result.kind === "error") setProblem(result.message);
      reload();
    });
  }
  return <CompactionRowView compaction={compaction} saving={saving} problem={problem} onSwitch={(on) => void save(on)} />;
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
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);
  async function confirm() {
    if (sending) return;
    setSending(true);
    setProblem("");
    await settleAfter(restorePinned(), alive, (result) => {
      setSending(false);
      setAsking(false);
      if (result.kind === "error") setProblem(result.message);
      if (result.kind === "not_ready") setProblem("Restore 2.4 isn't in this build.");
      reload();
    });
  }
  return <div className="stack">
    <RestoreSettingsView status={status} asking={asking} sending={sending} problem={problem}
      onAsk={() => { setAsking(true); setProblem(""); }} onCancel={() => setAsking(false)} onConfirm={() => void confirm()} onRetry={reload} />
    <div className="pj set-groups"><CompactionRow /></div>
  </div>;
}
