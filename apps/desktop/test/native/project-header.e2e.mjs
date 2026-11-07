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
      title: box(document.querySelector(".pj-head h1")),
      tabs: box(document.querySelector(".pj-head [role=tablist]")),
      desc: desc ? { text: desc.textContent, scrollWidth: desc.scrollWidth, clientWidth: desc.clientWidth, clientHeight: desc.clientHeight, line: parseFloat(getComputedStyle(desc).lineHeight) } : null,
      more: more ? { hidden: more.hidden, text: more.textContent, expanded: more.getAttribute("aria-expanded") } : null,
    };
  })()`);
}

test("the project title and tab bar stay on screen while the page scrolls, and the description wraps to two lines, one once scrolled", { timeout: 300000 }, async () => {
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
