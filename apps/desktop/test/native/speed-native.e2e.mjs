import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { footprintMb, webkitHelpers } from "./speed-budget.mjs";

const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
const LOOK_MS = 10;
const LAUNCH_MS_MAX = 60000;
const USABLE_MS_MAX = 25000;
const SETTLE_MS = 1000;
const HOME_HASHES = ["", "#/", "#/home"];
const PROJECT_LINK = ".sidebar .nav-projects a.nav-item";
const SWITCHES = [
  ["project", PROJECT_LINK],
  ["agents", "a#tab-agents"],
  ["settings", '.sidebar nav.app-nav a[href="#/settings/accounts"]'],
  ["metrics", '.sidebar nav.app-nav a[href="#/metrics"]'],
  ["home", '.sidebar nav.app-nav a[href="#/home"]'],
];

/** Reads one JSON file the run wrote into the evidence folder. Input: its name. Output: the
 * value. */
function evidenceJson(name) {
  const text = readFileSync(join(evidence, name), "utf8");
  assert.ok(text.length > 0 && text.length < 1 << 20, `${name} is a bounded record`);
  return JSON.parse(text);
}

/** Whether Home is usable now, read in the page with the page's own clock. Output: the answer,
 * the wall-clock ms it was read at and the wall-clock ms the page's document started at. */
function homeNow() {
  return browser.execute((hashes) => ({
    ok: hashes.includes(location.hash) && Boolean(document.querySelector("main h1")) && !document.querySelector("main .skeleton"),
    at: performance.timeOrigin + performance.now(),
    origin: performance.timeOrigin,
  }), HOME_HASHES);
}

/** Reloads the app's page and times, with the new document's clock, from its start until Home
 * is usable: the app's own boot (scripts, engine connect, Home's reads) on every run, however
 * late WebDriver attached after the launch. Input: the old document's start. Output: the ms. */
async function reloadHome(before) {
  await browser.execute(() => { setTimeout(() => location.reload(), 0); });
  for (const deadline = Date.now() + LAUNCH_MS_MAX; ; await pause(LOOK_MS)) {
    const now = await homeNow().catch(() => null);
    if (now?.ok && now.origin > before) return now.at - now.origin;
    assert.ok(Date.now() < deadline, "Home never became usable after a reload");
  }
}

/** Clicks an element inside the page and times, with the page's clock, until the page at its
 * address is usable: the clicked link or button marked current, selected or expanded (so the
 * old page's DOM, still shown when the address changes, never counts), a heading in main, no
 * skeleton left in main, and `want` elements matching `extra` when given. A MutationObserver
 * and a 4 ms check both look, so a hidden window that draws no frames still finishes. Inputs:
 * the element's selector and the extra condition. Output: the ms from click to usable. */
async function timedClick(selector, extra = null, want = 0) {
  const result = await browser.executeAsync((selector, extra, want, limitMs, done) => {
    const target = document.querySelector(selector);
    if (!target) {
      done({ error: `nothing matches ${selector}` });
      return;
    }
    const hash = target.getAttribute("href") ?? location.hash;
    const marked = () => {
      const now = document.querySelector(selector);
      return Boolean(now) && (now.getAttribute("aria-current") === "page" || now.getAttribute("aria-selected") === "true"
        || now.getAttribute("aria-expanded") === "true");
    };
    const ready = () => location.hash === hash && marked() && Boolean(document.querySelector("main h1"))
      && !document.querySelector("main .skeleton") && (extra === null || document.querySelectorAll(extra).length >= want);
    let ended = false;
    let timer = 0;
    let limit = 0;
    const observer = new MutationObserver(() => finish());
    const end = (value) => {
      if (ended) return;
      ended = true;
      observer.disconnect();
      clearInterval(timer);
      clearTimeout(limit);
      done(value);
    };
    const began = performance.now();
    const finish = () => { if (ready()) end({ ms: performance.now() - began }); };
    target.click();
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, characterData: true });
    timer = setInterval(finish, 4);
    limit = setTimeout(() => end({ error: `not usable in ${limitMs} ms`, text: document.body.innerText.slice(0, 1500) }), limitMs);
    finish();
  }, selector, extra, want, USABLE_MS_MAX);
  assert.ok(!result.error, `${selector}: ${result.error} ${result.text ?? ""}`);
  assert.ok(Number.isFinite(result.ms) && result.ms >= 0, `a time for ${selector}`);
  return result.ms;
}

/** Finds the one running process of the app under test by its own binary path, which only this
 * run uses (the side's artifact folder). Output: its pid. */
function appPid() {
  const app = process.env.CRAZYAGENT_DESKTOP_TEST_APP;
  assert.match(app ?? "", /\/bin\/crazyagent-desktop$/, "the side's app binary");
  const listed = execFileSync("/bin/ps", ["-axww", "-o", "pid=,command="], { encoding: "utf8", maxBuffer: 16 << 20 });
  const pids = listed.split("\n").flatMap((line) => {
    const found = line.trim().match(/^(\d+)\s+(\S+)/);
    return found && found[2] === app ? [Number(found[1])] : [];
  });
  assert.equal(pids.length, 1, `exactly one ${app} runs: ${pids.join(", ")}`);
  return pids[0];
}

describe("speed rows on the real desktop app and the real engine (DEC-169)", () => {
  it("times launch, the big project, page switches and memory, and records them", async () => {
    const launch = evidenceJson("speed-launch.json");
    const engine = evidenceJson("speed-engine.json");
    const result = { launch: null, big: {}, switch_cold: {}, switch_warm: {}, memory: {} };
    let first = true;
    for (const deadline = Date.now() + LAUNCH_MS_MAX; ; await pause(LOOK_MS)) {
      const now = await homeNow();
      assert.ok(now.origin >= launch.at_ms, "the page started after the launch stamp");
      if (now.ok) {
        result.launch = { ms: now.at - launch.at_ms, first_look_usable: first, webview_ms: now.origin - launch.at_ms };
        break;
      }
      first = false;
      assert.ok(Date.now() < deadline, "Home never became usable after launch");
    }
    await browser.switchToWindow(await browser.getWindowHandle());
    await browser.setWindowSize(1180, 800);
    await pause(SETTLE_MS);
    result.launch.reload_ms = await reloadHome(launch.at_ms + result.launch.webview_ms);
    await pause(SETTLE_MS);
    const others = engine.seeded.sessions - 1;
    assert.ok(others >= 1, "the big project has sessions besides the live one");
    result.big.open_ms = await timedClick(PROJECT_LINK);
    result.big.conversation_ms = await timedClick("a#tab-conversation", '[data-testid="sessions-toggle"]', 1);
    result.big.sessions_ms = await timedClick('[data-testid="sessions-toggle"]', '[data-testid="resume-session"]', others);
    await timedClick('.sidebar nav.app-nav a[href="#/home"]');
    await pause(SETTLE_MS);
    for (const store of [result.switch_cold, result.switch_warm]) {
      for (const [page, selector] of SWITCHES) store[page] = await timedClick(selector);
      await pause(SETTLE_MS);
    }
    const helpers = webkitHelpers().filter((helper) => !launch.helpers.includes(helper));
    result.memory = {
      engine_mb: footprintMb(Number(process.env.SPEED_ENGINE_PID)),
      app_mb: footprintMb(appPid()),
      webkit_mb: helpers.map((helper) => footprintMb(helper)),
      webkit_pids: helpers,
    };
    writeFileSync(join(evidence, "speed-app.json"), JSON.stringify(result, null, 1), { flag: "wx", mode: 0o600 });
  });
});
