import { useEffect, useRef, useState } from "react";
import type { Failure } from "../../api";
import { HEARTBEAT_MS, IDE_NOT_CLOSED, IDE_NOT_HIDDEN, IDE_NOT_PLACED, bounds, closeIde, desktopPlatform, hideIde, ideReadFailed, ideStatus, ideUnavailable, ideView, openFailure, openIde, placeIde, sameBounds, touchIde } from "../../api-ide";
import type { IdeBounds, IdeTabState, IdeView } from "../../api-ide";
import { Button, useOverlayShown } from "../../ui";
import "./ide.css";

/** How often a page waiting on the IDE build reads its state again, in milliseconds. */
export const IDE_POLL_MS = 1_000;
const NOT_READY = "CrazyAgent's engine isn't ready yet. Try again in a moment.";

function failureLine(failure: Failure, sentence: (code: string) => string = () => ""): string {
  if (failure.kind !== "error") return NOT_READY;
  return sentence(failure.code) || failure.message;
}

/** The IDE tab's status: the build's plain line and its one action, Download or Retry.
 * Inputs: the IDE state, whether the action runs, and its handler. Output: the panel. */
export function IdeStatus({ state, busy, onAction }: { state: IdeTabState; busy: boolean; onAction: () => void }) {
  const status = ideStatus(state);
  return <div className="ide-status" role="status">
    <p>{status.line}</p>
    {status.action && <button type="button" className="btn" disabled={busy} onClick={onAction}>{status.action === "retry" ? "Retry" : "Download"}</button>}
  </div>;
}

/** The project page's line while Context and Changes stay: the build's progress, or why the
 * IDE can't open on this platform with the build's one action, Download or Retry, or a failed
 * read of the IDE state with Retry. Inputs: the view, the failed read if the last one failed,
 * whether the action runs, and its handler. Output: the line, or nothing when the IDE opens. */
export function IdeUnavailable({ view, failure, busy = false, onAction }: { view: IdeView | undefined; failure?: Failure; busy?: boolean; onAction: () => void }) {
  const unavailable = failure ? ideReadFailed(failure, desktopPlatform()) : ideUnavailable(view);
  if (unavailable === null) return null;
  return <div className="row pj-ide-unavailable" role="status">
    <span className="secondary">{unavailable.line}</span>
    {unavailable.action && <Button size="small" pending={busy} disabled={busy} onClick={onAction}>{unavailable.action === "retry" ? "Retry" : "Download"}</Button>}
  </div>;
}

/** Closes the project's IDE when its page closes, and tells the owner when that failed. Inputs:
 * the project and the toast the page tells the owner through. Output: none. */
export function useCloseIde(projectId: string, notify: (line: string) => void) {
  useEffect(() => () => {
    void closeIde(projectId).then((result) => {
      if (result.kind !== "ok") notify(IDE_NOT_CLOSED);
    });
  }, [projectId]);
}

/** The project's IDE tab: the build state until the IDE is ready, then the IDE itself in a
 * native webview over the panel. The webview follows the panel's bounds, hides when the tab
 * is left or a dialog, drawer or menu opens over the page, and the engine hears every minute
 * that the owner still uses it. Every state it reads goes to `onView`, so the page swaps
 * Context and Changes back when the build fails. A failed move shows on the tab; a failed hide
 * goes to the page, whose line stays outside the box the IDE webview may still cover.
 * Inputs: the project id, the view handler and the hide's problem, null once it worked. Output:
 * the tab. */
export function IdeTab({ projectId, onView, onHideProblem }: { projectId: string; onView: (view: IdeView) => void; onHideProblem: (line: string | null) => void }) {
  const panel = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<IdeTabState | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ready = state !== null && ideStatus(state).ready;
  const covered = useOverlayShown();
  const waiting = state === null
    || (state.unqualified === null && (state.build.state === "downloading" || state.build.state === "verifying"));

  useEffect(() => {
    if (!waiting) return;
    let live = true;
    const read = async () => {
      const result = await ideView();
      if (!live) return;
      if (result.kind !== "ok") setProblem(failureLine(result));
      else {
        setProblem(null);
        setState(result.value.state);
        onView(result.value);
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), IDE_POLL_MS);
    return () => { live = false; window.clearInterval(timer); };
  }, [projectId, waiting]);

  useEffect(() => {
    const element = panel.current;
    if (!ready || !element || covered) return;
    let live = true;
    let shown = false;
    let last: IdeBounds | null = null;
    let frame = 0;
    let misplaced = false;
    const hide = () => void hideIde(projectId).then((result) => onHideProblem(result.kind === "ok" ? null : IDE_NOT_HIDDEN));
    const place = () => {
      if (!shown) return;
      const box = bounds(element.getBoundingClientRect());
      if (!box || sameBounds(box, last)) return;
      last = box;
      void placeIde(projectId, box).then((result) => {
        if (!live) return;
        if (result.kind !== "ok") {
          misplaced = true;
          setProblem(IDE_NOT_PLACED);
        } else if (misplaced) {
          misplaced = false;
          setProblem(null);
        }
      });
    };
    const follow = () => {
      if (!live || !shown) {
        frame = 0;
        return;
      }
      place();
      frame = window.requestAnimationFrame(follow);
    };
    const open = async () => {
      const box = bounds(element.getBoundingClientRect());
      if (!box) return;
      const result = await openIde(projectId, box);
      if (!live) {
        if (result.kind === "ok") hide();
        return;
      }
      if (result.kind === "error" && result.code === "ide_open_cancelled") return;
      if (result.kind !== "ok") {
        setProblem(failureLine(result, openFailure));
        return;
      }
      shown = true;
      last = null;
      setProblem(null);
      onHideProblem(null);
      if (frame === 0) follow();
    };
    void open();
    const beat = window.setInterval(() => {
      void touchIde(projectId).then((result) => {
        if (live && result.kind === "ok" && !result.value) void open();
      });
    }, HEARTBEAT_MS);
    return () => {
      live = false;
      window.cancelAnimationFrame(frame);
      window.clearInterval(beat);
      hide();
    };
  }, [projectId, ready, covered]);

  const act = async () => {
    setBusy(true);
    const result = await ideView(true);
    setBusy(false);
    if (result.kind !== "ok") setProblem(failureLine(result));
    else {
      setProblem(null);
      setState(result.value.state);
      onView(result.value);
    }
  };

  return <div className="ide-tab">
    {problem && <p className="ide-problem" role="alert">{problem}</p>}
    {state && !ready && <IdeStatus state={state} busy={busy} onAction={() => void act()} />}
    {!state && !problem && <p className="ide-status" role="status">Checking the IDE.</p>}
    <div ref={panel} className="ide-panel" aria-label="IDE" hidden={!ready} />
  </div>;
}
