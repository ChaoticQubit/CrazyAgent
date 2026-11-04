/** Right-clicks one element of the window as WebKit would deliver it and tells whether the app
 * cancelled the web view's own menu. WebDriver cannot see the native menu, so a cancelled
 * contextmenu event is the proof. Input: the CSS selector. Output: { found, cancelled, tag }. */
function rightClick(selector) {
  return browser.execute((query) => {
    const target = document.querySelector(query);
    if (!target) return { found: false, cancelled: false, tag: "" };
    const box = target.getBoundingClientRect();
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: box.left + 4, clientY: box.top + 4 });
    target.dispatchEvent(event);
    return { found: true, cancelled: event.defaultPrevented, tag: target.tagName.toLowerCase() };
  }, selector);
}

/** Tells whether the window shows an element. Input: the CSS selector. Output: true or false. */
function shown(selector) {
  return browser.execute((query) => document.querySelector(query) !== null, selector);
}

/** Right-clicks a place and checks the outcome, failing with what was seen. Inputs: the place
 * name, the selector and whether the web view menu must be cancelled there. Output: one line. */
async function expectMenu(place, selector, cancelled) {
  const seen = await rightClick(selector);
  if (!seen.found) throw new Error(`${place}: nothing at ${selector} to right-click`);
  if (seen.cancelled !== cancelled) {
    throw new Error(`${place}: the web view menu was ${seen.cancelled ? "cancelled" : "left open"} on <${seen.tag}>, expected ${cancelled ? "cancelled" : "left open"}`);
  }
  return `${place} ${cancelled ? "no web view menu" : "edit menu kept"}`;
}

const MASTER_BOX = 'textarea[aria-label="Message the master"]';
const MASTER_BOX_MS = 10000;

export const checks = [{
  id: "N-D3",
  op: ["OP-48"],
  title: "Right-click shows no Reload, Services or Inspect menu outside text; text boxes keep their edit menu",
  why: "owner: right-click shows the WebKit context menu (Reload, Services, developer items)",
  after: "end",
  run: async (kit) => {
    const lines = [];
    await kit.go("#/home");
    await kit.until(() => shown("main .page"), kit.UI_MS, "Home did not show its page");
    lines.push(await expectMenu("Home", "main .page", true));
    await kit.go(`#/projects/${encodeURIComponent(kit.project())}/overview`);
    await kit.until(() => shown("main .page h1"), kit.UI_MS, "the project page did not open");
    lines.push(await expectMenu("project page", "main .page", true));
    await kit.go(`#/projects/${encodeURIComponent(kit.project())}/conversation`);
    let place = "master message box";
    let box = MASTER_BOX;
    try {
      await kit.until(() => shown(box), MASTER_BOX_MS, "the master message box did not show");
    } catch {
      place = "Profile text box (the master input is the live terminal)";
      box = "main textarea";
      await kit.go("#/settings/profile");
      await kit.until(() => shown(box), kit.UI_MS, "Profile showed no text box");
    }
    lines.push(await expectMenu(place, box, false));
    await kit.screen();
    return lines.join("; ");
  },
}];
