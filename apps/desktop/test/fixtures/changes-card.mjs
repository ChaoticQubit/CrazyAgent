import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import changes from "../../src/screens/changes/changes.css?inline";
import { ChangeRow } from "../../src/screens/changes/index.tsx";

let root = null;

const setOf = (id, label, extra) => ({
  id, agent: { kind: "sub", id, label }, state: "pending", base: "a".repeat(40), head: "b".repeat(40), files: 1, added: 4, removed: 0,
  last_outcome: "succeeded", checked: false, created_ms: 1, updated_ms: 1, restorable_until_ms: null, conflicts: [], regenerated: [], message: "", waiting: false, outside: false, ...extra,
});

const SETS = [
  setOf("cs-1", "Bo (@south) · Codex", { state: "updating", conflicts: ["README.md"] }),
  setOf("cs-2", "Ada (@north) · Codex", { state: "accepted" }),
];

/** Mounts the Changes list as the Changes tab draws it, with the real stylesheets, beside a
 * detail column: one set its agent is fixing conflicts with main and one accepted set. */
export function mountCards() {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, changes].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div class="pj" style="width: 1000px; padding: 16px"><div id="root"></div></div>';
  root = createRoot(document.getElementById("root"));
  const list = h("ul", { className: "pj-rows ch-list", "aria-label": "Change sets" },
    SETS.map((set, index) => h("li", { key: set.id }, h(ChangeRow, { set, active: index === 1, onSelect: () => {} }))));
  flushSync(() => root.render(h("div", { className: "ch-layout" }, list, h("div", { className: "ch-detail" }, "Detail"))));
}

export async function settled() {
  await document.fonts.ready;
  await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Measures the list and each row's badge, rounded to whole pixels. Output: the list width,
 * its inner width, the lines drawn between rows, and per row the badge word, its height and
 * right edge, the row's width and content right edge and whether the badge text overflows. */
export function cards() {
  const list = document.querySelector(".ch-list");
  const items = [...list.children];
  const width = (element, side) => parseFloat(getComputedStyle(element)[`border${side}Width`]);
  return {
    list: Math.round(list.getBoundingClientRect().width),
    inner: list.clientWidth,
    between: items.slice(1).map((item, index) => width(item, "Top") + width(items[index].querySelector(".ch-row"), "Bottom")),
    rows: [...list.querySelectorAll(".ch-row")].map((row) => {
      const badge = row.querySelector(".badge");
      const box = badge.getBoundingClientRect();
      const rowBox = row.getBoundingClientRect();
      return {
        word: badge.textContent,
        height: Math.round(box.height),
        right: Math.round(box.right),
        contentRight: Math.round(rowBox.right - parseFloat(getComputedStyle(row).paddingRight)),
        overflows: badge.scrollWidth > badge.clientWidth,
        rowWidth: Math.round(rowBox.width),
      };
    }),
  };
}
