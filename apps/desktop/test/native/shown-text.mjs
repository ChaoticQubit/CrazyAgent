/** The page expression that reads the text of the first element a selector names, as the
 * questions flow reads its pop-up. Input: the CSS selector. Output: the expression, which gives
 * the element's text, or "" when no element matches. */
export function shownText(selector) {
  return `document.querySelector(${JSON.stringify(selector)})?.innerText ?? ""`;
}
