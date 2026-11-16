import assert from "node:assert/strict";
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { FAILURE_TEXT_MAX, KEPT_MIN, REFUSED_EXIT, REQUIRED, clickUntilUsable, compareExit, countFindings, incomplete, joinSample, judge, margin, noise, noiseShares, prepareKeychain, privateFolder, quiet, runSchedule, sampleMetrics, schedule, stayedQuiet, summary, verdict } from "./speed-budget.mjs";

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

test("a run that timed an onboarding start records it as its own metric, and a seeding run that timed none records nothing (rv-code4 r2 F2)", () => {
  assert.equal(sampleMetrics({ ...ENGINE, onboard_start_ms: 42 }, APP).onboard_start_ms, 42);
  assert.equal(Object.hasOwn(sampleMetrics({ ...ENGINE, onboard_start_ms: null }, APP), "onboard_start_ms"), false);
  assert.equal(Object.hasOwn(sampleMetrics(ENGINE, APP), "onboard_start_ms"), false);
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
  assert.deepEqual(incomplete([...whole, row("switch_cold_settings_ms", "unmeasured")]), ["switch_cold_settings_ms"]);
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

/** Runs the page's click function against stand-ins for the page it runs in: a document whose
 * selector finds `target`, a heading and no skeleton, and an observer that never fires.
 * Inputs: the target and the limit in ms. Output: what the function passed to `done`. */
async function clickInPage(target, limitMs) {
  const kept = { document: globalThis.document, location: globalThis.location, MutationObserver: globalThis.MutationObserver };
  globalThis.document = {
    body: { innerText: "page text" },
    querySelector: (selector) => (selector === "#target" ? target : selector === "main h1" ? {} : null),
    querySelectorAll: () => [],
  };
  globalThis.location = { hash: "#/project" };
  globalThis.MutationObserver = class { observe() {} disconnect() {} };
  try {
    return await new Promise((resolve, reject) => {
      const guard = setTimeout(() => reject(new Error("done was never called")), 2000);
      clickUntilUsable("#target", null, 0, limitMs, (value) => { clearTimeout(guard); resolve(value); });
    });
  } finally {
    Object.assign(globalThis, kept);
  }
}

/** A button stand-in that counts its clicks and is expanded by one. Input: whether it is
 * disabled. Output: the button. */
function button(disabled) {
  const attributes = { "aria-expanded": "false" };
  return {
    disabled,
    clicks: 0,
    getAttribute: (name) => attributes[name] ?? null,
    click() { this.clicks += 1; attributes["aria-expanded"] = "true"; },
  };
}

test("the page's click refuses a disabled target at once, naming it, and never clicks it, so a race fails loud and not as a timeout", async () => {
  const target = button(true);
  const answer = await clickInPage(target, 1);
  assert.match(answer.error ?? "", /#target is disabled/);
  assert.equal(answer.ms, undefined);
  assert.equal(target.clicks, 0);
});

test("the page's click clicks an enabled target once and times it until the page is usable", async () => {
  const target = button(false);
  const answer = await clickInPage(target, 1000);
  assert.equal(answer.error, undefined, JSON.stringify(answer));
  assert.ok(Number.isFinite(answer.ms) && answer.ms >= 0, JSON.stringify(answer));
  assert.equal(target.clicks, 1);
});

const calm = { quietBefore: async () => null, quietAfter: () => ({ quiet: true, reasons: [] }) };
const progressOf = () => ({ samples: { base: [], candidate: [] }, failed: { base: 0, candidate: 0 }, runs: [] });

/** A stand-in launch that numbers its calls and fails the ones `fails` picks. Input: the pick,
 * from the call's number, side and warm-up flag. Output: the launch and its call count. */
function launches(fails) {
  const seen = { calls: 0 };
  seen.launch = async (side, warm) => {
    seen.calls += 1;
    if (fails(seen.calls, side, warm)) throw new Error(`run.mjs speed failed (1): run ${seen.calls} never showed Home`);
    return { run: { evidence: `/evidence/${seen.calls}` }, sample: warm ? null : { page_ms: seen.calls } };
  };
  return seen;
}

test("a failed run is recorded and counted, the schedule goes on, and the medians come only from good runs (SPEED-RUN-ABORT)", async () => {
  const kept = KEPT_MIN + 2;
  const progress = progressOf();
  const seen = launches((call) => call === 3);
  assert.equal(await runSchedule(kept, { ...calm, launch: seen.launch }, progress), null);
  assert.equal(seen.calls, 2 * kept + 2, "every slot ran once and the failed one was not run again");
  assert.deepEqual(progress.failed, { base: 1, candidate: 0 });
  assert.equal(progress.samples.base.length, kept - 1);
  assert.equal(progress.samples.candidate.length, kept);
  assert.ok(progress.samples.base.every((sample) => sample.page_ms !== 3), "the failed run gives no sample");
  assert.equal(progress.runs.length, 2 * kept + 2);
  assert.deepEqual(progress.runs.filter((run) => Object.hasOwn(run, "failed")), [
    { side: "base", warm: false, failed: "run.mjs speed failed (1): run 3 never showed Home" },
  ]);
  const rows = judge(progress.samples.base, progress.samples.candidate, { page_ms: 0.05 });
  assert.deepEqual(rows.map((row) => [row.metric, row.status]), [["page_ms", "pass"]], "the good runs alone are judged");
});

test("a failed warm-up is counted and the schedule goes on to every kept run", async () => {
  const progress = progressOf();
  const seen = launches((call) => call === 1);
  assert.equal(await runSchedule(KEPT_MIN, { ...calm, launch: seen.launch }, progress), null);
  assert.equal(seen.calls, 2 * KEPT_MIN + 2);
  assert.deepEqual(progress.failed, { base: 1, candidate: 0 });
  assert.equal(progress.samples.base.length, KEPT_MIN);
  assert.equal(progress.samples.candidate.length, KEPT_MIN);
});

test("a side that can no longer reach KEPT_MIN good runs ends the schedule there instead of running on to no verdict", async () => {
  const progress = progressOf();
  const seen = launches((_, side, warm) => side === "candidate" && !warm);
  assert.equal(await runSchedule(KEPT_MIN, { ...calm, launch: seen.launch }, progress), null);
  assert.equal(seen.calls, 4, "both warm-ups, one base run and the candidate's first failed run");
  assert.deepEqual(progress.failed, { base: 0, candidate: 1 });
  assert.equal(progress.samples.base.length, 1);
  assert.equal(progress.samples.candidate.length, 0);
});

test("a failed run's record keeps the tail of its error, bounded", async () => {
  const progress = progressOf();
  const long = `${"x".repeat(FAILURE_TEXT_MAX)} the stderr tail`;
  let calls = 0;
  const launch = async (side, warm) => {
    calls += 1;
    if (calls === 1) throw new Error(long);
    return { run: {}, sample: warm ? null : { page_ms: 1 } };
  };
  await runSchedule(KEPT_MIN, { ...calm, launch }, progress);
  const failed = progress.runs[0].failed;
  assert.equal(failed.length, FAILURE_TEXT_MAX);
  assert.ok(failed.endsWith(" the stderr tail"));
});

test("a run the Mac got busy through runs again, three in a row refuse, and a busy Mac before a run refuses at once", async () => {
  const busyAfter = progressOf();
  const seen = launches(() => false);
  const refused = await runSchedule(KEPT_MIN, { quietBefore: async () => null, launch: seen.launch, quietAfter: () => ({ quiet: false, reasons: ["load 4"] }) }, busyAfter);
  assert.deepEqual(refused, ["3 runs in a row saw the Mac get busy"]);
  assert.equal(seen.calls, 3);
  assert.equal(busyAfter.runs.length, 3);
  const busyBefore = progressOf();
  const none = launches(() => false);
  assert.deepEqual(await runSchedule(KEPT_MIN, { quietBefore: async () => ["a build runs"], launch: none.launch, quietAfter: calm.quietAfter }, busyBefore), ["a build runs"]);
  assert.equal(none.calls, 0);
});

/** A stand-in keychain tool: records each call, prints for a home its own login keychain as its
 * search list and default unless `answer` prints something else, and runs no real tool. Input:
 * the answer, given the home, the arguments and the calls so far, or null to print the home's
 * own. Output: the tool and its calls. */
function keychainTool(answer = () => null) {
  const calls = [];
  const tool = (home, args) => {
    calls.push([home, ...args]);
    const given = answer(home, args, calls);
    if (given !== null) return given;
    const reads = args.length === 3 && (args[0] === "list-keychains" || args[0] === "default-keychain");
    return reads ? printed(join(home, "Library", "Keychains", "login.keychain-db")) : "";
  };
  return { tool, calls };
}

const printed = (...paths) => `${paths.map((path) => `    "${path}"`).join("\n")}\n`;
const loginOf = (home) => join(home, "Library", "Keychains", "login.keychain-db");
const setBefore = (calls, setting) => calls.some((call) => call[1] === setting && call.includes("-s"));

/** Runs `check` with a new speed state home under /private/tmp and an owner home stand-in, then
 * removes both. Input: the check, given the home and the owner home. Output: none. */
function withStateHome(check) {
  const state = realpathSync(mkdtempSync("/private/tmp/cas-noise-base-0123abcd-"));
  const owner = realpathSync(mkdtempSync(join(tmpdir(), "ca-speed-owner-")));
  try {
    const home = join(state, "home");
    mkdirSync(home, { mode: 0o700 });
    mkdirSync(join(owner, "Library", "Preferences"), { recursive: true, mode: 0o700 });
    check(home, owner);
  } finally {
    rmSync(state, { recursive: true, force: true });
    rmSync(owner, { recursive: true, force: true });
  }
}

const MAC_ONLY = { skip: process.platform !== "darwin" && "a speed state home lies under /private/tmp on macOS" };

test("a new speed state home gets its own empty login keychain as its default and its only search entry, as the gate prepared by hand (DEC-146), with the owner's search list and default read before and after", MAC_ONLY, () => {
  withStateHome((home, owner) => {
    const { tool, calls } = keychainTool();
    const keychain = loginOf(home);
    assert.equal(prepareKeychain(home, owner, tool), keychain);
    const ownerReads = [[owner, "list-keychains", "-d", "user"], [owner, "default-keychain", "-d", "user"]];
    assert.deepEqual(calls, [
      ...ownerReads,
      [home, "create-keychain", "-p", "", keychain],
      [home, "set-keychain-settings", keychain],
      [home, "list-keychains", "-d", "user", "-s", keychain],
      [home, "default-keychain", "-d", "user", "-s", keychain],
      [home, "list-keychains", "-d", "user"],
      [home, "default-keychain", "-d", "user"],
      ...ownerReads,
    ]);
    assert.ok(calls.filter((call) => call.length > 4).every((call) => call[0] === home), "every call that changes a setting runs on the state home");
    for (const folder of ["Library", "Library/Keychains", "Library/Preferences"]) {
      assert.equal(lstatSync(join(home, folder)).mode & 0o777, 0o700, folder);
    }
  });
});

test("a keychain is prepared only in a new speed state home, only as its own, and never with the owner's keychain settings changed", MAC_ONLY, () => {
  withStateHome((home, owner) => {
    const outside = keychainTool();
    assert.throws(() => prepareKeychain(owner, owner, outside.tool), /a speed state home/);
    assert.deepEqual(outside.calls, [], "no keychain tool runs outside a state home");
    const shared = keychainTool((at, args) => at === home && args.length === 3 && args[0] === "list-keychains" ? printed(loginOf(home), loginOf(owner)) : null);
    assert.throws(() => prepareKeychain(home, owner, shared.tool), /only its own keychain/);
    assert.throws(() => prepareKeychain(home, owner, keychainTool().tool), /no Library yet/, "a home is prepared once");
  });
  withStateHome((home, owner) => {
    const settings = join(owner, "Library", "Preferences", "com.apple.security.plist");
    const leaky = keychainTool((_, args) => {
      if (args[0] === "create-keychain") writeFileSync(settings, "changed");
      return null;
    });
    assert.throws(() => prepareKeychain(home, owner, leaky.tool), /the owner's keychain settings/);
    assert.ok(existsSync(settings));
  });
  withStateHome((home, owner) => {
    writeFileSync(join(owner, "Library", "Preferences", "com.apple.security.plist"), "the owner's");
    const before = keychainTool();
    assert.throws(() => prepareKeychain(home, owner, before.tool), /the owner's keychain settings file stays absent/);
    assert.deepEqual(before.calls, [], "no keychain tool runs while the owner has a settings file");
  });
});

test("a keychain prepared for a state home that reached the owner's search list or default keychain stops the run", MAC_ONLY, () => {
  withStateHome((home, owner) => {
    const listLeak = keychainTool((at, args, calls) => at === owner && args[0] === "list-keychains" && setBefore(calls, "list-keychains") ? printed(loginOf(owner), loginOf(home)) : null);
    assert.throws(() => prepareKeychain(home, owner, listLeak.tool), /the owner's search list is unchanged/);
  });
  withStateHome((home, owner) => {
    const defaultLeak = keychainTool((at, args, calls) => at === owner && args[0] === "default-keychain" && setBefore(calls, "default-keychain") ? printed(loginOf(home)) : null);
    assert.throws(() => prepareKeychain(home, owner, defaultLeak.tool), /the owner's default keychain is unchanged/);
  });
});

test("a compare fails when a metric fails or is not judged, or when the candidate failed more runs than the base", () => {
  const pass = [{ metric: "page_ms", status: "pass" }];
  assert.equal(compareExit(pass, [], { base: 0, candidate: 0 }), 0);
  assert.equal(compareExit(pass, [], { base: 1, candidate: 1 }), 0, "a hiccup both sides met is counted, not failed");
  assert.equal(compareExit(pass, [], { base: 2, candidate: 0 }), 0, "the base's own failures are counted only");
  assert.equal(compareExit(pass, [], { base: 0, candidate: 1 }), 1, "a run only the candidate failed is a finding");
  assert.equal(compareExit(pass, [], { base: 1, candidate: 2 }), 1);
  assert.equal(compareExit([{ metric: "page_ms", status: "fail" }], [], { base: 0, candidate: 0 }), 1);
  assert.equal(compareExit(pass, ["tui_launch_ms"], { base: 0, candidate: 0 }), 1);
});

test("speed-run gives each state home its prepared keychain as it makes the home", () => {
  const runner = readFileSync(new URL("./speed-run.mjs", import.meta.url), "utf8");
  assert.match(runner, /mkdirSync\(join\(state, "home"\), \{ mode: 0o700 \}\);\s*prepareKeychain\(join\(state, "home"\)\);/);
});
