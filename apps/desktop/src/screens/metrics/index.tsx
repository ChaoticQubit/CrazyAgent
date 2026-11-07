import { useState } from "react";
import { PageHeader, type SidebarProject } from "../../ui";
import { MetricsView } from "./view";

/** The app Metrics page from the bottom of the sidebar: the project Metrics tab's tiles, chart
 * and breakdown for every project on the engine, with a filter for one project. A picked
 * project that leaves the list falls back to every project. Input: the engine's projects. */
export function AppMetrics({ projects }: { projects: SidebarProject[] }) {
  const [picked, setPicked] = useState<string | null>(null);
  const projectId = picked !== null && projects.some((project) => project.id === picked) ? picked : null;
  return <>
    <PageHeader title="Metrics" description="Every project on this engine in one place." />
    <MetricsView projectId={projectId} filter={{ projects, onProject: setPicked }} />
  </>;
}
