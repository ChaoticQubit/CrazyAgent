import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { loadavg } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { LIMIT_MS, SWITCHES, TARGET_MS, line, report } from "./switch-time-kit.mjs";

/** How long one switch may take to show its target before it counts as never shown, in ms.
 * It is far under the WebDriver request limit, so a stuck switch is a sample, not a lost run. */
export const SHOW_MS_MAX = 10000;

/** How long a row stays away before it switches back, in ms: longer than the 250 ms a hidden
 * terminal batches its output for, so a switch back meets a view that was truly hidden. */
export const AWAY_MS = 1000;

/** How long leaving may take until the target is off the screen, in ms. */
const GONE_MS_MAX = 10000;
const LOOK_MS = 50;

/** How many frame gaps a row times in its idle window before it switches. */
export const PERIOD_FRAMES = 30;

/** How long timing those frame gaps may take, in ms. */
const PERIOD_MS_MAX = 5000;

/** Runs inside the app page and uses nothing outside itself. It times the gaps between
 * `frames` + 1 frame callbacks with no switch going on: the grid every switch time sits on,
 * 8 or 17 ms on a display at full rate and 33 ms where the system halves it to save power.
 * Inputs: `{ frames, limit_ms }` and the callback. Output, to the callback: `{ period_ms,
 * min_ms, frames }`, the median and the shortest gap in ms and how many gaps were timed, both
 * times null when `limit_ms` passed before the window drew them all. */
export function inPageFramePeriod(request, done) {
  const { frames: wanted, limit_ms: limitMs } = request;
  const stamps = [];
  let over = false;
  const finish = (answer) => {
    if (over) return;
    over = true;
    clearTimeout(limit);
    done(answer);
  };
  const limit = setTimeout(() => finish({ period_ms: null, min_ms: null, frames: Math.max(0, stamps.length - 1) }), limitMs);
  const look = () => {
    if (over) return;
    stamps.push(performance.now());
    if (stamps.length <= wanted) {
      requestAnimationFrame(look);
      return;
    }
    const gaps = stamps.slice(1).map((stamp, index) => stamp - stamps[index]).sort((left, right) => left - right);
    finish({ period_ms: gaps[Math.floor(gaps.length / 2)], min_ms: gaps[0], frames: gaps.length });
  };
  requestAnimationFrame(look);
}

/** Runs inside the app page and uses nothing outside itself, so `executeAsync` and a Chrome
 * page's evaluate can both carry it. With `click` null it answers `{ shown }` now. Else it
 * checks the target is off the screen, notes the time, clicks the link, and looks on every
 * frame until the target is on screen: a terminal view's element inside `target.scope`
 * matching `target.rows` whose screen, read from its buffer through the element's
 * readScreen so it reads the same under every Terminal renderer, holds `target.text`, that
 * passes the browser's visibility check and is the thing a press at its middle would hit,
 * so text kept in the page while its view is hidden never counts.
 * Inputs: `{ click, target: { scope, rows, text }, limit_ms }` and the callback. Output, to
 * the callback: `{ ms, frame_ms, frames }`, ms from the click to the first task after the
 * frame that showed the target and to that frame's own callback, null when `limit_ms`
 * passed first; or `{ error }`. */
export function inPageSwitch(request, done) {
  const { click, target, limit_ms: limitMs } = request;
  const shown = () => {
    for (const scope of document.querySelectorAll(target.scope)) {
      for (const row of scope.querySelectorAll(target.rows)) {
        if (!row.readScreen().join("\n").includes(target.text)) continue;
        if (typeof row.checkVisibility === "function" && !row.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) continue;
        const box = row.getBoundingClientRect();
        if (!(box.width > 0 && box.height > 0)) continue;
        const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
        if (hit && scope.contains(hit)) return true;
      }
    }
    return false;
  };
  if (click === null) {
    done({ shown: shown() });
    return;
  }
  const link = document.querySelector(click);
  if (!link) {
    done({ error: `nothing matches ${click}` });
    return;
  }
  if (shown()) {
    done({ error: `the target is on screen before the click on ${click}` });
    return;
  }
  let frames = 0;
  let over = false;
  const finish = (answer) => {
    if (over) return;
    over = true;
    clearTimeout(limit);
    done(answer);
  };
  const limit = setTimeout(() => finish({ ms: null, frame_ms: null, frames }), limitMs);
  const start = performance.now();
  link.click();
  const look = () => {
    if (over) return;
    frames += 1;
    if (!shown()) {
      requestAnimationFrame(look);
      return;
    }
    const frameMs = performance.now() - start;
    const painted = new MessageChannel();
    painted.port1.onmessage = () => finish({ ms: performance.now() - start, frame_ms: frameMs, frames });
    painted.port2.postMessage(0);
  };
  requestAnimationFrame(look);
}

function checked(request) {
  assert.ok(request.click === null || /\S/.test(request.click), "a switch clicks a link, or null only asks what is shown");
  for (const part of ["scope", "rows", "text"]) assert.match(request.target?.[part] ?? "", /\S/, `a target names its ${part}`);
  assert.ok(Number.isInteger(request.limit_ms) && request.limit_ms > 0 && request.limit_ms <= SHOW_MS_MAX, `a switch has a limit up to ${SHOW_MS_MAX} ms`);
  return request;
}

