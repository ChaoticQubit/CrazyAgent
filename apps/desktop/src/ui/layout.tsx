import type { ReactNode } from "react";
import { Icon, type IconName } from "./icon";

/** The page header: title, optional one-line description and an actions slot (one primary).
 * Inputs: title, description, actions. Output: a header with the page's h1. */
export function PageHeader({ title, description, actions }: { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  return <header className="page-header">
    <div><h1>{title}</h1>{description && <p>{description}</p>}</div>
    {actions && <div className="actions">{actions}</div>}
  </header>;
}

export interface TabLink {
  id: string;
  label: string;
  href: string;
  icon?: IconName;
}

/** Route tabs (role "tablist"): each tab is a link to its route, arrow keys and Home/End move
 * between tabs. Inputs: accessible label, tabs, the current tab id. */
export function Tabs({ label, tabs, current }: { label: string; tabs: TabLink[]; current: string }) {
  return <nav className="tabs" role="tablist" aria-label={label} onKeyDown={(event) => {
    const index = tabs.findIndex((tab) => tab.id === current);
    const target = event.key === "ArrowRight" ? index + 1 : event.key === "ArrowLeft" ? index - 1 :
      event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : undefined;
    if (target === undefined) return;
    event.preventDefault();
    const next = tabs[(target + tabs.length) % tabs.length];
    window.location.hash = next.href;
    requestAnimationFrame(() => document.getElementById(`tab-${next.id}`)?.focus());
  }}>
    {tabs.map((tab) => <a key={tab.id} id={`tab-${tab.id}`} role="tab" className="tab" href={tab.href}
      aria-selected={tab.id === current} tabIndex={tab.id === current ? 0 : -1}>
      {tab.icon && <Icon name={tab.icon} size={15} />}{tab.label}
    </a>)}
  </nav>;
}

/** A bordered panel with an optional header row. Inputs: title, header actions, body,
 * whether the body has padding, accessible label. */
export function Panel({ title, actions, children, padded = true, className }: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  padded?: boolean;
  className?: string;
}) {
  return <section className={`panel${className ? ` ${className}` : ""}`}>
    {(title || actions) && <div className="panel-head">{typeof title === "string" ? <h2>{title}</h2> : title}{actions}</div>}
    {padded ? <div className="panel-body">{children}</div> : children}
  </section>;
}
