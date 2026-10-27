import { useEffect, useRef, useState } from "react";
import type { Failure } from "../../api";
import { HEARTBEAT_MS, bounds, hideIde, ideStatus, ideView, openFailure, openIde, placeIde, touchIde } from "../../api-ide";
import type { IdeTabState } from "../../api-ide";
import "./ide.css";

const POLL_MS = 1_000;
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

/** The project's IDE tab: the build state until the IDE is ready, then the IDE itself in a
 * native webview over the panel. The webview follows the panel's bounds, hides when the tab
 * is left, and the engine hears every minute that the owner still uses it.
 * Input: the project id. Output: the tab. */
export function IdeTab({ projectId }: { projectId: string }) {
  const panel = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<IdeTabState | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ready = state !== null && ideStatus(state).ready;
  const waiting = state === null
    || (state.unqualified === null && (state.build.state === "downloading" || state.build.state === "verifying"));

  useEffect(() => {
    if (!waiting) return;
    let live = true;
    const read = async () => {
      const result = await ideView();
      if (!live) return;
      if (result.kind !== "ok") setProblem(failureLine(result));
      else if (result.value.tab === "on") {
        setProblem(null);
        setState(result.value.state);
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => { live = false; window.clearInterval(timer); };
  }, [projectId, waiting]);

  useEffect(() => {
    const element = panel.current;
    if (!ready || !element) return;
    let live = true;
    let shown = false;
    const place = () => {
      const box = bounds(element.getBoundingClientRect());
      if (box && shown) void placeIde(projectId, box);
    };
    const open = async () => {
      const box = bounds(element.getBoundingClientRect());
      if (!box) return;
      const result = await openIde(projectId, box);
      if (!live) {
        if (result.kind === "ok") void hideIde(projectId);
        return;
      }
      if (result.kind !== "ok") {
        setProblem(failureLine(result, openFailure));
        return;
      }
      shown = true;
      setProblem(null);
      place();
    };
    void open();
    const observer = new ResizeObserver(place);
    observer.observe(element);
    window.addEventListener("resize", place);
    const beat = window.setInterval(() => {
      void touchIde(projectId).then((result) => {
        if (live && result.kind === "ok" && !result.value) void open();
      });
    }, HEARTBEAT_MS);
    return () => {
      live = false;
      observer.disconnect();
      window.removeEventListener("resize", place);
      window.clearInterval(beat);
      void hideIde(projectId);
    };
  }, [projectId, ready]);

  const act = async () => {
    setBusy(true);
    const result = await ideView(true);
    setBusy(false);
    if (result.kind !== "ok") setProblem(failureLine(result));
    else if (result.value.tab === "on") {
      setProblem(null);
      setState(result.value.state);
    }
  };

  return <div className="ide-tab">
    {problem && <p className="ide-problem" role="alert">{problem}</p>}
    {state && !ready && <IdeStatus state={state} busy={busy} onAction={() => void act()} />}
    {!state && !problem && <p className="ide-status" role="status">Checking the IDE.</p>}
    <div ref={panel} className="ide-panel" aria-label="IDE" hidden={!ready} />
  </div>;
}
