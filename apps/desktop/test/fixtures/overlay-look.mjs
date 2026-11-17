import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { Dialog, Drawer } from "../../src/ui/overlay.tsx";

let root = null;
const none = () => {};

/**
 * Opens one real Dialog or Drawer, with a body and a footer, on the real stylesheet.
 * Inputs: "dialog" or "drawer", and the theme. Output: none.
 */
export function mount(kind, theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = shell;
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const Overlay = kind === "drawer" ? Drawer : Dialog;
  flushSync(() => root.render(h(Overlay, { open: true, title: "Remove project", onClose: none, footer: h("span", null, "Foot") }, h("p", null, "Body"))));
}

/** Waits two frames, so the modal effect has opened the dialog. Output: a promise. */
export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/**
 * The computed value one token gives a property, read from a probe element.
 * Inputs: the token name without its dashes and the CSS property. Output: the computed value.
 */
function token(name, property = "color") {
  const probe = document.createElement("span");
  probe.style.setProperty(property, `var(--${name})`);
  document.body.append(probe);
  const value = getComputedStyle(probe).getPropertyValue(property);
  probe.remove();
  return value;
}

/**
 * Reads the open overlay's look and the tokens it should take. Output: the tokens and the
 * overlay's fill, text, edges, radius, shadow, backdrop and title type.
 */
export function look() {
  const dialog = document.querySelector("dialog");
  const style = getComputedStyle(dialog);
  const title = getComputedStyle(dialog.querySelector(".dialog-head h2"));
  return {
    tokens: {
      surface: token("surface"), text: token("text"), border: token("border"),
      scrim: token("scrim", "background-color"), shadow: token("elev-2", "box-shadow"),
    },
    open: dialog.open,
    background: style.backgroundColor, color: style.color,
    edges: { top: style.borderTopColor, left: style.borderLeftColor, leftWidth: style.borderLeftWidth },
    radius: style.borderTopLeftRadius, shadow: style.boxShadow,
    backdrop: getComputedStyle(dialog, "::backdrop").backgroundColor,
    head: getComputedStyle(dialog.querySelector(".dialog-head")).borderBottomColor,
    foot: getComputedStyle(dialog.querySelector(".dialog-foot")).borderTopColor,
    title: { size: title.fontSize, line: title.lineHeight, weight: title.fontWeight },
  };
}
