import { UI_MS } from "../trial-kit.mjs";
import { approvedBrand, dockIcon, iconsVerdict, served } from "../trial4-icons.mjs";

const MAIN = "#main";
const SIDEBAR = "aside.sidebar";
const MARK = "aside.sidebar .brand-mark[role=\"img\"][aria-label=\"CrazyAgent\"] svg.brand-glyph";
const FAVICON_BYTES_MAX = 65536;

/** Runs in the page: counts the line icons a region draws, an icon being an svg.icon with
 * shapes that the window shows at a size above zero. Input: the region's selector. Output:
 * the count. */
function drawnIcons(selector) {
  const shown = (svg) => svg.childElementCount > 0 && svg.getBoundingClientRect().width > 0 && svg.getBoundingClientRect().height > 0
    && svg.checkVisibility({ opacityProperty: true, visibilityProperty: true, checkOpacity: true, checkVisibilityCSS: true });
  return [...document.querySelectorAll(`${selector} svg.icon`)].filter(shown).length;
}

/** Runs in the page: reads each drawing of the sidebar's CrazyAgent mark the window shows,
 * its coil path and each square as "x y width height transform". Input: the drawings'
 * selector. Output: [{ coil, squares }]. */
function shownMarks(selector) {
  const shown = (svg) => svg.getBoundingClientRect().width > 0 && svg.checkVisibility({ opacityProperty: true, visibilityProperty: true, checkOpacity: true, checkVisibilityCSS: true });
  return [...document.querySelectorAll(selector)].filter(shown).map((svg) => ({
    coil: svg.querySelector("path")?.getAttribute("d") ?? "",
    squares: [...svg.querySelectorAll("rect")].map((rect) => ["x", "y", "width", "height", "transform"].map((name) => rect.getAttribute(name)).join(" ")),
  }));
}

/** Runs in the page: fetches each favicon its link rel=icon names, as the window loads it.
 * Inputs: the most bytes to return and the WebDriver callback. Answers [{ href, status, size,
 * bytes }], bytes empty when the file is bigger, or { failed } when a fetch threw. */
function faviconReadings(max, done) {
  Promise.all([...document.querySelectorAll("link[rel~=\"icon\"]")].map(async (link) => {
    const reply = await fetch(link.href);
    const bytes = new Uint8Array(await reply.arrayBuffer());
    return { href: link.getAttribute("href"), status: reply.status, size: bytes.length, bytes: bytes.length <= max ? [...bytes] : [] };
  })).then(done, (failure) => done({ failed: String(failure) }));
}

/** Fetches the window's favicons through the page and hashes them. Output: [{ href, sha }];
 * throws with OP-59 when the window could not fetch one. */
async function favicons() {
  const answer = await browser.executeAsync(faviconReadings, FAVICON_BYTES_MAX);
  if (!Array.isArray(answer)) throw new Error(`OP-59: the window could not fetch its favicons: ${answer?.failed ?? JSON.stringify(answer).slice(0, 300)}`);
  return served(answer);
}

/** Waits until a region draws an icon. Inputs: the kit and the region's selector. Output: the
 * icons it draws, 0 when none appeared within the UI wait. */
async function settled(kit, selector) {
  try {
    await kit.until(async () => await browser.execute(drawnIcons, selector) > 0, UI_MS, `${selector} drew no icon`);
  } catch {
    return 0;
  }
  return browser.execute(drawnIcons, selector);
}

export const checks = [{
  id: "N-D33",
  op: ["OP-59"],
  title: "Home, a project page and the Dock show the approved icons",
  why: "owner: no icons or favicons in the UI",
  after: "end",
  run: async (kit) => {
    const approved = approvedBrand();
    await kit.go("#/home");
    const home = await settled(kit, MAIN);
    await kit.screen();
    await kit.tab("context");
    const project = await settled(kit, MAIN);
    const sidebar = await browser.execute(drawnIcons, SIDEBAR);
    const glyphs = await browser.execute(shownMarks, MARK);
    await kit.screen();
    return iconsVerdict({ home, project, sidebar, glyphs, favicons: await favicons(), dock: dockIcon(process.env.TRIAL3_APP), approved });
  },
}];
