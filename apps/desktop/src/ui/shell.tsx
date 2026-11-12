import type { ReactNode } from "react";
import { BrandMark } from "./brand";
import { Icon } from "./icon";
import { formatRoute, type Route } from "./router";
import { SidebarFoot, type EngineRestartControl, type EngineState, type UserMenuControl } from "./sidebar-foot";

export { BrandMark };

export type { EngineRestartControl, EngineState, UserMenuControl };

/** Says how many agents are working. Input: the count, above zero. Output: the words. */
export function agentsWorking(count: number): string {
  return `${count} ${count === 1 ? "agent" : "agents"} working`;
}

export interface SidebarProject {
  id: string;
  name: string;
  flag?: string;
  working?: number;
}

/** A project row's alert count: the number on screen and, after it, the words a screen
 * reader needs to know what the number counts. Input: the count as text. Output: the flag. */
function AlertFlag({ flag }: { flag: string }) {
  return <><span className="nav-flag">{flag}</span><span className="visually-hidden">{flag === "1" ? "alert" : "alerts"}</span></>;
}

/** A project row's running-agent count: the number on screen, the words for a screen reader
 * and the tooltip. Input: how many agents are working, above zero. Output: the count. */
function WorkingCount({ count }: { count: number }) {
  const words = agentsWorking(count);
  return <span className="nav-live" title={words}><span aria-hidden="true">{count}</span><span className="visually-hidden">{words}</span></span>;
}

/** The fixed 220 px sidebar as the approved design draws it: the brand, the project list alone
 * on top and scrolling by itself, a dividing line, then Home, New project, Settings and the
 * app-wide Metrics page, and a footer with the profile's initial, its name, the engine
 * state and, on hover, keyboard focus or a click on the name, the user menu. Inputs: current
 * route, projects, profile name, engine state, a retry action, the restart control and the
 * user menu's dialogs; without both the footer has no menu. */
export function Sidebar({ route, projects, profileName, engine, onRetry, restart, menu }: {
  route: Route;
  projects: SidebarProject[] | undefined;
  profileName: string | undefined;
  engine: EngineState;
  onRetry: () => void;
  restart?: EngineRestartControl;
  menu?: UserMenuControl;
}) {
  const current = (match: boolean) => match ? "page" as const : undefined;
  return <aside className="sidebar" aria-label="CrazyAgent">
    <BrandMark />
    <nav className="nav nav-projects" aria-labelledby="nav-projects">
      <p className="nav-label" id="nav-projects">Projects</p>
      <div className="nav-group">
        {projects?.map((project) => <a key={project.id} className="nav-item" href={formatRoute({ name: "project", id: project.id, tab: "overview" })}
          aria-current={current(route.name === "project" && route.id === project.id)}>
          {project.working ? <span className="nav-live-dot" aria-hidden="true" /> : <Icon name="folder" />}<span className="name">{project.name}</span>
          {project.working ? <WorkingCount count={project.working} /> : null}{project.flag && <AlertFlag flag={project.flag} />}
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
      <a className="nav-item" href={formatRoute({ name: "settings", tab: "accounts" })} aria-current={current(route.name === "settings")}>
        <Icon name="settings" /><span className="name">Settings</span>
      </a>
      <a className="nav-item" href={formatRoute({ name: "metrics" })} aria-current={current(route.name === "metrics")}>
        <Icon name="trending-up" /><span className="name">Metrics</span>
      </a>
    </nav>
    <SidebarFoot name={profileName || "Profile"} engine={engine} onRetry={onRetry} restart={restart} menu={menu} />
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
