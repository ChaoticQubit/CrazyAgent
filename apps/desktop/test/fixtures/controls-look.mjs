import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { Select, TextField } from "../../src/ui/field.tsx";
import { Checkbox, RadioCard, Switch } from "../../src/ui/toggle.tsx";

let root = null;
const none = () => {};

/**
 * Mounts the real form controls with the real stylesheet: a text field with help and an
 * error, a select, a switch off and on, a checked checkbox and a checked and an unchecked
 * radio card. Input: the theme. Output: none.
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
  flushSync(() => root.render(h("div", { style: { display: "grid", gap: "12px", padding: "16px", width: "360px" } },
    h(TextField, { id: "name", label: "Name", value: "", onValue: none, helper: "Shown on the card." }),
    h(TextField, { id: "url", label: "Address", value: "x", onValue: none, error: "Not an address." }),
    h(Select, { id: "pick", label: "Pick", value: "a", options: [{ value: "a", label: "A" }, { value: "b", label: "B" }], onValue: none }),
    h(Switch, { id: "off", label: "Off", checked: false, onChecked: none }),
    h(Switch, { id: "on", label: "On", checked: true, onChecked: none }),
    h(Checkbox, { label: "Agree", checked: true, onChecked: none }),
    h(RadioCard, { name: "level", value: "one", checked: true, onChecked: none, title: "One", details: ["The first."] }),
    h(RadioCard, { name: "level", value: "two", checked: false, onChecked: none, title: "Two", details: ["The second."] }),
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
 * Reads the controls' look and the tokens they should take. Output: the tokens and, per
 * control, the computed colors, sizes and weights the look rows compare.
 */
export function look() {
  const css = (selector, pseudo) => getComputedStyle(document.querySelector(selector), pseudo ?? null);
  const switchOf = (id) => ({ track: css(`#${id}`).backgroundColor, border: css(`#${id}`).borderTopColor, knob: css(`#${id}`, "::after").backgroundColor });
  const cards = document.querySelectorAll(".radio-card");
  const card = (node) => {
    const style = getComputedStyle(node);
    return { border: style.borderTopColor, background: style.backgroundColor, title: getComputedStyle(node.querySelector(".title")).fontWeight };
  };
  return {
    tokens: Object.fromEntries(["border", "border-input", "surface", "surface-2", "surface-3", "text-3", "danger", "primary", "on-primary"].map((name) => [name, token(name)])),
    label: css('label[for="name"]').fontWeight,
    input: css("#name").borderTopColor,
    select: css("#pick").borderTopColor,
    helper: { color: css(".helper").color, size: css(".helper").fontSize, line: css(".helper").lineHeight },
    error: { color: css(".field-error").color, size: css(".field-error").fontSize },
    off: switchOf("off"),
    on: switchOf("on"),
    checkbox: css('.checkbox input').accentColor,
    checked: card(cards[0]),
    unchecked: card(cards[1]),
    radio: getComputedStyle(cards[0].querySelector("input")).accentColor,
    detail: getComputedStyle(cards[0].querySelector(".detail")).fontSize,
  };
}
