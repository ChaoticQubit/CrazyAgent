const VISIT_MS_MAX = 10000;
const HOME_WATCH_MS = 5000;
const PAGE_WATCH_MS = 300;
const SLOW_MS = 3000;

/** Opens a route from inside the window and watches every frame until the page shows content
 * and then for a while longer: when it showed (no skeleton left in the page), and whether the
 * Get set up side panel appeared and then went away. Inputs: the hash and how long to watch
 * once shown. Output: { ready, panel, flashed, last } with ready in ms or null. */
function visit(route, watch) {
  return browser.executeAsync((next, after, limit, done) => {
    const start = performance.now();
    let ready = null;
    let panel = false;
    let flashed = false;
    const frame = () => {
      const now = performance.now() - start;
      const page = document.querySelector("main .page");
      const shown = Boolean(document.querySelector(".setup-panel"));
      if (shown) panel = true;
      if (panel && !shown) flashed = true;
      const loading = !page || page.childElementCount === 0 || page.querySelector(".skeleton") !== null;
      if (ready === null && location.hash === next && !loading) ready = Math.round(now);
      if (now >= limit || (ready !== null && now - ready >= after)) return done({ ready, panel, flashed, last: shown });
      requestAnimationFrame(frame);
    };
    location.hash = next;
    requestAnimationFrame(frame);
  }, route, watch, VISIT_MS_MAX);
}

/** Visits each route from another page and fails on a Home side-panel flash or a page that
 * took longer than SLOW_MS to show. Inputs: the kit, a route to leave from and the routes.
 * Output: one line with every page's time in ms. */
async function measure(kit, away, routes) {
  const times = [];
  for (const [name, route] of routes) {
    await kit.go(away);
    const home = route === "#/home";
    const seen = await visit(route, home ? HOME_WATCH_MS : PAGE_WATCH_MS);
    if (seen.ready === null) throw new Error(`${name} showed no content within ${VISIT_MS_MAX} ms`);
    if (home && seen.flashed) throw new Error(`Home showed the Get set up side panel and then dropped it (${name}, shown after ${seen.ready} ms)`);
    if (seen.ready > SLOW_MS) throw new Error(`${name} took ${seen.ready} ms to show, over ${SLOW_MS} ms`);
    times.push(`${name} ${seen.ready} ms${home ? `, side panel ${seen.panel ? "kept" : "not shown"}` : ""}`);
  }
  return times.join("; ");
}

export const checks = [{
  id: "N-D34",
  op: ["OP-35"],
  title: "Home right after the app opens never flashes the side panel, and the first pages show quickly",
  why: "owner: every time I go to Home it first shows the side status bar for a few seconds and then it goes away",
  after: "open-app",
  run: async (kit) => {
    const line = await measure(kit, "#/settings/profile", [
      ["Home", "#/home"], ["Accounts", "#/settings/accounts"], ["Pools", "#/settings/pools"],
      ["Harnesses", "#/settings/harnesses"], ["Home again", "#/home"],
    ]);
    await kit.screen();
    return line;
  },
}, {
  id: "N-D35",
  op: ["OP-35"],
  title: "Home and the project pages show quickly when opened again, with no side panel flash",
  why: "owner: other tabs and pages are also a little slow to load",
  after: "end",
  run: async (kit) => {
    const id = encodeURIComponent(kit.project());
    const line = await measure(kit, "#/settings/profile", [
      ["Home", "#/home"], ["project overview", `#/projects/${id}/overview`], ["conversation", `#/projects/${id}/conversation`],
      ["agents", `#/projects/${id}/agents`], ["project settings", `#/projects/${id}/settings`], ["Home again", "#/home"],
    ]);
    await kit.screen();
    return line;
  },
}];
