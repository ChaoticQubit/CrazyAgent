import { NotReady, PageHeader, type NewProjectStep } from "../../ui";

/** The New project flow. Stub owned by v1-desktop-projects, who replace it with the five
 * steps. Input: the current step from the route. Output: the Not ready yet state. */
export function NewProjectFlow({ step }: { step: NewProjectStep }) {
  return <div className="stub-page" data-step={step}>
    <PageHeader title="New project" />
    <NotReady what="Creating projects" />
  </div>;
}
