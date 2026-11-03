import { panelScroll, scrollPoints, scrollState, scroller, shifted, windowOff } from "../scroll-points.mjs";

const WHEEL_PX = 240;
const WHEEL_MS = 80;
const SETTLE_MS = 300;
const PAGE_MS = 30000;
const NO_BOUNCE = "none";

/** The pages the check turns the wheel on: Home, the trial project and Settings. Input: the
 * kit. Output: [name, route] pairs. */
function pages(kit) {
  return [["Home", "#/home"], ["the trial project", `#/projects/${encodeURIComponent(kit.project())}/overview`], ["Settings", "#/settings/process"]];
}

/** Turns the wheel down once over a point through the app's WebDriver, which hands the page
 * a wheel event there and scrolls the window by the same amount, then lets it settle.
 * Input: the point. */
async function turn(spot) {
  await browser.action("wheel").scroll({ x: spot.x, y: spot.y, deltaX: 0, deltaY: WHEEL_PX, duration: WHEEL_MS }).perform();
  await browser.pause(SETTLE_MS);
}

/** Checks one point of a page: the window does not bounce, the wheel leaves it in place and
 * moves nothing but the box under the pointer, and that box, when it has room, scrolls with
 * the window and every other box still. Inputs: the page's name and the point. Output:
 * { line, panel }, panel true when a box scrolled; throws naming what moved. */
async function point(page, spot) {
  const where = `${page}, ${spot.name}${spot.over.startsWith(".") ? "" : ` (over ${spot.over})`}`;
  const before = await scrollState(spot.x, spot.y);
  if (!before) throw new Error(`${where}: nothing is drawn under the pointer`);
  if (before.overscroll.some((style) => style !== NO_BOUNCE)) throw new Error(`${where}: the window still bounces at its edges (html and body overscroll ${before.overscroll.join(", ")})`);
  const early = windowOff(before);
  if (early.length) throw new Error(`${where}: before the wheel, ${early.join(", ")} was already scrolled`);
  const keep = scroller(before.chain);
  await turn(spot);
  const after = await scrollState(spot.x, spot.y);
  if (!after) throw new Error(`${where}: nothing is drawn under the pointer after the wheel`);
  const moved = [...new Set([...windowOff(after), ...shifted(before, after, keep)])];
  if (moved.length) throw new Error(`${where}: the wheel moved ${moved.join(", ")}`);
  if (keep < 0) return { line: `${where}: nothing under the pointer scrolls and the window stayed`, panel: false };
  const box = before.chain[keep].name;
  const scroll = await panelScroll(spot.x, spot.y, keep, WHEEL_PX);
  if (!scroll) throw new Error(`${where}: ${box} left the pointer before it could scroll`);
  const was = scroll.before.chain[keep].top;
  const now = scroll.after.chain[keep]?.top ?? was;
  if (now === was) throw new Error(`${where}: ${box} has ${scroll.before.chain[keep].room} px of room and did not scroll`);
  const others = [...new Set([...windowOff(scroll.after), ...shifted(scroll.before, scroll.after, keep)])];
  if (others.length) throw new Error(`${where}: scrolling ${box} also moved ${others.join(", ")}`);
  return { line: `${where}: ${box} scrolled ${Math.abs(now - was)} px alone and the window stayed`, panel: true };
}

export const checks = [{
  id: "N-D31",
  op: ["OP-53"],
  title: "The wheel over the window chrome and every page never moves the window, only the panel under the pointer",
  why: "owner: \"why am I able to scroll on the whole application? The whole application gets scrolled up and down\"",
  after: "end",
  run: async (kit) => {
    const lines = [];
    let panels = 0;
    for (const [page, route] of pages(kit)) {
      await kit.go(route);
      await kit.until(async () => (await scrollPoints()).some((spot) => spot.name === "the page panel"), PAGE_MS, `${page} drew no page panel`);
      for (const spot of await scrollPoints()) {
        const result = await point(page, spot);
        lines.push(result.line);
        if (result.panel) panels += 1;
      }
      await kit.screen();
    }
    if (panels === 0) throw new Error(`no panel on Home, the trial project or Settings had content to scroll, so only the fixed window is shown: ${lines.join("; ")}`);
    return `${WHEEL_PX} px wheel turns through the app's WebDriver at ${lines.length} points, html and body overscroll none: ${lines.join("; ")}`;
  },
}];
