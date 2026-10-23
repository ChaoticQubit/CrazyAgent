import { NotReady, PageHeader, type ProjectTab } from "../../ui";

/** The Project page. Stub owned by v1-desktop-projects, who replace it with the Overview,
 * Conversation, Agents, Context and Settings tabs. Inputs: the project id and current tab
 * from the route, and the project's name when the sidebar knows it. */
export function ProjectPage({ id, tab, name }: { id: string; tab: ProjectTab; name?: string }) {
  return <div className="stub-page" data-project={id} data-tab={tab}>
    <PageHeader title={name ?? "Project"} />
    <NotReady what="The project page" />
  </div>;
}
