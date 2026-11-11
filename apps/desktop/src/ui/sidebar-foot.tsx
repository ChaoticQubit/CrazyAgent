import { useEffect, useRef, useState } from "react";
import { Icon } from "./icon";
import { ConfirmDialog } from "./overlay";
import { formatRoute } from "./router";

export type EngineState = "connecting" | "running" | "down" | "outdated";

/** What the sidebar's footer needs to restart the engine: whether an update install holds
 * the restart off, whether a restart runs now, the last failed restart's plain words, empty
 * when none, and the action that restarts. */
export interface EngineRestartControl {
  held: boolean;
  busy: boolean;
  failure: string;
  onRestart: () => void;
}

const RESTART_WORDS = "This stops the live master and every running agent. The engine comes back in a few seconds. Your projects, conversations and settings are kept.";

const HELD_WORDS = "Waits until the update install ends.";

/** Stops on a state this file holds impossible. Inputs: the condition and what it guards.
 * Output: nothing, or a thrown error. */
function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The engine's state in words, a running restart first. Inputs: the engine state and whether
 * a restart runs. Output: the line. */
function EngineLine({ engine, restarting }: { engine: EngineState; restarting: boolean }) {
  if (restarting) return <span className="engine-state connecting"><Icon name="loader-circle" size={13} spin />Restarting engine...</span>;
  if (engine === "running") return <span className="engine-state running"><Icon name="server" size={13} />Engine running</span>;
  if (engine === "connecting") return <span className="engine-state connecting"><Icon name="loader-circle" size={13} spin />Connecting...</span>;
  return <span className="engine-state down"><Icon name="wifi-off" size={13} />Engine not running</span>;
}

/** The sidebar's footer: the profile's initial, its name and the engine state. Pointing at
 * the block or moving keyboard focus into it opens a menu with Restart engine; Esc, or an
 * answered question, closes the menu until the pointer or focus leaves and comes back, and
 * focus inside the block rests on the profile name. Restart engine asks first, and stays off while an update
 * install holds it or a restart runs. Without a restart control the block has no menu.
 * Inputs: the profile name, the engine state, the retry action and the restart control. */
export function SidebarFoot({ name, engine, onRetry, restart }: {
  name: string;
  engine: EngineState;
  onRetry: () => void;
  restart?: EngineRestartControl;
}) {
  assert(name.length > 0, "the footer names a profile");
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [asking, setAsking] = useState(false);
  const foot = useRef<HTMLDivElement>(null);
  const profile = useRef<HTMLAnchorElement>(null);
  const asked = useRef(false);
  const open = restart !== undefined && (asking || ((hovered || focused) && !dismissed));
  const off = restart === undefined || restart.held || restart.busy;
  const dismiss = () => {
    setDismissed(true);
    if (foot.current?.contains(document.activeElement)) profile.current?.focus();
  };
  const menuShown = open && !asking;
  useEffect(() => {
    if (!menuShown) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      dismiss();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [menuShown]);
  useEffect(() => {
    if (asking) asked.current = true;
    if (asking || !asked.current) return;
    asked.current = false;
    dismiss();
  }, [asking]);
  const confirm = () => {
    setAsking(false);
    if (!off) restart?.onRestart();
  };
  return <div ref={foot} className="sidebar-foot"
    onMouseEnter={() => { setHovered(true); setDismissed(false); }}
    onMouseLeave={() => setHovered(false)}
    onFocus={() => { if (!focused) { setFocused(true); setDismissed(false); } }}
    onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false); }}>
    <span className="avatar" aria-hidden="true">{Array.from(name)[0].toUpperCase()}</span>
    <span className="foot-text">
      <span className="foot-name">
        <a ref={profile} className="me" href={formatRoute({ name: "settings", tab: "profile" })}>{name}</a>
        {engine !== "running" && engine !== "connecting" && <button type="button" className="btn quiet small foot-retry" onClick={onRetry}>Try again</button>}
      </span>
      <EngineLine engine={engine} restarting={restart?.busy === true} />
      {restart?.failure && <span className="foot-error" role="alert">{restart.failure}</span>}
    </span>
    {open && <div className="menu-list foot-menu" role="menu" aria-label="Engine">
      <button type="button" role="menuitem" className="menu-item" disabled={off} onClick={() => setAsking(true)}><Icon name="refresh-cw" size={14} />Restart engine</button>
      {restart?.held && <p className="foot-menu-note">{HELD_WORDS}</p>}
    </div>}
    <ConfirmDialog open={asking} title="Restart the engine?" confirmLabel="Restart engine" onConfirm={confirm} onCancel={() => setAsking(false)}>{RESTART_WORDS}</ConfirmDialog>
  </div>;
}
