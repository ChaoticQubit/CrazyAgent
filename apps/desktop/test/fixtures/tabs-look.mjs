import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import overview from "../../src/screens/project/overview-tabs.css?inline";
import { Tabs } from "../../src/ui/layout.tsx";
import { Segmented } from "../../src/ui/toggle.tsx";

let root = null;

/**
 * Mounts the real link tabs, a button tab strip drawn as the Overview strip draws it, and a
 * real three-choice segmented control, with the real stylesheets. The second choice of each is
 * selected. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, overview].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const tabs = [{ id: "one", label: "Overview", href: "#one" }, { id: "two", label: "Changes", href: "#two" }];
  const strip = ["Summary", "Activity"].map((label, index) => h("button", {
    key: label, type: "button", id: `strip-${index}`, role: "tab", className: "tab", "aria-selected": index === 1, tabIndex: index === 1 ? 0 : -1,
  }, label));
  const options = [{ value: "one", label: "One" }, { value: "two", label: "Two" }, { value: "three", label: "Three" }];
  flushSync(() => root.render(h("div", { style: { padding: "16px", width: "480px" } },
    h(Tabs, { label: "Project", tabs, current: "two" }),
    h("div", { className: "tabs ov-tablist", role: "tablist", "aria-label": "Overview" }, strip),
    h(Segmented, { label: "Mode", value: "two", options, onValue: () => {} }),
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
 * The look of one tab or segment. Input: the element. Output: its text color, weight, fill
 * and underline color.
 */
function part(node) {
  const style = getComputedStyle(node);
  return { color: style.color, weight: style.fontWeight, background: style.backgroundColor, underline: style.borderBottomColor };
}

/**
 * Reads the tabs' and the segmented control's look and the tokens they should take.
 * Output: the tokens, then per kind the selected and the unselected part.
 */
export function look() {
  const links = document.querySelectorAll("a.tab");
  const buttons = document.querySelectorAll("button.tab");
  const segments = document.querySelectorAll(".segmented button");
  const group = getComputedStyle(document.querySelector(".segmented"));
  return {
    tokens: Object.fromEntries(["text", "text-2", "primary", "surface", "surface-3", "border"].map((name) => [name, token(name)])),
    link: { selected: part(links[1]), other: part(links[0]) },
    strip: { selected: part(buttons[1]), other: part(buttons[0]) },
    segment: { selected: part(segments[1]), other: part(segments[0]) },
    group: { border: group.borderTopColor, background: group.backgroundColor },
  };
}
