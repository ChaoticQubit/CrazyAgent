import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { KEPT_MIN, REFUSED_EXIT, REQUIRED, countFindings, incomplete, joinSample, judge, margin, noise, noiseShares, privateFolder, quiet, sampleMetrics, schedule, stayedQuiet, summary, verdict } from "./speed-budget.mjs";

const QUIET = { load1: 0.8, compilers: [], memoryLevel: 80, onAC: true, screenLocked: false, busy: false };

test("summary takes the median and the nearest-rank p90 of the kept samples", () => {
  assert.deepEqual(summary([5, 1, 4, 2, 3]), { n: 5, median: 3, p90: 5 });
  assert.deepEqual(summary([10, 20, 30, 40, 50, 60, 70, 80, 90, 100]), { n: 10, median: 55, p90: 90 });
  assert.deepEqual(summary([7]), { n: 1, median: 7, p90: 7 });
});

test("summary refuses an empty list, a negative or a non-finite sample", () => {
  assert.throws(() => summary([]), /at least one sample/);
  assert.throws(() => summary([1, -2]), /finite and not negative/);
  assert.throws(() => summary([1, Number.NaN]), /finite and not negative/);
  assert.throws(() => summary([1, Infinity]), /finite and not negative/);
});

test("noise is the relative gap between two medians of the same build", () => {
  assert.equal(noise({ median: 100 }, { median: 104 }), 0.04);
  assert.equal(noise({ median: 104 }, { median: 100 }), 0.04);
  assert.equal(noise({ median: 50 }, { median: 50 }), 0);
});

test("the margin is 10% or twice the measured noise, whichever is larger", () => {
  assert.equal(margin(0), 0.1);
  assert.equal(margin(0.03), 0.1);
  assert.equal(margin(0.08), 0.16);
  assert.throws(() => margin(-0.1), /noise/);
});

test("a candidate inside the margin passes; one past both the margin and the floor fails", () => {
  const base = { n: 10, median: 200, p90: 240 };
  assert.deepEqual(verdict({ base, candidate: { n: 10, median: 215, p90: 250 }, margin: 0.1, floor: 20 }),
    { pass: true, reasons: [] });
  const slow = verdict({ base, candidate: { n: 10, median: 250, p90: 260 }, margin: 0.1, floor: 20 });
  assert.equal(slow.pass, false);
  assert.match(slow.reasons.join("\n"), /median 250 > 220 \(base 200 \+ 10%\) by 50 > floor 20/);
});

test("a gap past the margin but under the floor passes, so tiny absolute gaps never fail", () => {
  const base = { n: 10, median: 10, p90: 12 };
  const result = verdict({ base, candidate: { n: 10, median: 25, p90: 30 }, margin: 0.1, floor: 20 });
  assert.deepEqual(result, { pass: true, reasons: [] });
});

test("the p90 fails past twice the margin and twice the floor even when the median holds", () => {
  const base = { n: 10, median: 200, p90: 240 };
  const result = verdict({ base, candidate: { n: 10, median: 205, p90: 330 }, margin: 0.1, floor: 20 });
  assert.equal(result.pass, false);
  assert.match(result.reasons.join("\n"), /p90 330 > 288 \(base 240 \+ 20%\) by 90 > floor 40/);
});

test("verdict refuses mismatched sample counts or a zero floor", () => {
  const base = { n: 10, median: 200, p90: 240 };
  assert.throws(() => verdict({ base, candidate: { n: 9, median: 200, p90: 240 }, margin: 0.1, floor: 20 }), /same number of samples/);
  assert.throws(() => verdict({ base, candidate: base, margin: 0.1, floor: 0 }), /floor/);
});

test("call counts: a new command or one called more than once beyond the base is a finding", () => {
  const base = { start: { desktop_connect: 1, desktop_list_projects: 2 }, idle: { desktop_overview: 12 } };
  const candidate = { start: { desktop_connect: 1, desktop_list_projects: 3, desktop_load_extras: 1 }, idle: { desktop_overview: 14 } };
  assert.deepEqual(countFindings(base, candidate), [
    { scenario: "idle", cmd: "desktop_overview", base: 12, candidate: 14 },
    { scenario: "start", cmd: "desktop_load_extras", base: 0, candidate: 1 },
  ]);
});

test("call counts: fewer calls, or one more from poll timing, is no finding", () => {
  const base = { idle: { desktop_overview: 12, desktop_status: 4 } };
  const candidate = { idle: { desktop_overview: 13, desktop_status: 1 } };
  assert.deepEqual(countFindings(base, candidate), []);
});

test("call counts refuse a scenario the base never ran", () => {
  assert.throws(() => countFindings({ start: {} }, { start: {}, idle: {} }), /scenario idle/);
});

test("a quiet Mac passes the gate", () => {
  assert.deepEqual(quiet(QUIET), { quiet: true, reasons: [] });
});

