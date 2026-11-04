import { UI_MS } from "../trial-kit.mjs";
import { approvedBrand, dockIcon, iconsVerdict, served } from "../trial4-icons.mjs";

const MAIN = "#main";
const SIDEBAR = "aside.sidebar";
const MARK = "aside.sidebar .brand-mark[role=\"img\"][aria-label=\"CrazyAgent\"] svg.brand-glyph";

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

/** Runs in the page: draws each favicon its link rel=icon names and each approved favicon
 * onto a canvas, from images the window loads as it shows them. It never fetches, which the
 * app CSP's connect-src refuses. Inputs: the approved favicons [{ label, url }] as data URLs
 * and the WebDriver callback. Answers { links: [{ href, image }], approved: [{ label, image }] },
 * an image being { width, height, pixels } or { failed }. */
export function faviconReadings(approved, done) {
  const draw = async (source) => {
    try {
      const image = new Image();
      image.src = source;
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const context = canvas.getContext("2d");
      context.drawImage(image, 0, 0);
      return { width: canvas.width, height: canvas.height, pixels: [...context.getImageData(0, 0, canvas.width, canvas.height).data] };
    } catch (failure) {
      return { failed: String(failure) };
    }
  };
  Promise.all([
    Promise.all([...document.querySelectorAll("link[rel~=\"icon\"]")].map(async (link) => ({ href: link.getAttribute("href") ?? "", image: await draw(link.href) }))),
    Promise.all(approved.map(async ({ label, url }) => ({ label, image: await draw(url) }))),
  ]).then(([links, drawn]) => done({ links, approved: drawn }), (failure) => done({ failed: String(failure) }));
}

/** Reads the window's favicons through the page and judges them against the approved
 * brand. Input: the approved brand. Output: [{ label, href, problem }]; throws with OP-59
 * when the page could not read them at all. */
async function favicons(approved) {
  const answer = await browser.executeAsync(faviconReadings, approved.favicons.map(({ label, url }) => ({ label, url })));
  if (answer?.failed !== undefined) throw new Error(`OP-59: the window could not read its favicons: ${answer.failed}`);
  return served(answer, approved);
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
    return iconsVerdict({ home, project, sidebar, glyphs, favicons: await favicons(approved), dock: dockIcon(process.env.TRIAL3_APP), approved });
  },
}];
