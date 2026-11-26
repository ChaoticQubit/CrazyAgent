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
