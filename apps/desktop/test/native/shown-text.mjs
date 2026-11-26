/** The page expression that reads the text of the first element a selector names only while
 * the owner can see it, as the questions flow reads its pop-up. Input: the CSS selector.
 * Output: the expression, which gives the element's text, or "" when no element matches or
 * the element is hidden, inside a hidden parent or has no size. */
export function shownText(selector) {
  return `(() => {
    const element = document.querySelector(${JSON.stringify(selector)});
    if (!element || !element.checkVisibility({ visibilityProperty: true, opacityProperty: true })) return "";
    const box = element.getBoundingClientRect();
    return box.width > 0 && box.height > 0 ? element.innerText : "";
  })()`;
}

/** The page expression that tells whether a tab panel's terminal has settled: no line in it
 * still says the terminal is connecting, so a field the questions flow picks there stays
 * where it is. Input: the panel's CSS selector. Output: the expression, which gives true once
 * the terminal is live, ended, not ready or absent. */
export function terminalSettled(panel) {
  return `!document.querySelector(${JSON.stringify(panel)})?.querySelector(".tg-terminal-state[role=status] svg.spin")`;
}

/** The page expression that puts the keyboard in the first field a selector names that the
 * owner can see, as the questions flow does before a question pops up. Input: the CSS
 * selector. Output: the expression, which gives the field's class or tag once it holds the
 * keyboard, else null. */
export function shownField(selector) {
  return `(() => {
    const field = [...document.querySelectorAll(${JSON.stringify(selector)})].find((item) => item.checkVisibility() && item.getClientRects().length > 0);
    field?.focus();
    return field && document.activeElement === field ? field.className || field.tagName : null;
  })()`;
}
