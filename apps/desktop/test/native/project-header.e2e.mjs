import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const NAME = "Larkspur Booking";
const ABOUT = `${"A booking web app for small studios: clients pick a class from a live calendar, pay a deposit at checkout and get reminders by email. ".repeat(4)}\nV2 adds recurring classes, package passes and an accessible checkout.`;

/** Reads where the project head sits in the window: the title, the tab bar, the description
 * and its toggle, the head's class and the page's scroll. Input: the page. Output: the
 * measured boxes and values. */
function measure(page) {
  return page.evaluate(`(() => {
    const box = (element) => element ? (({ top, bottom, left, right }) => ({ top, bottom, left, right }))(element.getBoundingClientRect()) : null;
    const desc = document.querySelector("#pj-desc");
    const more = document.querySelector(".pj-desc-more");
    return {
      height: window.innerHeight,
      scrolled: document.querySelector(".main")?.scrollTop ?? 0,
      head: document.querySelector(".pj-head")?.className ?? null,
      headHeight: document.querySelector(".pj-head")?.offsetHeight ?? null,
      settling: document.querySelector(".pj-head")?.getAnimations({ subtree: true }).filter((animation) => animation instanceof CSSTransition).length ?? 0,
      room: (() => { const project = document.querySelector(".pj-project"); return project ? parseFloat(getComputedStyle(project).getPropertyValue("--pj-head-room")) || 0 : 0; })(),
      range: (() => { const main = document.querySelector(".main"); return main ? main.scrollHeight - main.clientHeight : 0; })(),
      title: box(document.querySelector(".pj-head h1")),
      tabs: box(document.querySelector(".pj-head [role=tablist]")),
      desc: desc ? { text: desc.textContent, scrollWidth: desc.scrollWidth, clientWidth: desc.clientWidth, clientHeight: desc.clientHeight, line: parseFloat(getComputedStyle(desc).lineHeight) } : null,
      more: more ? { hidden: more.hidden, text: more.textContent, expanded: more.getAttribute("aria-expanded") } : null,
    };
  })()`);
}

/** Puts the page at the top of a window of the given height, with motion reduced or not,
 * waits for the head's full form to settle, scrolls to a point and samples the head for
 * 1.5 s. Inputs: the page, the window height, whether motion is reduced and the scrollTop.
 * Output: the head's full height, the scroll range at rest and [class, scrollTop, room,
 * height] samples. */
async function scrollAndSample(page, { height, reduced, top }) {
  await page.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: reduced ? "reduce" : "no-preference" }] });
  await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height, deviceScaleFactor: 1, mobile: false });
  await page.evaluate("document.activeElement?.blur(); document.querySelector('.main').scrollTop = 0");
  let rest = await measure(page);
  for (const until = Date.now() + STEP_MS; (rest.head !== "pj-head" || rest.settling > 0) && Date.now() < until; rest = await measure(page)) await pause(POLL_MS);
  assert.equal(rest.head, "pj-head", "the head is back in its full form before the scroll");
  await page.evaluate(`document.querySelector('.main').scrollTop = ${top}`);
  const samples = [];
  for (const until = Date.now() + 1500; Date.now() < until; await pause(POLL_MS)) {
    const now = await measure(page);
    samples.push([now.head, now.scrolled, now.room, now.headHeight]);
  }
  return { full: rest.headHeight, range: rest.range - rest.room, samples };
}

/** Whether the head held its scrolled form and the page its scroll through the later half of
 * the samples. Input: the samples. Output: true when it held. */
function held(samples) {
  const late = samples.slice(Math.floor(samples.length / 2));
  return late.length > 0 && late.every(([head, top]) => head === "pj-head scrolled" && top > 2);
}

/** Whether the room under the page equals what the scrolled head gives up through the later
 * half of the samples. Inputs: the head's full height and the samples. Output: true when so. */
function trimmed(full, samples) {
  const late = samples.slice(Math.floor(samples.length / 2));
  return late.length > 0 && late.every(([, , room, height]) => Math.abs(room - Math.ceil(full - height)) <= 1);
}

