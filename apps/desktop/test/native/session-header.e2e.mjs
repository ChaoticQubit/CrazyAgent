import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { newProject, prepareWindow } from "./master-settings-window.mjs";
import { go, paste, pick, reopen, run, tab, until, waitFor } from "./trial-kit.mjs";
import { startIfPaused, teamNodes } from "./trial4/n-d8-master-reply.mjs";

const UI_MS = 30000;
const MASTER_MS = 180000;
const SETTLE_MS = 1500;
const RUNNING = ["starting", "idle", "working"];
const ROW_SHARE_MAX = 0.02;
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };
const folder = mkdtempSync(join(tmpdir(), "session-header-"));

/**
 * Reads the boxes the header turns on, in CSS px from the page's top left: the page, the
 * native host, the sessions bar and its list. Output: the boxes, null for one not shown.
 */
function boxes() {
  return browser.execute(() => {
    const box = (element) => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return { x: rect.left, y: rect.top, width: rect.width, height: rect.height };
    };
    return {
      page: { width: window.innerWidth, height: window.innerHeight },
      host: box(document.querySelector("section.tg-terminal.embedded [data-native-terminal]")),
      bar: box(document.querySelector('[data-testid="master-sessions"]')),
      list: box(document.querySelector(".pj-session-list")),
    };
  });
}

/**
 * Finds the test app's one on-screen window as wide as the page, by the exact binary the
 * run started, never another process. Input: the page width. Output: { id, height }.
 */
function rigWindow(width) {
  const app = process.env.WD_APP;
  assert.ok(app, "the run names its app binary");
  const listed = spawnSync("/usr/bin/pgrep", ["-f", `^${app}`], { encoding: "utf8" });
  const pids = listed.stdout.split("\n").filter(Boolean).map(Number);
  assert.equal(pids.length, 1, `one test app process: ${listed.stdout}`);
  const script = "ObjC.import('CoreGraphics'); JSON.stringify(ObjC.deepUnwrap(ObjC.castRefToObject("
    + "$.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0))).filter((w) => "
    + `w.kCGWindowOwnerPID === ${pids[0]} && w.kCGWindowBounds.Width === ${width})`
    + ".map((w) => ({ id: w.kCGWindowNumber, height: w.kCGWindowBounds.Height })))";
  const result = spawnSync("/usr/bin/osascript", ["-l", "JavaScript", "-e", script],
    { encoding: "utf8" });
  assert.equal(result.status, 0, `the window list: ${result.stderr}`);
  const windows = JSON.parse(result.stdout.trim());
  assert.equal(windows.length, 1, `one test window as wide as the page: ${result.stdout}`);
  return windows[0];
}

/**
 * Reads a PNG's pixels through sips as a BMP. Input: the file. Output: { width, height, at }
 * where at(x, y) gives [r, g, b] with y counted from the top.
 */
function pixels(png) {
  const bmp = `${png}.bmp`;
  const made = spawnSync("/usr/bin/sips", ["-s", "format", "bmp", png, "--out", bmp]);
  assert.equal(made.status, 0, `sips read ${png}`);
  const data = readFileSync(bmp);
  assert.equal(data.toString("latin1", 0, 2), "BM", "sips wrote a BMP");
  const start = data.readUInt32LE(10);
  const [width, height, bits] = [data.readInt32LE(18), data.readInt32LE(22), data.readUInt16LE(28)];
  assert.ok((bits === 24 || bits === 32) && width > 0 && height !== 0, `a plain BMP: ${bits}`);
  const step = bits / 8;
  const stride = (width * step + 3) & ~3;
  const rows = Math.abs(height);
  const at = (x, y) => {
    const offset = start + (height > 0 ? rows - 1 - y : y) * stride + x * step;
    return [data[offset + 2], data[offset + 1], data[offset]];
  };
  return { width, height: rows, at };
}

/**
 * Captures the test window with its native layers and the page alone, then finds the worst
 * row of a page box where the two differ: a native view drawn over the box shows there.
 * Inputs: the step name, the box and the page size. Output: { row, share } for that row.
 */
async function worstRow(name, box, page) {
  const shown = rigWindow(page.width);
  const inset = shown.height - page.height;
  assert.ok(inset >= 0 && inset < 80, `the page starts inside the window: ${inset}`);
  const windowFile = join(folder, `${name}-window.png`);
  const pageFile = join(folder, `${name}-page.png`);
  const capture = ["-l", String(shown.id), "-o", "-x", windowFile];
  const taken = spawnSync("/usr/sbin/screencapture", capture);
  assert.equal(taken.status, 0, `screencapture: ${taken.stderr}`);
  await browser.saveScreenshot(pageFile);
  const [whole, alone] = [pixels(windowFile), pixels(pageFile)];
  const columns = [];
  for (let x = Math.ceil(box.x) + 4; x < Math.floor(box.x + box.width) - 4; x += 2) columns.push(x);
  assert.ok(columns.length > 10, "the box is wide enough to read");
  let worst = { row: null, share: 0 };
  for (let y = Math.ceil(box.y) + 1; y < Math.floor(box.y + box.height) - 1; y += 1) {
    const differ = columns.filter((x) => {
      const [a, b] = [whole.at(x, y + inset), alone.at(x, y)];
      return Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 48;
    }).length;
    if (differ / columns.length > worst.share) worst = { row: y, share: differ / columns.length };
  }
  return worst;
}

describe("The session header above a native master terminal (V243-NATIVE-SESSION-HEADER)", () => {
  before(async () => {
    await prepareWindow();
    await newProject("session-header-shop");
    await tab("conversation");
    await startIfPaused(kit, run.project);
    await waitFor(async () => {
      const node = (await teamNodes(run.project)).find((each) => each.kind === "master") ?? null;
      const done = Boolean(node?.terminal) && RUNNING.includes(node.status);
      return { done, seen: JSON.stringify(node).slice(0, 200) };
    }, MASTER_MS, "the master never ran in a terminal");
    await until(async () => browser.execute(() => Boolean(
      document.querySelector("section.tg-terminal.embedded .tg-terminal-note"))),
    UI_MS, "the master panel never went live");
    await pause(2 * SETTLE_MS);
  });

  after(() => rmSync(folder, { recursive: true, force: true }));

  it("draws nothing native over the sessions bar, with Sessions closed and open", async () => {
    const closed = await boxes();
    assert.ok(closed.host && closed.bar, "the master panel and its sessions bar show");
    const above = closed.bar.y + closed.bar.height <= closed.host.y + 1;
    assert.ok(above, "the bar sits above the host");
    const bar = await worstRow("closed", closed.bar, closed.page);
    const seenClosed = { bar: closed.bar, host: closed.host, worst: bar };
    console.log(`header closed ${JSON.stringify(seenClosed)}`);
    await $('[data-testid="sessions-toggle"]').click();
    await pause(SETTLE_MS);
    const opened = await boxes();
    assert.ok(opened.list, "Sessions shows its list");
    const list = await worstRow("opened", opened.list, opened.page);
    const seenOpened = { list: opened.list, host: opened.host, worst: list };
    console.log(`header opened ${JSON.stringify(seenOpened)}`);
    await $('[data-testid="sessions-toggle"]').click();
    assert.ok(bar.share <= ROW_SHARE_MAX, `native pixels over the bar: ${JSON.stringify(bar)}`);
    assert.ok(list.share <= ROW_SHARE_MAX, `native pixels over the list: ${JSON.stringify(list)}`);
  });
});
