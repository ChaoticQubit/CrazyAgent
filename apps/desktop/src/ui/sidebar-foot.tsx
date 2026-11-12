import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Icon, type IconName } from "./icon";
import { ConfirmDialog } from "./overlay";
import { USER_MENU_TABS, type UserMenuTab } from "./router";

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

/** What the sidebar's footer needs to open the user menu's dialogs (DEC-151): the one open
 * now, if any, from the menu or a deep link, and the action that opens one. */
export interface UserMenuControl {
  open: UserMenuTab | null;
  onOpen: (tab: UserMenuTab) => void;
}

/** Each user menu item's words and icon, in menu order; a dialog takes the same words as its
 * title. */
export const USER_MENU_ITEMS: Record<UserMenuTab, { label: string; icon: IconName }> = {
  profile: { label: "Profile", icon: "user-round" },
  updates: { label: "Updates", icon: "sparkles" },
  github: { label: "GitHub", icon: "git-branch" },
  home: { label: "Projects folder", icon: "folder" },
};

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
 * the block, moving keyboard focus into it or clicking the name opens one menu: Profile,
 * Updates, GitHub and Projects folder, each opening its dialog, then Restart engine; a click
 * never navigates. Arrow keys move through the items, Esc closes the menu until the pointer
 * or focus leaves and comes back, and once a dialog or question it opened closes, focus rests
 * on the name with the menu closed. Restart engine asks first, and stays off while an update
 * install holds it or a restart runs. Without either control the block has no menu.
 * Inputs: the profile name, the engine state, the retry action, the restart control and the
 * user menu control. */
export function SidebarFoot({ name, engine, onRetry, restart, menu }: {
  name: string;
  engine: EngineState;
  onRetry: () => void;
  restart?: EngineRestartControl;
  menu?: UserMenuControl;
}) {
  assert(name.length > 0, "the footer names a profile");
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [asking, setAsking] = useState(false);
  const foot = useRef<HTMLDivElement>(null);
  const me = useRef<HTMLButtonElement>(null);
  const away = useRef(false);
  const keep = useRef(false);
  const hasMenu = restart !== undefined || menu !== undefined;
  const elsewhere = asking || (menu?.open ?? null) !== null;
  const menuShown = hasMenu && !elsewhere && (hovered || focused) && !dismissed;
  const off = restart === undefined || restart.held || restart.busy;
  const dismiss = () => {
    setDismissed(true);
    if (foot.current?.contains(document.activeElement)) me.current?.focus();
  };
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
    if (elsewhere) { away.current = true; return; }
    if (!away.current) return;
    away.current = false;
    setDismissed(true);
    const active = document.activeElement;
    if (active === null || active === document.body || foot.current?.contains(active)) {
      keep.current = true;
      me.current?.focus();
    }
  }, [elsewhere]);
  const move = (event: ReactKeyboardEvent) => {
    if (!menuShown || (event.key !== "ArrowDown" && event.key !== "ArrowUp")) return;
    const items = [...(foot.current?.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)") ?? [])];
    if (items.length === 0) return;
    event.preventDefault();
    const at = items.findIndex((item) => item === document.activeElement);
    const step = event.key === "ArrowDown" ? 1 : -1;
    const next = at < 0 ? (step > 0 ? 0 : items.length - 1) : (at + step + items.length) % items.length;
    items[next].focus();
  };
  const confirm = () => {
    setAsking(false);
    if (!off) restart?.onRestart();
  };
  return <div ref={foot} className="sidebar-foot"
    onMouseEnter={() => { setHovered(true); setDismissed(false); }}
    onMouseLeave={() => setHovered(false)}
    onFocus={() => {
      if (!focused) { setFocused(true); if (!keep.current) setDismissed(false); }
      keep.current = false;
    }}
    onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false); }}
    onKeyDown={move}>
    <span className="avatar" aria-hidden="true">{Array.from(name)[0].toUpperCase()}</span>
    <span className="foot-text">
      <span className="foot-name">
        {hasMenu
          ? <button ref={me} type="button" className="me" aria-haspopup="menu" aria-expanded={menuShown}
            onClick={() => setDismissed(false)}>{name}</button>
          : <span className="me">{name}</span>}
        {engine !== "running" && engine !== "connecting" && <button type="button" className="btn quiet small foot-retry" onClick={onRetry}>Try again</button>}
      </span>
      <EngineLine engine={engine} restarting={restart?.busy === true} />
      {restart?.failure && <span className="foot-error" role="alert">{restart.failure}</span>}
    </span>
    {menuShown && <div className="menu-list foot-menu" role="menu" aria-label={name}>
      {menu && USER_MENU_TABS.map((tab) => <button key={tab} type="button" role="menuitem" className="menu-item" data-item={tab}
        onClick={() => menu.onOpen(tab)}><Icon name={USER_MENU_ITEMS[tab].icon} size={14} />{USER_MENU_ITEMS[tab].label}</button>)}
      {restart && <button type="button" role="menuitem" className="menu-item" data-item="restart" disabled={off}
        onClick={() => setAsking(true)}><Icon name="refresh-cw" size={14} />Restart engine</button>}
      {restart?.held && <p className="foot-menu-note">{HELD_WORDS}</p>}
    </div>}
    <ConfirmDialog open={asking} title="Restart the engine?" confirmLabel="Restart engine" onConfirm={confirm} onCancel={() => setAsking(false)}>{RESTART_WORDS}</ConfirmDialog>
  </div>;
}
