import { useEffect, useRef, useState, type ReactNode } from "react";
import { PageHeader, Tabs, type TabLink } from "../../ui";
import { displayText } from "../conversation";

/** Scroll distance in px past which the project head takes its scrolled form. */
export const HEAD_SCROLLED_PX = 8;

/** Whether the page scrolled far enough for the project head to shrink. Input: the scroll
 * container's scrollTop in px. Output: true once past HEAD_SCROLLED_PX. */
export function headScrolled(top: number): boolean {
  return top > HEAD_SCROLLED_PX;
}

/** The project's description: wraps to two lines, one once the page scrolls, and offers
 * "more" while the wrapped text is cut, which opens the whole text and turns into "less".
 * Input: the description text. */
function ProjectAbout({ about }: { about: string }) {
  const text = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const [cut, setCut] = useState(false);
  useEffect(() => {
    const node = text.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const measure = () => setCut(node.scrollHeight > node.clientHeight + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [about]);
  return <span className="pj-desc-wrap">
    <span id="pj-desc" className={open ? "pj-desc open" : "pj-desc"} ref={text}>{displayText(about)}</span>
    <button type="button" className="pj-desc-more" aria-expanded={open} aria-controls="pj-desc" hidden={!open && !cut}
      onClick={() => setOpen(!open)}>{open ? "less" : "more"}</button>
  </span>;
}

/** The project page's head, which stays on screen while the page scrolls: name,
 * description, actions and the tab bar. Inputs: the title, the description text, the
 * actions, the tabs, the current tab and whether the page has scrolled. */
export function ProjectHead({ title, about, actions, tabs, current, scrolled }: {
  title: ReactNode;
  about: string;
  actions: ReactNode;
  tabs: TabLink[];
  current: string;
  scrolled: boolean;
}) {
  return <div className={scrolled ? "pj-head scrolled" : "pj-head"}>
    <PageHeader title={title} description={about ? <ProjectAbout about={about} /> : undefined} actions={actions} />
    <Tabs label="Project" current={current} tabs={tabs} />
  </div>;
}
