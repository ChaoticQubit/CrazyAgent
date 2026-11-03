const WIDE = { width: 2560, height: 1440 };
const WIDE_MIN = 1400;
const SLACK_PX = 1;

/** Measures the main area, its page and the screen body as the full-width unit test does,
 * rounded to whole pixels. Output: the window width and the right edges, or null before the
 * shell shows. */
function boxes() {
  return browser.execute(() => {
    const main = document.querySelector(".main");
    const page = main?.querySelector(".page");
    const body = page?.firstElementChild;
    if (!main || !page || !body) return null;
    const left = main.getBoundingClientRect().left;
    return {
      windowWidth: window.innerWidth,
      mainRight: Math.round(left + main.clientWidth),
      pageRight: Math.round(page.getBoundingClientRect().right),
      pagePadding: Math.round(parseFloat(getComputedStyle(page).paddingRight)),
      bodyRight: Math.round(body.getBoundingClientRect().right),
    };
  });
}

/** Checks one screen fills the window to its right edge and fails with the measured boxes
 * when a blank strip is left. Input: the screen name. Output: one line. */
async function filled(kit, name) {
  const seen = await kit.waitFor(async () => {
    const box = await boxes();
    return { done: box !== null, seen: JSON.stringify(box) };
  }, kit.UI_MS, `${name} did not show its page`);
  const box = JSON.parse(seen);
  const near = (left, right) => Math.abs(left - right) <= SLACK_PX;
  if (!near(box.mainRight, box.windowWidth)) throw new Error(`${name}: main ends at ${box.mainRight} of ${box.windowWidth} px`);
  if (!near(box.pageRight, box.mainRight)) throw new Error(`${name}: the page ends at ${box.pageRight}, main at ${box.mainRight}`);
  if (!near(box.bodyRight, box.pageRight - box.pagePadding)) throw new Error(`${name}: the screen body ends at ${box.bodyRight}, the page padding at ${box.pageRight - box.pagePadding}`);
  return `${name} fills ${box.windowWidth} px`;
}

export const checks = [{
  id: "N-D12",
  op: ["OP-21"],
  title: "A full-width window: Home and a project page fill the width with no blank right half",
  why: "owner: full screen, everything squished to the left, blank space on the right",
  after: "end",
  run: async (kit) => {
    const before = await browser.getWindowSize();
    try {
      await browser.setWindowSize(WIDE.width, WIDE.height);
      const width = await browser.execute(() => window.innerWidth);
      if (width < WIDE_MIN) throw new Error(`the window opened only ${width} px wide; a full-width check needs at least ${WIDE_MIN} px`);
      await kit.go("#/home");
      const home = await filled(kit, "Home");
      await kit.go(`#/projects/${encodeURIComponent(kit.project())}/overview`);
      const page = await filled(kit, "the project page");
      await kit.screen();
      return `${home}; ${page}`;
    } finally {
      await browser.setWindowSize(before.width, before.height);
    }
  },
}];
