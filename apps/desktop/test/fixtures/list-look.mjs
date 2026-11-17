import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import changes from "../../src/screens/changes/changes.css?inline";
import { ChangeRow } from "../../src/screens/changes/index.tsx";
import { WindowedList } from "../../src/ui/windowed-list.tsx";

let root = null;

const setOf = (id, label) => ({
  id, agent: { kind: "sub", id, label }, state: "pending", base: "a".repeat(40), head: "b".repeat(40), files: 1, added: 4, removed: 0,
  last_outcome: "succeeded", checked: false, created_ms: 1, updated_ms: 1, restorable_until_ms: null, conflicts: [], regenerated: [], message: "", waiting: false, outside: false,
});

/**
 * Mounts the list rows with the real stylesheets: a windowed account table with its column
 * head and two picked rows drawn as the Accounts page draws them, an item row, and the real
 * Changes list of three with its middle row shown, so the shown row is not the last one, which
 * draws no divider. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, changes].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div class="pj" style="width: 1000px; padding: 16px"><div id="root"></div></div>';
  root = createRoot(document.getElementById("root"));
  const head = h("div", { className: "list-head" }, h("span", null, "Name"), h("span", null, "State"));
  const row = (index) => h("div", { className: "account-line" },
    h("input", { type: "checkbox", className: "account-pick", checked: true, readOnly: true, "aria-label": `Pick ${index}` }),
    h("button", { type: "button", className: "account-row" }, h("span", { className: "cell" }, `Account ${index}`)));
  const sets = [setOf("cs-1", "Bo (@south) · Codex"), setOf("cs-2", "Ada (@north) · Codex"), setOf("cs-3", "Cy (@east) · Codex")];
  flushSync(() => root.render(h("div", { style: { display: "grid", gap: "16px" } },
    h("div", { className: "account-table" }, h(WindowedList, { label: "Accounts", head, count: 2, viewport: 200, renderRow: row })),
    h("div", { className: "item-row" }, h("span", { className: "title" }, "Item"), h("span", null, "Meta")),
    h("ul", { className: "pj-rows ch-list", "aria-label": "Change sets" },
      sets.map((set, index) => h("li", { key: set.id }, h(ChangeRow, { set, active: index === 1, onSelect: () => {} })))),
  )));
}

/**
 * The color one token paints in the mounted theme, as the browser computes it.
 * Input: the token name without its dashes. Output: an rgb() string.
 */
function token(name) {
  const probe = document.createElement("span");
  probe.style.color = `var(--${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}

/**
 * The look of one change row. Input: the row button. Output: its fill, divider, left edge
 * and title weight.
 */
function changeRow(node) {
  const style = getComputedStyle(node);
  return { background: style.backgroundColor, divider: style.borderBottomColor, edge: style.boxShadow, title: getComputedStyle(node.querySelector("strong")).fontWeight };
}

/**
 * Reads the list rows' look and the tokens they should take. Output: the tokens, the column
 * head, an account row, its pick box, the item row title and both change rows.
 */
export function look() {
  const css = (selector) => getComputedStyle(document.querySelector(selector));
  const rows = document.querySelectorAll(".ch-row");
  return {
    tokens: Object.fromEntries(["text-3", "border", "primary", "surface-3"].map((name) => [name, token(name)])),
    head: { size: css(".list-head").fontSize, color: css(".list-head").color },
    account: { background: css(".account-row").backgroundColor, divider: css(".account-line").borderBottomColor },
    pick: css(".account-pick").accentColor,
    item: css(".item-row .title").fontWeight,
    change: { other: changeRow(rows[0]), shown: changeRow(rows[1]) },
  };
}
