import { useEffect, useRef, useState } from "react";
import { APPROVAL_PAGE, answerApproval, answeredLine, approvalLines, loadApproval, shownState, timeLeft, type ApprovalView } from "../../api-connectors";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, PageHeader, Skeleton } from "../../ui";

const NOT_READY = "The engine is not ready yet. Try again.";
const TICK_MS = 1000;

/** What the approval page asks its container to do. */
export interface ApprovalActions {
  onAnswer: (allow: boolean) => void;
  onRetry: () => void;
}

/** An answer the engine took, kept with the read it answered so the page can show it until
 * a newer read lands. */
export interface KeptAnswer {
  over: Loaded<ApprovalView>;
  view: ApprovalView;
}

/** What the approval page shows: the view an answer returned while the read it answered is
 * still the loaded one and names the same approval, so Allow and Deny are gone before the
 * re-read lands; else the read. Inputs: the loaded read and the kept answer, if any.
 * Output: the view to show. */
export function approvalOnPage(view: Loaded<ApprovalView>, kept: KeptAnswer | null): Loaded<ApprovalView> {
  if (kept === null || kept.over !== view || view.kind !== "ok" || kept.view.id !== view.value.id) return view;
  return { kind: "ok", value: kept.view };
}

/** The approval page for one connector write: what asks and what it sends as plain text,
 * the time left with Allow and Deny while it waits and its wait has not run out, else where
 * it stands, and the full arguments in a closed fold. Inputs: the approval as read, now in
 * epoch milliseconds, a busy flag, the last failure and the actions. Output: the page. */
export function ApprovalPageView({ view, now, busy, failure, actions }: {
  view: Loaded<ApprovalView>;
  now: number;
  busy: boolean;
  failure: string;
  actions: ApprovalActions;
}) {
  const top = <PageHeader title={APPROVAL_PAGE.title} description={APPROVAL_PAGE.purpose} />;
  if (view.kind === "loading") return <>{top}<Skeleton shape="row" count={3} /></>;
  if (view.kind === "not_ready") return <>{top}<NotReady what={APPROVAL_PAGE.title} /></>;
  if (view.kind === "error") return <>{top}<LoadError message={view.message} onRetry={actions.onRetry} /></>;
  const approval = view.value;
  const state = shownState(approval, now);
  return <>
    {top}
    <div className="stack">
      {failure && <Notice tone="danger" alert>{failure}</Notice>}
      {approval.hidden && <Notice>{APPROVAL_PAGE.hidden}</Notice>}
      <ul className="pj-rows" aria-label={APPROVAL_PAGE.title}>{approvalLines(approval).map((line) => <li key={line}>{line}</li>)}</ul>
      {state === "waiting"
        ? <div className="pj-row">
          <span>{`Time left: ${timeLeft(now, approval.expires_ms)}`}</span>
          <Button variant="primary" disabled={busy} onClick={() => actions.onAnswer(true)}>{APPROVAL_PAGE.allow}</Button>
          <Button variant="danger" disabled={busy} onClick={() => actions.onAnswer(false)}>{APPROVAL_PAGE.deny}</Button>
        </div>
        : <p>{answeredLine({ ...approval, state })}</p>}
      <details className="set-body"><summary>{APPROVAL_PAGE.fold}</summary><pre className="mono small">{approval.arguments}</pre></details>
    </div>
  </>;
}

/** The approval page the ConnectorWrite notice opens: reads the write, counts its time down
 * each second and reads it again once when the wait ends, answers it once and shows the
 * answer with no buttons until the re-read lands, and reads it again when it was already
 * answered. Input: the approval id from the route. Output: the page. */
export function ConnectorApprovalPage({ id }: { id: number }) {
  const session = useAppSession();
  const [view, reload] = useLoad(() => loadApproval(id), [id]);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [kept, setKept] = useState<KeptAnswer | null>(null);
  const reread = useRef<number | null>(null);
  const shownRead = useRef(view);
  shownRead.current = view;
  const ends = view.kind === "ok" && view.value.state === "waiting" ? view.value.expires_ms : null;
  useEffect(() => {
    if (ends === null) return undefined;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), TICK_MS);
    return () => window.clearInterval(timer);
  }, [ends]);
  useEffect(() => {
    if (ends === null || now < ends || reread.current === ends) return;
    reread.current = ends;
    reload();
  }, [ends, now, reload]);

  async function answer(allow: boolean) {
    if (busy) return;
    setBusy(true);
    setFailure("");
    const result = session.noteResult(await answerApproval(id, allow));
    setBusy(false);
    if (result.kind === "ok") {
      setKept({ over: shownRead.current, view: result.value });
      session.showToast(allow ? APPROVAL_PAGE.allowed : APPROVAL_PAGE.denied);
      reload();
      return;
    }
    setFailure(result.kind === "not_ready" ? NOT_READY : result.message);
    if (result.kind === "error" && result.code === "connector_approval_closed") reload();
  }

  const onPage = approvalOnPage(view, kept);
  return <ApprovalPageView view={onPage} now={now} busy={busy || onPage !== view} failure={failure} actions={{ onAnswer: (allow) => { void answer(allow); }, onRetry: reload }} />;
}