test("load, compilers, memory pressure, battery, a locked screen or busy slots each refuse", () => {
  const cases = [
    [{ load1: 3.5 }, /load 3.5 > 2/],
    [{ compilers: ["rustc", "cargo"] }, /compilers running: rustc, cargo/],
    [{ memoryLevel: 30 }, /memory level 30 < 50/],
    [{ onAC: false }, /on battery/],
    [{ screenLocked: true }, /screen locked/],
    [{ busy: true }, /build slots busy/],
  ];
  for (const [change, reason] of cases) {
    const result = quiet({ ...QUIET, ...change });
    assert.equal(result.quiet, false, JSON.stringify(change));
    assert.match(result.reasons.join("\n"), reason);
  }
});

test("after a run the run's own load never taints it, every outside sign still does", () => {
  assert.deepEqual(stayedQuiet({ ...QUIET, load1: 6.5 }), { quiet: true, reasons: [] });
  for (const change of [{ compilers: ["rustc"] }, { memoryLevel: 30 }, { onAC: false }, { screenLocked: true }, { busy: true }]) {
    const result = stayedQuiet({ ...QUIET, load1: 6.5, ...change });
    assert.equal(result.quiet, false, JSON.stringify(change));
    assert.doesNotMatch(result.reasons.join("\n"), /load/);
  }
});

test("REFUSED exits 75 (EX_TEMPFAIL), never 0 or 1", () => {
  assert.equal(REFUSED_EXIT, 75);
});

const ENGINE = { engine_start_ms: 310, seeded: { project: "p", sessions: 50 }, round_trip_ms: { desktop_master_state: [3, 1, 2], desktop_open_project: [5, 4, 6] } };
const APP = {
  launch: { ms: 1450, first_look_usable: false, webview_ms: 380, reload_ms: 520 },
  big: { open_ms: 120, conversation_ms: 80, sessions_ms: 200 },
  switch_cold: { home: 40, metrics: 90 },
  switch_warm: { home: 30, metrics: 60 },
  memory: { engine_mb: 60.5, app_mb: 120, webkit_mb: [200, 30], webkit_pids: [11, 12] },
};

test("a run's records flatten into one value per metric, round trips as their median", () => {
  assert.deepEqual(sampleMetrics(ENGINE, APP), {
    launch_ms: 1450, launch_webview_ms: 380, reload_home_ms: 520, engine_start_ms: 310,
    big_open_ms: 120, big_conversation_ms: 80, big_sessions_ms: 200,
    switch_cold_home_ms: 40, switch_cold_metrics_ms: 90, switch_warm_home_ms: 30, switch_warm_metrics_ms: 60,
    round_trip_desktop_master_state_ms: 2, round_trip_desktop_open_project_ms: 5,
    memory_engine_mb: 60.5, memory_app_mb: 120, memory_webkit_mb: 230,
  });
});

test("a launch whose first look already found Home usable has no launch time, but always its webview start and reload", () => {
  const metrics = sampleMetrics(ENGINE, { ...APP, launch: { ...APP.launch, first_look_usable: true } });
  assert.equal(Object.hasOwn(metrics, "launch_ms"), false);
  assert.equal(metrics.launch_webview_ms, 380);
  assert.equal(metrics.reload_home_ms, 520);
  assert.equal(metrics.engine_start_ms, 310);
  assert.throws(() => sampleMetrics(ENGINE, { ...APP, launch: { ms: 1450, first_look_usable: true } }), /webview start and a reload/);
});

test("the schedule warms each side once, then alternates them", () => {
  assert.deepEqual(schedule(2), [
    { side: "base", warm: true }, { side: "candidate", warm: true },
    { side: "base", warm: false }, { side: "candidate", warm: false },
    { side: "base", warm: false }, { side: "candidate", warm: false },
  ]);
  assert.throws(() => schedule(0), /kept runs/);
});

test("noise shares compare a side with itself metric by metric", () => {
  const first = Array.from({ length: KEPT_MIN }, () => ({ launch_ms: 1000, memory_app_mb: 100 }));
  const second = Array.from({ length: KEPT_MIN }, () => ({ launch_ms: 1050, memory_app_mb: 100 }));
  assert.deepEqual(noiseShares(first, second), { launch_ms: 0.05, memory_app_mb: 0 });
});

test("judge passes, fails, reports and leaves unmeasured each metric by its rule", () => {
  const base = Array.from({ length: KEPT_MIN }, (_, index) => ({ launch_ms: 1000 + index, switch_cold_home_ms: 40, memory_webkit_mb: 200, memory_app_mb: 100 }));
  const candidate = Array.from({ length: KEPT_MIN }, (_, index) => ({ launch_ms: 1400 + index, switch_cold_home_ms: 42, memory_webkit_mb: 260, memory_app_mb: 101,
    ...(index < 2 ? {} : { round_trip_x_ms: 1 }) }));
  const rows = judge(base, candidate, { launch_ms: 0.02, switch_cold_home_ms: 0.01, memory_app_mb: 0, memory_webkit_mb: 0 });
  const by = Object.fromEntries(rows.map((row) => [row.metric, row.status]));
  assert.deepEqual(by, { launch_ms: "fail", memory_app_mb: "pass", memory_webkit_mb: "report", round_trip_x_ms: "unmeasured", switch_cold_home_ms: "pass" });
});