test("the project title and tab bar stay on screen while the page scrolls, the description wraps to two lines, one once scrolled, a focused control lands below the head, and a small scroll or a page just taller than the window keeps the head shrunk with or without motion", { timeout: 300000 }, async () => {
  assert.ok(ABOUT.length >= 600, "the description is a long one");
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-ph-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const provider = await closedPort();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
  try {
    ({ child: engine } = await launch(engineBinary,
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
      { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const receipt = await value(bridge, "desktop_setup_project", {
      spec: { name: NAME, repositories: [] },
      setup: { about: ABOUT, tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
      generation: 3,
    });
    assert.equal(receipt.outcome, "applied", `the project is made: ${JSON.stringify(receipt)}`);
    const project = receipt.project_id;

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 420, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings` });
        await me.waitFor("the project's Settings tab", (text) => text.includes(NAME) && text.includes("Settings"), STEP_MS);
        assert.equal(await page.evaluate("document.visibilityState"), "visible", "the window is drawn: a locked screen hides it (ENV-LOCKED)");

        const before = await measure(page);
        assert.equal(before.head, "pj-head", "the head starts in its full form");
        assert.ok(before.desc, "the description is shown");
        assert.ok(before.desc.text.includes("V2 adds recurring classes"), "the description keeps every line");
        assert.ok(before.desc.scrollWidth <= before.desc.clientWidth, `the description wraps inside its box: ${JSON.stringify(before.desc)}`);
        assert.ok(before.desc.clientHeight <= before.desc.line * 2 + 1, `the description takes at most two lines: ${JSON.stringify(before.desc)}`);
        assert.deepEqual([before.more?.hidden, before.more?.text, before.more?.expanded], [false, "more", "false"], "a cut description offers more");
        await me.capture("head-top");

        await page.evaluate("document.querySelector('.main').scrollBy(0, 2000)");
        const deadline = Date.now() + STEP_MS;
        let after = await measure(page);
        while (after.head !== "pj-head scrolled" && Date.now() < deadline) {
          await pause(POLL_MS);
          after = await measure(page);
        }
        assert.ok(after.scrolled > 8, `the page scrolled: ${after.scrolled}`);
        assert.equal(after.head, "pj-head scrolled", "the head takes its scrolled form");
        for (const [what, box] of [["title", after.title], ["tab bar", after.tabs]]) {
          assert.ok(box && box.top >= 0 && box.bottom <= after.height, `the ${what} stays on screen: ${JSON.stringify(box)}`);
        }
        assert.ok(after.desc.scrollWidth <= after.desc.clientWidth, `the description still wraps: ${JSON.stringify(after.desc)}`);
        assert.ok(after.desc.clientHeight <= after.desc.line + 1, `the description shrinks to one line: ${JSON.stringify(after.desc)}`);
        await me.capture("head-scrolled");

        const hidden = await page.evaluate(`(() => {
          const head = document.querySelector(".pj-head").getBoundingClientRect();
          const under = [...document.querySelectorAll(".pj-project input, .pj-project select, .pj-project textarea, .pj-project button")]
            .filter((element) => !element.closest(".pj-head") && !element.disabled && element.getClientRects().length > 0)
            .filter((element) => element.getBoundingClientRect().top < head.bottom);
          const target = under.at(-1);
          if (!target) return null;
          target.setAttribute("data-e2e-focus", "1");
          return target.outerHTML.slice(0, 120);
        })()`);
        assert.ok(hidden, "a control sits under or above the scrolled head");
        await page.evaluate("document.querySelector('[data-e2e-focus]').focus()");
        let clear = null;
        for (const until = Date.now() + STEP_MS; Date.now() < until; await pause(POLL_MS)) {
          clear = await page.evaluate(`(() => {
            const head = document.querySelector(".pj-head").getBoundingClientRect();
            const focused = document.activeElement.getBoundingClientRect();
            return { focused: document.activeElement.hasAttribute("data-e2e-focus"), top: focused.top, head: head.bottom };
          })()`);
          if (clear.focused && clear.top >= clear.head - 1) break;
        }
        assert.ok(clear.focused && clear.top >= clear.head - 1, `the focused control lands below the head, not under it: ${hidden} ${JSON.stringify(clear)}`);
        await me.capture("head-focus");

        const nudge = await scrollAndSample(page, { height: 420, reduced: false, top: 10 });
        await me.capture("head-small-scroll");
        const range = nudge.range;
        assert.ok(range > 40, `the page scrolls far enough to trim it: ${range}`);
        const short = await scrollAndSample(page, { height: 420 + range - 20, reduced: false, top: 20 });
        await me.capture("head-short-page");
        const still = await scrollAndSample(page, { height: 420 + range - 20, reduced: true, top: 20 });
        await me.capture("head-short-page-reduced-motion");
        assert.deepEqual({
          smallScrollOnALongPage: held(nudge.samples),
          shortPage: held(short.samples),
          shortPageRoomTrimmedOnceSettled: trimmed(short.full, short.samples),
          shortPageReducedMotion: held(still.samples),
          shortPageReducedMotionRoomTrimmed: trimmed(still.full, still.samples),
        }, {
          smallScrollOnALongPage: true,
          shortPage: true,
          shortPageRoomTrimmedOnceSettled: true,
          shortPageReducedMotion: true,
          shortPageReducedMotionRoomTrimmed: true,
        }, `the head keeps its scrolled form after a small scroll and on a page only just taller than the window, with and without motion, and the room ends at what the head gives up: ${JSON.stringify({ nudge, short, still })}`);
        await page.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "no-preference" }] });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 420, deviceScaleFactor: 1, mobile: false });
        await page.evaluate("document.querySelector('.main').scrollBy(0, 2000)");
        for (const until = Date.now() + STEP_MS; (await measure(page)).head !== "pj-head scrolled" && Date.now() < until;) await pause(POLL_MS);

        await me.click("more", ".pj-desc-more");
        const open = await measure(page);
        assert.deepEqual([open.more?.text, open.more?.expanded], ["less", "true"], "more opens the whole description");
        assert.ok(open.desc.clientHeight > open.desc.line + 1, `the open description shows more than one line: ${JSON.stringify(open.desc)}`);
        await me.capture("head-open");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
