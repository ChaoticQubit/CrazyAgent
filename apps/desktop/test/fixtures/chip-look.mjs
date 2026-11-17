import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import team from "../../src/screens/team/team-graph.css?inline";
import { StatusBadge } from "../../src/ui/feedback.tsx";

let root = null;

export const WORDS = { ok: "Signed in", warn: "Needs you", danger: "Failed", info: "Running", neutral: "Queued" };
export const CONSOLE = { ok: "terminal-green", warn: "terminal-yellow", danger: "terminal-red", muted: "terminal-muted" };

/**
 * Mounts one real status badge per tone plus a steady info badge, the master console head's
 * state words as live-master.tsx draws them, and one team graph dot per tone, with the real
 * stylesheets.
 * Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, team].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h("div", { style: { display: "grid", gap: "12px", padding: "16px", width: "480px" } },
    h("div", { id: "badges" }, Object.entries(WORDS).map(([tone, word]) => h("span", { key: tone, "data-tone": tone }, h(StatusBadge, { word })))),
    h("div", { id: "steady" }, h(StatusBadge, { word: "Ready to review", tone: "info" })),
    h("div", { className: "pj-console" }, h("div", { className: "pj-console-head" },
      Object.keys(CONSOLE).map((tone) => h("span", { key: tone, className: `pj-console-state ${tone}` }, tone)))),
    h("div", { id: "dots" }, ["info", "ok", "warn", "danger"].map((tone) => h("span", { key: tone, className: `tg-dot tone-${tone}` }))),
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
 * The look of one status word with its dot. Input: the element. Output: its fill, word color,
 * size, weight, left padding and its dot's color and size.
 */
function chip(node) {
  const style = getComputedStyle(node);
  const dot = getComputedStyle(node, "::before");
  return {
    background: style.backgroundColor, color: style.color, size: style.fontSize, weight: style.fontWeight, padding: style.paddingLeft,
    dot: { color: dot.backgroundColor, width: dot.width, height: dot.height },
  };
}

/**
 * Reads every chip's look and the tokens each should take. Output: the tokens, the badge per
 * tone, the dot animation of the working and the steady info badge, the console state per tone
 * and the graph dot color per tone.
 */
export function look() {
  const names = ["ok", "warn", "danger", "running", "text-2", ...Object.values(CONSOLE)];
  return {
    tokens: Object.fromEntries(names.map((name) => [name, token(name)])),
    badges: Object.fromEntries([...document.querySelectorAll("#badges > span")].map((node) => [node.dataset.tone, chip(node.querySelector(".badge"))])),
    motion: {
      working: getComputedStyle(document.querySelector('#badges > [data-tone="info"] .badge'), "::before").animationName,
      steady: getComputedStyle(document.querySelector("#steady .badge"), "::before").animationName,
    },
    console: Object.fromEntries([...document.querySelectorAll(".pj-console-state")].map((node) => [node.textContent, chip(node)])),
    dots: Object.fromEntries([...document.querySelectorAll("#dots > .tg-dot")].map((node) => [node.className.replace("tg-dot tone-", ""), getComputedStyle(node).backgroundColor])),
  };
}
