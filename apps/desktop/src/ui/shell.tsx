import type { ReactNode } from "react";
import { BrandMark } from "./brand";
import { Icon } from "./icon";
import { formatRoute, type Route } from "./router";

export { BrandMark };

export type EngineState = "connecting" | "running" | "down";

export interface SidebarProject {
  id: string;
  name: string;
  flag?: string;
}

/** The fixed 220 px sidebar: brand, Home, the project list with New project, Settings, and a
 * footer with the profile name and the engine state. Inputs: current route, projects,
 * profile name, engine state and a retry action. */
export function Sidebar({ route, projects, profileName, engine, onRetry }: {
  route: Route;
  projects: SidebarProject[] | undefined;
  profileName: string | undefined;
  engine: EngineState;
  onRetry: () => void;
}) {
  const current = (match: boolean) => match ? "page" as const : undefined;
  return <aside className="sidebar" aria-label="CrazyAgent">
    <BrandMark />
    <nav className="nav" aria-label="Main">
      <a className="nav-item" href={formatRoute({ name: "home" })} aria-current={current(route.name === "home")}>
        <Icon name="house" /><span className="name">Home</span>
      </a>
      <p className="nav-label" id="nav-projects">Projects</p>
      <div className="nav-group" role="group" aria-labelledby="nav-projects">
        {projects?.map((project) => <a key={project.id} className="nav-item" href={formatRoute({ name: "project", id: project.id, tab: "overview" })}
          aria-current={current(route.name === "project" && route.id === project.id)}>
          <Icon name="folder" /><span className="name">{project.name}</span>{project.flag && <span className="nav-flag">{project.flag}</span>}
        </a>)}
        <a className="nav-item nav-add" href={formatRoute({ name: "new-project", step: "basics" })} aria-current={current(route.name === "new-project")}>
          <Icon name="folder-plus" /><span className="name">New project</span>
        </a>
      </div>
      <a className="nav-item" href={formatRoute({ name: "settings", tab: "accounts" })} aria-current={current(route.name === "settings" && route.tab !== "profile")}>
        <Icon name="settings" /><span className="name">Settings</span>
      </a>
    </nav>
    <div className="sidebar-foot">
      <a className="me" href={formatRoute({ name: "settings", tab: "profile" })} aria-current={current(route.name === "settings" && route.tab === "profile")}>
        <Icon name="user-round" />{profileName || "Profile"}
      </a>
      {engine === "running" ? <span className="engine-state running"><Icon name="server" size={13} />Engine running</span>
        : engine === "connecting" ? <span className="engine-state connecting"><Icon name="loader-circle" size={13} spin />Connecting...</span>
        : <span className="row"><span className="engine-state down"><Icon name="wifi-off" size={13} />Engine not running</span>
          <button type="button" className="btn quiet small" onClick={onRetry}>Try again</button></span>}
    </div>
  </aside>;
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
