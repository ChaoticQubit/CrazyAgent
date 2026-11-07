import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import * as conversation from "../../src/screens/project/conversation.tsx";
import * as team from "../../src/screens/team/controls.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let root = null;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page"><div class="pj" id="mount"></div></div></main></div>';
const DRAWER = '<dialog class="drawer" open><div class="dialog-body pj" id="mount"></div></dialog>';

/** Mounts Agent settings in the app's real page column, or the master's settings in the app's
 * drawer, styled by the app's own CSS. Inputs: that CSS, "page" or "drawer", and the team
 * status. Output: false when the screen has no such view, else true. */
export async function mount(css, view, status) {
  root?.unmount();
  document.head.querySelector("style[data-app]")?.remove();
  const style = document.createElement("style");
  style.dataset.app = "";
  style.textContent = css;
  document.head.append(style);
  document.body.innerHTML = view === "drawer" ? DRAWER : PAGE;
  root = createRoot(document.getElementById("mount"));
  const data = {
    id: "alpha", name: "Alpha", now: 0, setup: undefined, harnesses: undefined, research: undefined, context: undefined, pools: undefined,
    assigned: { pool: { id: "pool-a", name: "Pool A", members: [] } }, team: { result: { kind: "ok", value: status }, at: 0 },
    navigate() {}, notify() {}, reload() {}, reloadPools() {}, reloadTeam() {},
  };
  const component = view === "drawer" ? conversation.MasterSettings : team.TeamControls;
  if (typeof component !== "function") return false;
  const props = view === "drawer" ? { data } : { projectId: "alpha", status, reload() {}, navigate() {} };
  await act(async () => {
    root.render(createElement(component, props));
    await new Promise((done) => setTimeout(done, 0));
  });
  return true;
}

/** Measures the mounted settings where they are drawn. Output: each group's heading and row
 * labels, the rows and parts that run past their group's panel, how far the scrolling area
 * scrolls sideways and the width the groups take. */
export function measure() {
  const groups = [...document.querySelectorAll(".set-group")];
  const past = groups.flatMap((group) => {
    const edge = group.getBoundingClientRect();
    return [...group.querySelectorAll("*")].filter((part) => {
      const box = part.getBoundingClientRect();
      return box.width > 1 && (box.right > edge.right + 0.5 || box.left < edge.left - 0.5);
    }).map((part) => part.closest(".set-row")?.dataset.setting ?? part.getAttribute("class") ?? part.tagName);
  });
  const scroller = document.querySelector(".main") ?? document.querySelector("dialog");
  return {
    groups: groups.map((group) => ({ title: group.querySelector("h2, h3")?.textContent ?? null, rows: [...group.querySelectorAll(".set-row")].map((row) => row.dataset.setting) })),
    past: [...new Set(past)],
    sideways: scroller.scrollWidth - scroller.clientWidth,
    width: Math.round(document.querySelector(".set-groups")?.getBoundingClientRect().width ?? 0),
  };
}

/** Says which setting row holds the focused element. Output: the row's label, or null. */
export function focused() {
  return document.activeElement?.closest(".set-row")?.dataset.setting ?? null;
}

/** Lets the page draw for a screenshot: the page and its scrolling area grow to their content.
 * Output: nothing. */
export function unclip() {
  for (const selector of ["html", "body", ".shell", ".main"]) {
    const element = document.querySelector(selector);
    if (element) Object.assign(element.style, { height: "auto", overflow: "visible" });
  }
}
