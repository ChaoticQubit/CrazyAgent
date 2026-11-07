import { createElement as h, useRef } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import { ProjectHead, useProjectHead } from "../../src/screens/project/head.tsx";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

const STILL = `
.pj-head, .pj-head * { transition: none !important; }
.pj-head .page-header, .pj-head.scrolled .page-header { padding: 20px 0 16px !important; }
.pj-head .page-header h1, .pj-head.scrolled .page-header h1 { font-size: 22px !important; line-height: 28px !important; }
`;

let root = null;

/** A project page as the app draws one: the real head and its scroll hook over a long body.
 * Output: the page element. */
function Page() {
  const page = useRef(null);
  const scrolled = useProjectHead(page, "p1");
  return h("div", { className: "pj pj-project", ref: page },
    h(ProjectHead, { title: "Larkspur Booking", about: "", actions: null, tabs: [{ id: "overview", label: "Overview", href: "#/projects/p1/overview" }], current: "overview", scrolled }),
    h("div", { style: { height: "2000px" } }, "Overview"));
}

/** Mounts the real app shell with the real stylesheets around a project page whose head
 * takes its scrolled form with no transition and no change of size. */
export function mountStill() {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, STILL].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [{ id: "p1", name: "Project 1" }], profileName: "Mira Okafor", engine: "running", onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, h(Page))));
}

export function frames(count = 2) {
  return new Promise((done) => {
    const step = (left) => (left === 0 ? done() : requestAnimationFrame(() => step(left - 1)));
    step(count);
  });
}

/** Reads the head's form and height and the room under the page. Output: the values. */
export function state() {
  const head = document.querySelector(".pj-head");
  const page = document.querySelector(".pj-project");
  return {
    head: head.className,
    height: head.offsetHeight,
    room: parseFloat(getComputedStyle(page).getPropertyValue("--pj-head-room")) || 0,
    top: document.querySelector(".main").scrollTop,
  };
}