test("judge leaves a metric with no noise reading unmeasured, and the verdict names it as not judged", () => {
  const side = Array.from({ length: KEPT_MIN }, () => ({ reload_home_ms: 1000, switch_cold_home_ms: 40 }));
  const rows = judge(side, side, { switch_cold_home_ms: 0 });
  assert.deepEqual(rows.map((item) => [item.metric, item.status]), [["reload_home_ms", "unmeasured"], ["switch_cold_home_ms", "pass"]]);
  assert.match(rows[0].reasons.join(" "), /noise run never measured it/);
  assert.ok(incomplete(rows).includes("reload_home_ms"));
});

test("noise shares leave out a metric whose median is zero on either run, so only that metric goes unjudged", () => {
  const first = Array.from({ length: KEPT_MIN }, () => ({ round_trip_x_ms: 0, launch_webview_ms: 100 }));
  const second = Array.from({ length: KEPT_MIN }, () => ({ round_trip_x_ms: 0.4, launch_webview_ms: 104 }));
  assert.deepEqual(noiseShares(first, second), { launch_webview_ms: 0.04 });
  assert.deepEqual(noiseShares(second, first), { launch_webview_ms: 0.04 });
});

test("judge rows carry their floor: launches and engine start 100 ms, TUI keys and pages 20 ms, memory 10 MB", () => {
  const side = Array.from({ length: KEPT_MIN }, () => ({ tui_launch_ms: 300, tui_settings_ms: 30, engine_start_ms: 200, memory_engine_mb: 50 }));
  const rows = judge(side, side, { tui_launch_ms: 0, tui_settings_ms: 0, engine_start_ms: 0, memory_engine_mb: 0 });
  const floors = Object.fromEntries(rows.map((row) => [row.metric, row.floor]));
  assert.deepEqual(floors, { engine_start_ms: 100, memory_engine_mb: 10, tui_launch_ms: 100, tui_settings_ms: 20 });
});

test("a TUI sample joins a run's sample under its own names only", () => {
  const joined = joinSample({ launch_ms: 900, memory_app_mb: 80 }, { tui_launch_ms: 300, tui_home_ms: 12 });
  assert.deepEqual(joined, { launch_ms: 900, memory_app_mb: 80, tui_launch_ms: 300, tui_home_ms: 12 });
  assert.throws(() => joinSample({ launch_ms: 900 }, { launch_ms: 1 }), /tui_<step>_ms/);
  assert.throws(() => joinSample({ launch_ms: 900 }, { tui_home_ms: -1 }), /a reading/);
});

test("a verdict is incomplete while a required metric is missing or unmeasured; only the launch may be", () => {
  const row = (metric, status) => ({ metric, status });
  const whole = REQUIRED.map((metric) => row(metric, "pass"));
  assert.deepEqual(incomplete(whole), []);
  assert.deepEqual(incomplete([...whole, row("launch_ms", "unmeasured")]), []);
  assert.deepEqual(incomplete(whole.filter((item) => item.metric !== "reload_home_ms")), ["reload_home_ms"]);
  assert.deepEqual(incomplete([...whole.slice(1), row(REQUIRED[0], "unmeasured")]), [REQUIRED[0]]);
  assert.deepEqual(incomplete([...whole, row("switch_cold_home_ms", "unmeasured")]), ["switch_cold_home_ms"]);
  assert.ok(REQUIRED.includes("launch_webview_ms") && REQUIRED.includes("tui_launch_ms") && !REQUIRED.includes("launch_ms"));
});

test("judge leaves the optional launch unmeasured when the noise run never measured it", () => {
  const side = Array.from({ length: KEPT_MIN }, () => ({ launch_ms: 1000, reload_home_ms: 500 }));
  const rows = judge(side, side, { reload_home_ms: 0 });
  assert.deepEqual(rows.map((item) => [item.metric, item.status]), [["launch_ms", "unmeasured"], ["reload_home_ms", "pass"]]);
});

test("a speed state folder must be a real folder this user owns that no group or other user can write", () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-speed-owner-")));
  try {
    const state = join(root, "state");
    mkdirSync(state, { mode: 0o700 });
    assert.equal(privateFolder(state), state);
    chmodSync(state, 0o755);
    assert.equal(privateFolder(state), state, "read and search for others is fine");
    for (const mode of [0o775, 0o757, 0o777]) {
      chmodSync(state, mode);
      assert.throws(() => privateFolder(state), /no group or other user can write/, mode.toString(8));
    }
    chmodSync(state, 0o700);
    symlinkSync(state, join(root, "link"));
    assert.throws(() => privateFolder(join(root, "link")), /a real folder, not a link/);
    writeFileSync(join(root, "file"), "");
    assert.throws(() => privateFolder(join(root, "file")), /a real folder, not a link/);
    assert.throws(() => privateFolder(join(root, "missing")), /ENOENT/);
    assert.throws(() => privateFolder("relative/state"), /an absolute folder/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
