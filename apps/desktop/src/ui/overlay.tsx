import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Button } from "./button";
import { Icon, type IconName } from "./icon";

function useModal(open: boolean) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog || !open) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!dialog.open) dialog.showModal();
    const first = dialog.querySelector<HTMLElement>("[data-autofocus], input:not([type=hidden]), textarea, select");
    first?.focus();
    return () => {
      if (dialog.open) dialog.close();
      opener?.focus();
    };
  }, [open]);
  return ref;
}

/** A modal dialog on the native dialog element: traps focus, starts on the first field, Esc
 * asks onClose, and focus returns to the opener. Inputs: open, title, close handler, optional
 * Back handler, body, footer, wide layout. */
export function Dialog({ open, title, onClose, onBack, children, footer, wide = false }: {
  open: boolean;
  title: string;
  onClose: () => void;
  onBack?: () => void;
  children: ReactNode;
  footer?: ReactNode;
  wide?: boolean;
}) {
  const ref = useModal(open);
  const titleId = useId();
  if (!open) return null;
  return <dialog ref={ref} className={`dialog${wide ? " wide" : ""}`} aria-labelledby={titleId}
    onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <div className="dialog-head">
      {onBack && <Button iconOnly="Back" icon="arrow-left" onClick={onBack} />}
      <h2 id={titleId}>{title}</h2>
      <Button iconOnly="Close" icon="x" onClick={onClose} />
    </div>
    <div className="dialog-body">{children}</div>
    {footer && <div className="dialog-foot">{footer}</div>}
  </dialog>;
}

/** A right-side drawer (480 px) on the native dialog element; Esc closes it.
 * Inputs: open, title, close handler, body, footer. */
export function Drawer({ open, title, onClose, children, footer }: {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const ref = useModal(open);
  const titleId = useId();
  if (!open) return null;
  return <dialog ref={ref} className="drawer" aria-labelledby={titleId} onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <div className="dialog-head"><h2 id={titleId}>{title}</h2><Button iconOnly="Close" icon="x" onClick={onClose} /></div>
    <div className="dialog-body">{children}</div>
    {footer && <div className="dialog-foot">{footer}</div>}
  </dialog>;
}

/** A confirmation dialog for a destructive or irreversible step. Inputs: open, title, body,
 * confirm label, cancel label, pending flag, handlers. */
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
  return <Dialog open={open} title={title} onClose={onCancel}
    footer={<><Button onClick={onCancel} data-autofocus>{cancelLabel}</Button><Button variant="danger" pending={pending} onClick={onConfirm}>{confirmLabel}</Button></>}>
    <p>{children}</p>
  </Dialog>;
}

export interface MenuItem {
  label: string;
  icon?: IconName;
  danger?: boolean;
  onSelect: () => void;
}

/** A button that opens a small menu; arrow keys move, Esc closes and returns focus.
 * Inputs: accessible label for the button and the items. */
export function Menu({ label, items }: { label: string; items: MenuItem[] }) {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => { if (open) list.current?.querySelector<HTMLElement>("[role=menuitem]")?.focus(); }, [open]);
  const close = () => { setOpen(false); button.current?.focus(); };
  return <span className="menu" onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false); }}>
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
