import { useEffect, useId, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { Button } from "./button";
import { Icon, type IconName } from "./icon";

let covering = 0;
const watchers = new Set<() => void>();

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Counts one open dialog, drawer or menu over the page, so a native view above the page,
 * the IDE webview, steps aside until the last one closes. Output: the release, which counts
 * once however often it is called. */
export function cover(): () => void {
  let held = true;
  covering += 1;
  if (covering === 1) for (const watcher of watchers) watcher();
  return () => {
    if (!held) return;
    held = false;
    covering -= 1;
    assert(covering >= 0, "an overlay released more often than it covered");
    if (covering === 0) for (const watcher of watchers) watcher();
  };
}

/** Whether any dialog, drawer or menu is open over the page now. Output: true while one is. */
export function overlayShown(): boolean {
  return covering > 0;
}

function watch(watcher: () => void): () => void {
  watchers.add(watcher);
  return () => { watchers.delete(watcher); };
}

/** Follows whether any dialog, drawer or menu is open over the page. Output: true while one
 * is, and the component renders again when that changes. */
export function useOverlayShown(): boolean {
  return useSyncExternalStore(watch, overlayShown, overlayShown);
}

function useCover(open: boolean) {
  useEffect(() => (open ? cover() : undefined), [open]);
}

function useModal(open: boolean, returnFocus?: () => HTMLElement | null) {
  const ref = useRef<HTMLDialogElement>(null);
  useCover(open);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog || !open) return;
    const active = document.activeElement;
    const opener = active instanceof HTMLElement && active !== document.body ? active : null;
    if (!dialog.open) dialog.showModal();
    const first = dialog.querySelector<HTMLElement>("[data-autofocus], input:not([type=hidden]), textarea, select");
    first?.focus();
    return () => {
      if (dialog.open) dialog.close();
      (opener?.isConnected ? opener : returnFocus?.())?.focus();
    };
  }, [open]);
  return ref;
}

/** A modal dialog on the native dialog element: traps focus, starts on the first field, Esc
 * asks this dialog's onClose alone, and focus returns to the opener. While busy, Back, Close and
 * Esc do nothing.
 * Inputs: open, title, close handler, optional Back handler, body, footer, wide layout, busy. */
export function Dialog({ open, title, onClose, onBack, children, footer, wide = false, busy = false }: {
  open: boolean;
  title: ReactNode;
  onClose: () => void;
  onBack?: () => void;
  children: ReactNode;
  footer?: ReactNode;
  wide?: boolean;
  busy?: boolean;
}) {
  const ref = useModal(open);
  const titleId = useId();
  if (!open) return null;
  return <dialog ref={ref} className={`dialog${wide ? " wide" : ""}`} aria-labelledby={titleId} aria-busy={busy || undefined}
    onCancel={(event) => { event.preventDefault(); event.stopPropagation(); if (!busy) onClose(); }}>
    <div className="dialog-head">
      {onBack && <Button iconOnly="Back" icon="arrow-left" disabled={busy} onClick={onBack} />}
      <h2 id={titleId}>{title}</h2>
      <Button iconOnly="Close" icon="x" disabled={busy} onClick={onClose} />
    </div>
    <div className="dialog-body">{children}</div>
    {footer && <div className="dialog-foot">{footer}</div>}
  </dialog>;
}

/** A right-side drawer (480 px) on the native dialog element; Esc closes it.
 * Inputs: open, title, close handler, body, footer, and where focus goes when the element
 * that opened the drawer has left the page. */
export function Drawer({ open, title, onClose, children, footer, returnFocus }: {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  returnFocus?: () => HTMLElement | null;
}) {
  const ref = useModal(open, returnFocus);
  const titleId = useId();
  if (!open) return null;
  return <dialog ref={ref} className="drawer" aria-labelledby={titleId} onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <div className="dialog-head"><h2 id={titleId}>{title}</h2><Button iconOnly="Close" icon="x" onClick={onClose} /></div>
    <div className="dialog-body">{children}</div>
    {footer && <div className="dialog-foot">{footer}</div>}
  </dialog>;
}

/** A confirmation dialog for a destructive or irreversible step. While the step runs, Cancel,
 * Close and Esc are off, so the dialog can't claim the step was stopped.
 * Inputs: open, title, body, confirm label, cancel label, pending flag, handlers. */
export function ConfirmDialog({ open, title, children, confirmLabel, cancelLabel = "Cancel", pending = false, onConfirm, onCancel }: {
  open: boolean;
  title: string;
  children: ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  pending?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return <Dialog open={open} title={title} onClose={onCancel} busy={pending}
    footer={<><Button disabled={pending} onClick={onCancel} data-autofocus>{cancelLabel}</Button><Button variant="danger" pending={pending} onClick={onConfirm}>{confirmLabel}</Button></>}>
    <p>{children}</p>
  </Dialog>;
}

export interface MenuItem {
  label: string;
  icon?: IconName;
  danger?: boolean;
  onSelect: () => void;
}

/** Whether focus leaving a menu went somewhere outside it. A webview that moves focus to an
 * ancestor on mousedown, as WKWebView does with buttons, or to nothing has not left it.
 * Inputs: the menu element and the element taking focus. Output: true when the menu should close. */
function focusLeftMenu(menu: Node, next: Node | null): boolean {
  return next !== null && !menu.contains(next) && !next.contains(menu);
}

/** A button that opens a small menu; arrow keys move, Esc closes and returns focus, and a
 * pointerdown or focus outside closes it. Inputs: accessible label for the button and the items. */
export function Menu({ label, items }: { label: string; items: MenuItem[] }) {
  const [open, setOpen] = useState(false);
  useCover(open);
  const root = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => { if (open) list.current?.querySelector<HTMLElement>("[role=menuitem]")?.focus(); }, [open]);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => { if (!(event.target instanceof Node && root.current?.contains(event.target))) setOpen(false); };
    document.addEventListener("pointerdown", outside, true);
    return () => document.removeEventListener("pointerdown", outside, true);
  }, [open]);
  const close = () => { setOpen(false); button.current?.focus(); };
  return <span ref={root} className="menu" onBlur={(event) => { if (focusLeftMenu(event.currentTarget, event.relatedTarget as Node | null)) setOpen(false); }}>
    <button ref={button} type="button" className="btn icon-only" aria-label={label} title={label} aria-haspopup="menu" aria-expanded={open}
      onClick={() => setOpen(!open)}><Icon name="ellipsis" /></button>
    {open && <div ref={list} className="menu-list" role="menu" aria-label={label} onKeyDown={(event) => {
      const entries = [...(list.current?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
      const index = entries.indexOf(document.activeElement as HTMLElement);
      if (event.key === "Escape") { event.preventDefault(); close(); }
      if (event.key === "ArrowDown") { event.preventDefault(); entries[(index + 1) % entries.length]?.focus(); }
      if (event.key === "ArrowUp") { event.preventDefault(); entries[(index - 1 + entries.length) % entries.length]?.focus(); }
    }}>
      {items.map((item) => <button key={item.label} type="button" role="menuitem" className={`menu-item${item.danger ? " danger" : ""}`}
        onClick={() => { setOpen(false); item.onSelect(); }}>{item.icon && <Icon name={item.icon} size={14} />}{item.label}</button>)}
    </div>}
  </span>;
}
