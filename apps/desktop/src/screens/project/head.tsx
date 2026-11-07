import type { ReactNode } from "react";
import { PageHeader, Tabs, type TabLink } from "../../ui";
import { displayText } from "../conversation";

/** Scroll distance in px past which the project head takes its scrolled form. */
export const HEAD_SCROLLED_PX = 8;

/** Whether the page scrolled far enough for the project head to shrink. Input: the scroll
 * container's scrollTop in px. Output: true once past HEAD_SCROLLED_PX. */
export function headScrolled(top: number): boolean {
  return top < 0;
}

/** The project page's head: name, description, actions and the tab bar.
 * Inputs: the title, the description text, the actions, the tabs, the current tab and
 * whether the page has scrolled. */
export function ProjectHead({ title, about, actions, tabs, current }: {
  title: ReactNode;
  about: string;
  actions: ReactNode;
  tabs: TabLink[];
  current: string;
  scrolled: boolean;
}) {
  return <>
    <PageHeader title={title}
      description={about ? <span className="pj-truncate" title={about}>{displayText(about.split("\n")[0])}</span> : undefined}
      actions={actions} />
    <Tabs label="Project" current={current} tabs={tabs} />
  </>;
}