/** The text a Chrome page evaluates, awaiting its promise, to run one switch where no
 * WebDriver is. Input: the request `inPageSwitch` takes. Output: the expression. */
export function switchExpression(request) {
  return `new Promise((done) => (${inPageSwitch.toString()})(${JSON.stringify(checked(request))}, done))`;
}

/** Whether a target is on screen now, in the real window. Input: the target. Output: the
 * answer. */
export async function onScreen(target) {
  const answer = await browser.executeAsync(inPageSwitch, checked({ click: null, target, limit_ms: SHOW_MS_MAX }));
  assert.equal(typeof answer?.shown, "boolean", `the page answered what is shown: ${JSON.stringify(answer)}`);
  return answer.shown;
}

/** The real window's idle frame period now. Output: `{ period_ms, min_ms, frames }` as
 * `inPageFramePeriod` answers it, the times null for a window that drew too few frames. */
export async function framePeriod() {
  const answer = await browser.executeAsync(inPageFramePeriod, { frames: PERIOD_FRAMES, limit_ms: PERIOD_MS_MAX });
  assert.ok(Number.isInteger(answer?.frames) && answer.frames >= 0 && answer.frames <= PERIOD_FRAMES, `the page answered its frame period: ${JSON.stringify(answer)}`);
  assert.ok(answer.period_ms === null ? answer.frames < PERIOD_FRAMES : answer.min_ms > 0 && answer.period_ms >= answer.min_ms,
    `a frame period is a time, or null for a window that drew too few frames: ${JSON.stringify(answer)}`);
  return { period_ms: answer.period_ms, min_ms: answer.min_ms, frames: answer.frames };
}

/** Times one switch in the real window. Inputs: `{ click, target }`, the link's selector and
 * the target. Output: the sample `{ ms, frame_ms, frames, load1 }`, with ms null for a
 * target that never showed; throws when the switch could not be timed at all: no link, the
 * target already on screen, or a window that drew no frame. */
export async function timeSwitch({ click, target }) {
  assert.match(click ?? "", /\S/, "a timed switch clicks a link");
  const load1 = loadavg()[0];
  const answer = await browser.executeAsync(inPageSwitch, checked({ click, target, limit_ms: SHOW_MS_MAX }));
  assert.ok(answer && answer.error === undefined, `the switch on ${click} could not be timed: ${answer?.error}`);
  assert.ok(Number.isInteger(answer.frames) && answer.frames > 0,
    `the window drew no frame in ${SHOW_MS_MAX} ms after the click on ${click}: it is covered, hidden or on a locked screen, so nothing was timed`);
  assert.ok(answer.ms === null || (Number.isFinite(answer.ms) && Number.isFinite(answer.frame_ms) && answer.frame_ms >= 0 && answer.ms >= answer.frame_ms),
    `a shown target has both its times: ${JSON.stringify(answer)}`);
  return { ms: answer.ms, frame_ms: answer.frame_ms, frames: answer.frames, load1 };
}

/** Runs one row (DEC-187): a warm-up switch, then `SWITCHES` timed ones. Each leaves with
 * `away`, waits until the target is off the screen and `AWAY_MS` more, then times the click
 * back. Before the first switch it times the window's idle frame period, the grid the times
 * sit on. Writes the samples, that period and the report to `switch-time-<slug>.json` in the
 * evidence folder and prints the row's line. Inputs: `{ name, away, back, target, evidence }`: the row's
 * name, a function that leaves, the selector of the link back, the target and the folder.
 * Output: the report with its line; the caller asserts `pass`. */
export async function measureRow({ name, away, back, target, evidence }) {
  assert.match(name, /^[a-z0-9]+(-[a-z0-9]+)*$/, "a row's name is lower-case words joined by dashes");
  assert.equal(typeof away, "function", "a row knows how to leave");
  assert.ok(isAbsolute(evidence ?? ""), "the evidence folder is an absolute path");
  const frames = await framePeriod();
  const samples = [];
  for (let turn = 0; turn <= SWITCHES; turn += 1) {
    await away();
    for (const deadline = Date.now() + GONE_MS_MAX; await onScreen(target); await pause(LOOK_MS)) {
      assert.ok(Date.now() < deadline, `${name}: the target stayed on screen ${GONE_MS_MAX} ms after leaving`);
    }
    await pause(AWAY_MS);
    samples.push(await timeSwitch({ click: back, target }));
  }
  const [warmUp, ...timed] = samples;
  assert.equal(timed.length, SWITCHES, "a row times every switch after its warm-up");
  const result = { ...report(timed), target_ms: TARGET_MS, limit_ms: LIMIT_MS };
  result.line = line(name, result);
  writeFileSync(join(evidence, `switch-time-${name}.json`), JSON.stringify({ name, back, target, away_ms: AWAY_MS, frame_period: frames, warm_up: warmUp, samples: timed, report: result }, null, 1), { mode: 0o600 });
  console.log(`SWITCH-TIME ${result.line}`);
  console.log(`SWITCH-TIME ${name} frames: idle period ${frames.period_ms === null ? "not timed" : `${frames.period_ms.toFixed(1)} ms, shortest ${frames.min_ms.toFixed(1)} ms`} over ${frames.frames} frames`);
  return result;
}
