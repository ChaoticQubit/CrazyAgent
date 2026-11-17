import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import intake from "../../src/screens/intake/intake.css?inline";
import { Welcome } from "../../src/screens/first-run/index.tsx";

let root = null;

/**
 * Mounts the real first-run Welcome on one step with the real stylesheets.
 * Inputs: the theme and the step. Output: none.
 */
export function mount(theme = "light", step = "profile") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, intake].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root" style="width: 1100px; height: 800px"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h(Welcome, { onSaved: () => {}, onSkip: () => {}, initialStep: step })));
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
 * The look of one step in the step list. Input: the element. Output: its fill, edge width and
 * color, corner, words color, weight, size and line height.
 */
function step(node) {
  const style = getComputedStyle(node);
  return {
    fill: style.backgroundColor, edge: `${style.borderTopWidth} ${style.borderTopColor}`, corner: style.borderTopLeftRadius,
    color: style.color, weight: style.fontWeight, size: style.fontSize, line: style.lineHeight,
  };
}

/**
 * Where one step's words start on the page. Input: the step element. Output: the top of its
 * text in px.
 */
function wordsTop(node) {
  const range = document.createRange();
  range.selectNodeContents(node);
  return range.getBoundingClientRect().top;
}

/**
 * Reads the step list and the filled buttons of the mounted step. Output: the tokens, the
 * current and one other step's look and where their words start, and each filled button's
 * words, fill and word color.
 */
export function look() {
  const steps = document.querySelector(".in-steps");
  return {
    words: { current: wordsTop(steps.querySelector('[aria-current="step"]')), other: wordsTop(steps.querySelector("li:not([aria-current])")) },
    tokens: Object.fromEntries(["surface", "border", "text", "text-3", "primary", "on-primary"].map((name) => [name, token(name)])),
    current: step(steps.querySelector('[aria-current="step"]')),
    other: step(steps.querySelector("li:not([aria-current])")),
    filled: [...document.querySelectorAll(".welcome .btn.primary")].map((button) => ({
      words: button.textContent.trim(), fill: getComputedStyle(button).backgroundColor, color: getComputedStyle(button).color,
    })),
  };
}
