import type { SidebarProject } from "../../ui/shell";

/** The app Metrics page, not built yet. Input: the engine's projects. */
export function AppMetrics({ projects }: { projects: SidebarProject[] }) {
  return <div>{projects.length}</div>;
}
