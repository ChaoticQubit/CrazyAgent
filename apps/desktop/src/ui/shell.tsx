import type { ReactNode } from "react";
import { BrandMark } from "./brand";
import { Icon } from "./icon";
import { formatRoute, type Route } from "./router";

export { BrandMark };

export type EngineState = "connecting" | "running" | "down" | "outdated";

export interface SidebarProject {
  id: string;
  name: string;
  flag?: string;
}

/** The fixed 220 px sidebar as the approved design draws it: the brand, the project list alone
 * on top and scrolling by itself, a dividing line, then Home, New project, Settings and the
 * app-wide Metrics page, and a footer with the profile's initial, its name and the engine
 * state. Inputs: current route, projects, profile name, engine state and a retry action. */
export function Sidebar({ route, projects, profileName, engine, onRetry }: {
  route: Route;
  projects: SidebarProject[] | undefined;
  profileName: string | undefined;
  engine: EngineState;
  onRetry: () => void;
}) {
  const current = (match: boolean) => match ? "page" as const : undefined;
  const name = profileName || "Profile";
  return <aside className="sidebar" aria-label="CrazyAgent">
    <BrandMark />
    <nav className="nav nav-projects" aria-labelledby="nav-projects">
      <p className="nav-label" id="nav-projects">Projects</p>
      <div className="nav-group">
        {projects?.map((project) => <a key={project.id} className="nav-item" href={formatRoute({ name: "project", id: project.id, tab: "overview" })}
          aria-current={current(route.name === "project" && route.id === project.id)}>
          <Icon name="folder" /><span className="name">{project.name}</span>{project.flag && <span className="nav-flag">{project.flag}</span>}
        </a>)}
      </div>
    </nav>
    <nav className="nav app-nav" aria-label="App">
      <a className="nav-item" href={formatRoute({ name: "home" })} aria-current={current(route.name === "home")}>
        <Icon name="house" /><span className="name">Home</span>
      </a>
      <a className="nav-item" href={formatRoute({ name: "new-project", step: "basics" })} aria-current={current(route.name === "new-project")}>
        <Icon name="folder-plus" /><span className="name">New project</span>
      </a>
      <a className="nav-item" href={formatRoute({ name: "settings", tab: "accounts" })} aria-current={current(route.name === "settings" && route.tab !== "profile")}>
        <Icon name="settings" /><span className="name">Settings</span>
      </a>
      <a className="nav-item" href={formatRoute({ name: "metrics" })} aria-current={current(route.name === "metrics")}>
        <Icon name="trending-up" /><span className="name">Metrics</span>
      </a>
    </nav>
    <div className="sidebar-foot">
      <span className="avatar" aria-hidden="true">{Array.from(name)[0].toUpperCase()}</span>
      <span className="foot-text">
        <a className="me" href={formatRoute({ name: "settings", tab: "profile" })} aria-current={current(route.name === "settings" && route.tab === "profile")}>{name}</a>
        {engine === "running" ? <span className="engine-state running"><Icon name="server" size={13} />Engine running</span>
          : engine === "connecting" ? <span className="engine-state connecting"><Icon name="loader-circle" size={13} spin />Connecting...</span>
          : <span className="row"><span className="engine-state down"><Icon name="wifi-off" size={13} />Engine not running</span>
            <button type="button" className="btn quiet small" onClick={onRetry}>Try again</button></span>}
      </span>
    </div>
  </aside>;
}

const NOT_TEXT_INPUTS = new Set(["button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit"]);

function isEditableText(target: EventTarget | null): boolean {
  if (target instanceof HTMLTextAreaElement) return true;
  if (target instanceof HTMLInputElement) return !NOT_TEXT_INPUTS.has(target.type);
  return target instanceof HTMLElement && target.isContentEditable;
}

/** Keeps the web view's own right-click menu (Reload, Services, developer items) out of the
 * app: a right-click is cancelled unless it lands in editable text, where the native Cut,
 * Copy and Paste menu stays. Input: the document to guard. Installed once, at startup. */
export function blockWebContextMenu(target: Document): void {
  target.addEventListener("contextmenu", (event) => {
    if (!isEditableText(event.target)) event.preventDefault();
  });
}

/** The app frame: skip link, sidebar, and the scrolling main region holding the page, with
 * the toast region. Inputs: sidebar, page content, toast. */
export function AppShell({ sidebar, children, toast }: { sidebar: ReactNode; children: ReactNode; toast?: ReactNode }) {
  return <div className="shell">
    <button type="button" className="skip-link btn" onClick={() => document.getElementById("main")?.focus()}>Skip to content</button>
    {sidebar}
    <main id="main" className="main" tabIndex={-1}><div className="page">{children}</div></main>
    {toast}
  </div>;
}
