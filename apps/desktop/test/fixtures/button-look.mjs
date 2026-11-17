import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { Button } from "../../src/ui/button.tsx";

let root = null;

/** The variants the look rows read, each with the props that make it. */
const VARIANTS = {
  secondary: {},
  primary: { variant: "primary" },
  quiet: { variant: "quiet" },
  danger: { variant: "danger" },
  small: { size: "small" },
};

/**
 * Mounts one real Button per variant with the real stylesheet. Input: the theme.
 * Output: none.
 */
export function mount(theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = shell;
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const buttons = Object.entries(VARIANTS).map(([name, props]) => h(Button, { key: name, "data-variant": name, ...props }, name));
  flushSync(() => root.render(h("div", { style: { display: "flex", gap: "8px", padding: "16px" } }, buttons)));
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
 * Reads every mounted button's look and the tokens it should take. Output: the tokens by
 * name and, per variant, its fill, top border color, text color, weight and size.
 */
export function look() {
  const read = (name) => {
    const style = getComputedStyle(document.querySelector(`[data-variant="${name}"]`));
    return { background: style.backgroundColor, border: style.borderTopColor, color: style.color, weight: style.fontWeight, size: style.fontSize };
  };
  const tokens = Object.fromEntries(["surface", "border", "text", "text-2", "primary", "on-primary", "danger-solid", "on-danger"].map((name) => [name, token(name)]));
  return { tokens, ...Object.fromEntries(Object.keys(VARIANTS).map((name) => [name, read(name)])) };
}
