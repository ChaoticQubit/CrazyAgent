import type { ReactNode } from "react";
import type { Route } from "./router";
import "./top-bar.css";

/** The name the top bar shows for a route: the page, or the project's name on a project's
 * pages. Inputs: the route and the projects with their names. Output: the name, "Project"
 * for a project the list does not have. */
export function pageName(route: Route, projects: readonly { id: string; name: string }[]): string {
  switch (route.name) {
    case "welcome": return "Welcome";
    case "home": return "Home";
    case "metrics": return "Metrics";
    case "settings": return "Settings";
    case "demo": return "Demo";
    case "new-project":
    case "add-project": return "New project";
    case "project":
    case "onboarding": return projects.find((project) => project.id === route.id)?.name ?? "Project";
  }
}

/** The bar above every page (DEC-167 addendum 5): the page's name on the left as plain text,
 * so the page keeps its one heading, then the Keep awake control and, right of it, the
 * notification bell. Inputs: the page's name, the Keep awake control and the bell. Output:
 * the bar. */
export function TopBar({ title, keepAwake, bell }: { title: string; keepAwake: ReactNode; bell: ReactNode }) {
  return <header className="top-bar">
    <span className="top-bar-title">{title}</span>
    <div className="top-bar-tools">{keepAwake}{bell}</div>
  </header>;
}
