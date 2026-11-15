import { useEffect, useRef, useState } from "react";
import { KEEP_AWAKE_CHANGED, KEEP_AWAKE_OPEN, KEEP_AWAKE_POLL_MS, loadKeepAwake, type KeepAwakeView } from "../../api-keep-awake";
import type { Loaded } from "../../state";
import { Icon } from "../../ui";
import { cover } from "../../ui/overlay";
import { keepAwakeLabel, keepAwakeTone } from "./model";
import { KeepAwakePanel } from "./panel";
import "./keep-awake.css";

/** The Keep awake control in the top bar, as a view of its state: a pill with the state's
 * label in the state's tone, named for a screen reader, that opens the panel. Before the
 * first read and without an engine it reads "Keep awake". Inputs: the view read, whether the
 * panel is open, now in milliseconds and the toggle. Output: the button. */
export function KeepAwakeControlView({ view, open, now, onToggle }: { view: Loaded<KeepAwakeView>; open: boolean; now: number; onToggle: () => void }) {
  const loaded = view.kind === "ok" ? view.value : null;
  const label = loaded ? keepAwakeLabel(loaded, now) : "Keep awake";
  const tone = loaded ? keepAwakeTone(loaded) : "plain";
  const name = loaded ? `Keep awake: ${label.charAt(0).toLowerCase()}${label.slice(1)}` : "Keep awake";
  return <button type="button" className={`keep-awake-control ${tone}`} data-tone={tone} aria-label={name} aria-expanded={open} aria-haspopup="dialog"
    aria-busy={view.kind === "loading" || undefined} onClick={onToggle}>
    <span className="keep-awake-dot" aria-hidden="true" />{label}<Icon name="chevron-down" size={16} />
  </button>;
}

/** Reads Keep awake for the control: every 5 s while the window is visible, at once when it
 * becomes visible, when the count of working agents changes, after a change made here and on
 * a retry; a hidden window does not read (DEC-167 addendum 6). Input: the count of working
 * agents. Output: the view read, when it was read and the retry. */
function useKeepAwake(work: number): [Loaded<KeepAwakeView>, number, () => void] {
  const [view, setView] = useState<Loaded<KeepAwakeView>>({ kind: "loading" });
  const [now, setNow] = useState(() => Date.now());
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let live = true;
    let timer = 0;
    let newest = 0;
    const read = async () => {
      window.clearTimeout(timer);
      if (document.visibilityState !== "visible") return;
      newest += 1;
      const mine = newest;
      const result = await loadKeepAwake();
      if (!live || mine !== newest) return;
      setView(result);
      setNow(Date.now());
      timer = window.setTimeout(() => void read(), KEEP_AWAKE_POLL_MS);
    };
    const again = () => void read();
    void read();
    document.addEventListener("visibilitychange", again);
    window.addEventListener(KEEP_AWAKE_CHANGED, again);
    return () => {
      live = false;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", again);
      window.removeEventListener(KEEP_AWAKE_CHANGED, again);
    };
  }, [tick, work]);
  return [view, now, () => setTick((before) => before + 1)];
}

/** The Keep awake control with its panel, left of the bell on every page. A click or a Keep
 * awake notification opens the panel; Escape, a click outside or its link closes it, and
 * while open it covers the page like a Drawer, so the IDE webview steps aside. Input: how
 * many agents work, a change of which reads the engine again. Output: the control and, when
 * open, the panel. */
export function KeepAwakeControl({ work }: { work: number }) {
  const [view, now, retry] = useKeepAwake(work);
  const [open, setOpen] = useState(false);
  const corner = useRef<HTMLDivElement>(null);
  useEffect(() => (open ? cover() : undefined), [open]);
  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(KEEP_AWAKE_OPEN, show);
    return () => window.removeEventListener(KEEP_AWAKE_OPEN, show);
  }, []);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setOpen(false);
      corner.current?.querySelector<HTMLButtonElement>("button.keep-awake-control")?.focus();
    };
    const onDown = (event: MouseEvent) => { if (!corner.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("mousedown", onDown); };
  }, [open]);
  return <div className="keep-awake-corner" ref={corner}>
    <KeepAwakeControlView view={view} open={open} now={now} onToggle={() => setOpen(!open)} />
    {open && <KeepAwakePanel view={view} now={now} onRetry={retry} onClose={() => setOpen(false)} />}
  </div>;
}
