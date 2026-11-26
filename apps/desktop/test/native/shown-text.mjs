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

/** The page expression that finds the first line of a tab panel's terminal screen that shows
 * a text, read from the terminal's own buffer through its host's `readScreen`. Inputs: the
 * panel's CSS selector and the text. Output: the expression, which gives that line, or null
 * when no terminal is shown there or no line has the text. */
export function screenLine(panel, text) {
  return `(() => {
    const host = document.querySelector(${JSON.stringify(panel)})?.querySelector(".tg-terminal-host");
    const lines = host?.checkVisibility() ? host.readScreen?.() ?? [] : [];
    return lines.find((line) => line.includes(${JSON.stringify(text)})) ?? null;
  })()`;
}

/** Types a word at the keyboard as the owner does, one key at a time into the focused field,
 * waits until the panel's terminal screen shows it, then erases it with as many Backspaces,
 * so the keys provably reached the agent's terminal and its input line is left as it was.
 * Inputs: the page ({ send, evaluate }), the panel selector, the word, the flow's `until`
 * and its wait in milliseconds. Output: the screen line that showed the word. */
export async function echoed(page, panel, word, until, wait) {
  if (!/^[a-z0-9]{1,16}$/.test(word)) throw new TypeError(`echoed types 1 to 16 lowercase letters or digits, not ${JSON.stringify(word)}`);
  for (const key of word) {
    await page.send("Input.dispatchKeyEvent", { type: "keyDown", key, text: key });
    await page.send("Input.dispatchKeyEvent", { type: "keyUp", key });
  }
  const line = await until("typed_keys_echoed", wait, () => page.evaluate(screenLine(panel, word)));
  for (let index = 0; index < word.length; index += 1) {
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 });
  }
  return line;
}
