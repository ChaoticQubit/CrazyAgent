import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { PageHeader, Tabs, type TabLink } from "../../ui";
import { displayText } from "../conversation";

/** Scroll distance in px past which the project head takes its scrolled form. */
export const HEAD_SCROLLED_PX = 8;

/** Scroll distance in px a scrolled head must fall to or under before it takes its full
 * form again, below HEAD_SCROLLED_PX so the head never flips back and forth at one point. */
export const HEAD_GROWN_PX = 2;

/** Whether the project head takes its scrolled form. Inputs: the scroll container's
 * scrollTop in px and whether the head is scrolled now. Output: true once past
 * HEAD_SCROLLED_PX, and for a scrolled head until it is back at HEAD_GROWN_PX or under. */
export function headScrolled(top: number, was = false): boolean {
  return top > (was ? HEAD_GROWN_PX : HEAD_SCROLLED_PX);
}

/** The height a scrolled head gives up, kept as room under the page so the scroll range
 * never shrinks with the head and pulls the page back to the top. Inputs: the head's full
 * height, its height now and whether it is scrolled. Output: the room in whole px. */
export function headRoom(full: number, now: number, scrolled: boolean): number {
  return scrolled ? Math.max(0, Math.ceil(full - now)) : 0;
}

/** Follows the page scroll for the project head: its scrolled form, its height as
 * --pj-head-h on the scroll container for scroll-padding-top, and the room it gives up as
 * --pj-head-room on the page. Inputs: the project page's root and the project id.
 * Output: whether the head is scrolled. */
export function useProjectHead(root: RefObject<HTMLDivElement | null>, project: string): boolean {
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    const node = root.current;
    const page = node?.closest<HTMLElement>(".main");
    const head = node?.querySelector<HTMLElement>(".pj-head");
    if (!node || !page || !head) return;
    let was = false;
    let full = head.offsetHeight;
    const sync = () => {
      const height = head.offsetHeight;
      if (!was) full = height;
      page.style.setProperty("--pj-head-h", `${height}px`);
      node.style.setProperty("--pj-head-room", `${headRoom(full, height, was)}px`);
    };
    const follow = () => {
      was = headScrolled(page.scrollTop, was);
      setScrolled(was);
      sync();
    };
    follow();
    page.addEventListener("scroll", follow, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(sync);
    observer?.observe(head);
    return () => {
      page.removeEventListener("scroll", follow);
      observer?.disconnect();
      page.style.removeProperty("--pj-head-h");
      node.style.removeProperty("--pj-head-room");
    };
  }, [project]);
  return scrolled;
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
