import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, lstatSync, mkdirSync } from "node:fs";
import { userInfo } from "node:os";
import { basename, dirname, isAbsolute, join } from "node:path";

/** Exit code of a speed run the Mac was too busy to judge: EX_TEMPFAIL, so a gate reads it as
 * not run, never as a pass or a failure. */
export const REFUSED_EXIT = 75;
/** Most samples one summary takes; a run keeps 10 per side (DEC-169). */
export const SAMPLES_MAX = 64;
/** Smallest margin a timed row allows over the 2.3.2 base, whatever the measured noise. */
export const MARGIN_MIN = 0.1;
/** Gaps under these never fail a row, so a few milliseconds or megabytes of jitter on a small
 * number cannot turn it red. */
export const FLOOR = { page_ms: 20, round_trip_ms: 20, launch_ms: 100, memory_mb: 10 };
/** The quiet-Mac gate: 1-minute load at most `load1_max` on this 12-core Mac, the kernel's
 * memory level at least `memory_level_min` percent free. */
export const QUIET_LIMITS = { load1_max: 2, memory_level_min: 50 };
/** Processes whose presence means a build is running and timings are noise. */
export const COMPILERS = ["rustc", "cargo", "clang", "ld", "ld64", "swift-frontend", "xcodebuild"];
const COUNT_SLACK = 1;
/** Commands 2.3.2 never made that the v2.5 engine watch adds (250-S3C), each with the most
 * calls one scenario may make: the page's one `desktop_watch` at load, and one harness
 * `e2e_watch_next` per delivery in place of the app's channel: the first at once, at most 12
 * unknown turns (one per 5 s) and 3 empty 25 s waits in a one-minute scenario. */
export const WATCH_CALLS_MAX = Object.freeze({ desktop_watch: 1, e2e_watch_next: 16 });
/** A speed run's kept engine state folder, as speed-run.mjs makes it: new, private and short
 * enough for the engine's socket address. */
export const SPEED_STATE = /^\/private\/tmp\/cas-(noise|compare)-(base|candidate)-[0-9a-f]{8}-[A-Za-z0-9]{6}$/;
/** Most runs in a row the Mac may get busy through before a speed run is refused. */
const TAINTED_MAX = 3;
/** Most characters of a failed run's error its record keeps: the tail, where run.mjs puts the
 * child's stderr. */
export const FAILURE_TEXT_MAX = 4000;
/** The macOS keychain tool, and the longest one call of it may take. */
const SECURITY = "/usr/bin/security";
const SECURITY_MS_MAX = 30_000;

export const round = (value) => Number(value.toFixed(2));

/** Summarises timed samples. Input: 1 to `SAMPLES_MAX` finite, non-negative numbers. Output:
 * the count, the median and the nearest-rank 90th percentile. */
export function summary(samples) {
  assert.ok(Array.isArray(samples) && samples.length >= 1, "a summary needs at least one sample");
  assert.ok(samples.length <= SAMPLES_MAX, `a summary takes at most ${SAMPLES_MAX} samples`);
  for (const sample of samples) {
    assert.ok(Number.isFinite(sample) && sample >= 0, `every sample is finite and not negative: ${sample}`);
  }
  const sorted = [...samples].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  const median = sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
  const p90 = sorted[Math.ceil(0.9 * sorted.length) - 1];
  assert.ok(sorted[0] <= median && median <= sorted.at(-1), "the median lies inside the samples");
  assert.ok(median <= p90, "the p90 is not under the median");
  return { n: sorted.length, median, p90 };
}

/** The noise of one build against itself. Input: two summaries of the same artifacts. Output:
 * their medians' gap as a share of the smaller one. */
export function noise(first, second) {
  assert.ok(first.median >= 0 && second.median >= 0, "medians are not negative");
  const smaller = Math.min(first.median, second.median);
  assert.ok(smaller > 0, "a noise reading needs a median above zero");
  const share = Math.abs(first.median - second.median) / smaller;
  assert.ok(Number.isFinite(share) && share >= 0);
  return round(share);
}

/** The margin a row allows over the base. Input: the measured self-noise share. Output: the
 * larger of `MARGIN_MIN` and twice the noise. */
export function margin(selfNoise) {
  assert.ok(Number.isFinite(selfNoise) && selfNoise >= 0, `the noise share is a finite share: ${selfNoise}`);
  const allowed = Math.max(MARGIN_MIN, round(2 * selfNoise));
  assert.ok(allowed >= MARGIN_MIN);
  return allowed;
}

/** One limit check: a value fails when it is past the base by the share and by the floor.
 * Inputs: the label, the candidate's and base's values, the share and the floor. Output: the
 * reason it fails, or null. */
function past(label, candidate, base, share, floor) {
  assert.ok(share > 0 && floor > 0);
  const limit = base * (1 + share);
  const gap = candidate - base;
  if (candidate <= limit || gap <= floor) return null;
  return `${label} ${round(candidate)} > ${round(limit)} (base ${round(base)} + ${round(share * 100)}%) by ${round(gap)} > floor ${round(floor)}`;
}

/** Judges a candidate against the 2.3.2 base (DEC-169 section 3). Input: both summaries, the
 * margin share and the row's floor. Output: whether it passes and why not. */
export function verdict({ base, candidate, margin: share, floor }) {
  assert.equal(candidate.n, base.n, "base and candidate keep the same number of samples");
  assert.ok(Number.isFinite(floor) && floor > 0, `a row has a floor above zero: ${floor}`);
  assert.ok(share >= MARGIN_MIN, `the margin is at least ${MARGIN_MIN}`);
  const reasons = [
    past("median", candidate.median, base.median, share, floor),
    past("p90", candidate.p90, base.p90, 2 * share, 2 * floor),
  ].filter((reason) => reason !== null);
  assert.ok(reasons.length <= 2);
  return { pass: reasons.length === 0, reasons };
}

/** Compares bridge call counts per scenario (row C1). Inputs: the base's and the candidate's
 * `{ scenario: { cmd: count } }`, the same scenarios on both sides. Output: each command the
 * candidate calls that the base never did, past its `WATCH_CALLS_MAX` for the watch's own, or
 * more than `COUNT_SLACK` times more often, sorted by scenario and command. */
export function countFindings(base, candidate) {
  const scenarios = Object.keys(candidate).sort();
  for (const scenario of scenarios) assert.ok(Object.hasOwn(base, scenario), `the base ran scenario ${scenario}`);
  for (const scenario of Object.keys(base)) assert.ok(Object.hasOwn(candidate, scenario), `the candidate ran scenario ${scenario}`);
  const findings = [];
  for (const scenario of scenarios) {
    for (const cmd of Object.keys(candidate[scenario]).sort()) {
      const now = candidate[scenario][cmd];
      const was = base[scenario][cmd] ?? 0;
      assert.ok(Number.isInteger(now) && now >= 0 && Number.isInteger(was) && was >= 0);
      const limit = was > 0 ? was + COUNT_SLACK : Object.hasOwn(WATCH_CALLS_MAX, cmd) ? WATCH_CALLS_MAX[cmd] : 0;
      if (now > limit) findings.push({ scenario, cmd, base: was, candidate: now });
    }
  }
  return findings;
}

/** Judges whether the Mac is quiet enough to time anything. Input: one reading of load, the
 * compilers running, the memory level, power, screen lock and the build slots. Output: whether
 * it is quiet and each reason it is not. */
export function quiet(reading) {
  assert.ok(Number.isFinite(reading.load1) && reading.load1 >= 0, "a load reading");
  assert.ok(Array.isArray(reading.compilers), "a compiler list");
  const reasons = [];
  if (reading.load1 > QUIET_LIMITS.load1_max) reasons.push(`load ${reading.load1} > ${QUIET_LIMITS.load1_max}`);
  if (reading.compilers.length > 0) reasons.push(`compilers running: ${reading.compilers.join(", ")}`);
  if (reading.memoryLevel < QUIET_LIMITS.memory_level_min) reasons.push(`memory level ${reading.memoryLevel} < ${QUIET_LIMITS.memory_level_min}`);
  if (!reading.onAC) reasons.push("on battery");
  if (reading.screenLocked) reasons.push("screen locked");
  if (reading.busy) reasons.push("build slots busy");
  return { quiet: reasons.length === 0, reasons };
}

/** Judges whether the Mac stayed quiet through a timed run. The run itself (app, WebKit,
 * engine, WebDriver) lifts the load, so load is left to the gate before the next run; every
 * outside sign still taints the run. Input: one reading taken right after the run. Output:
 * as `quiet`. */
export function stayedQuiet(reading) {
  assert.ok(Number.isFinite(reading.load1) && reading.load1 >= 0, "a load reading");
  const result = quiet({ ...reading, load1: 0 });
  assert.ok(result.reasons.every((reason) => !reason.startsWith("load")));
  return result;
}

/** Runs one fixed-path read-only command. Input: the program and its arguments. Output: its
 * standard output. */
export function output(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8", timeout: 10000, maxBuffer: 16 << 20 });
  assert.equal(result.status, 0, `${program} ${args.join(" ")} failed: ${result.stderr}`);
  return result.stdout;
}

/** Reads the Mac's state for `quiet`, read-only: sysctl, ps, pmset and ioreg, plus the build
 * slots through `SPEED_BUSY_CMD` when set (an executable that exits 0 when no build holds or
 * waits for a slot). Input: none. Output: the reading. */
export function readQuiet() {
  const load1 = Number(output("/usr/sbin/sysctl", ["-n", "vm.loadavg"]).trim().replace(/[{}]/g, "").trim().split(/\s+/)[0]);
  const names = output("/bin/ps", ["-axo", "comm="]).split("\n").map((line) => line.trim().split("/").at(-1));
  const compilers = COMPILERS.filter((name) => names.includes(name));
  const memoryLevel = Number(output("/usr/sbin/sysctl", ["-n", "kern.memorystatus_level"]).trim());
  const onAC = output("/usr/bin/pmset", ["-g", "batt"]).includes("'AC Power'");
  const screenLocked = /"CGSSessionScreenIsLocked"\s*=\s*Yes/.test(output("/usr/sbin/ioreg", ["-n", "Root", "-d1"]));
  const check = process.env.SPEED_BUSY_CMD;
  const busy = Boolean(check) && spawnSync(check, [], { stdio: "ignore", timeout: 10000 }).status !== 0;
  assert.ok(Number.isFinite(load1) && Number.isFinite(memoryLevel), "sysctl gave numbers");
  return { load1, compilers, memoryLevel, onAC, screenLocked, busy };
}

/** The kinds of WebKit helper process the speed rows read, as their process names end. */
export const WEBKIT_KINDS = ["WebContent", "Networking", "GPU"];

/** Finds the WebKit helpers in a `ps -axo pid=,comm=` listing. Input: the listing. Output:
 * `[{ pid, kind }]` for every WebContent, Networking and GPU process, in listing order. */
export function webkitHelpersIn(listing) {
  const helpers = listing.split("\n").flatMap((line) => {
    const found = line.trim().match(/^(\d+)\s+(?:.*\/)?com\.apple\.WebKit\.(WebContent|Networking|GPU)$/);
    return found ? [{ pid: Number(found[1]), kind: found[2] }] : [];
  });
  assert.ok(helpers.every(({ pid, kind }) => Number.isInteger(pid) && pid > 1 && WEBKIT_KINDS.includes(kind)));
  return helpers;
}

/** WebKit helper processes run under launchd, not under the app, and their command lines name
 * no client; the speed rows find the ones the app started as the pids new since its launch.
 * Input: none. Output: `[{ pid, kind }]` for every WebKit helper running now. */
export function webkitHelpers() {
  return webkitHelpersIn(output("/bin/ps", ["-axo", "pid=,comm="]));
}

/** Reads one process's memory footprint, the number Activity Monitor shows as Memory.
 * Input: the pid. Output: megabytes. */
export function footprintMb(pid) {
  assert.ok(Number.isInteger(pid) && pid > 1, `a pid: ${pid}`);
  const found = output("/usr/bin/footprint", ["-p", String(pid)]).match(/Footprint: ([\d.]+) (KB|MB|GB)/);
  assert.ok(found, `footprint read pid ${pid}`);
  const megabytes = Number(found[1]) * { KB: 1 / 1024, MB: 1, GB: 1024 }[found[2]];
  assert.ok(Number.isFinite(megabytes) && megabytes > 0);
  return round(megabytes);
}

/** Fewest valid samples per side a metric needs to be judged (DEC-169 keeps 10). */
export const KEPT_MIN = 8;
/** Metrics read and shown but never failing a run until their reading is proven (DEC-169 Q9). */
export const REPORT_ONLY = new Set(["memory_webkit_mb", "memory_webkit_idle_mb", "memory_app_idle_mb",
  ...WEBKIT_KINDS.flatMap((kind) => [`memory_webkit_${kind.toLowerCase()}_mb`, `memory_webkit_${kind.toLowerCase()}_idle_mb`])]);
/** Metrics a run may lack: the launch is only timed when the spec's first look comes before
 * Home is usable; `launch_webview_ms` and `reload_home_ms` always time the launch path. */
const OPTIONAL = new Set(["launch_ms"]);
/** One metric of each row (T1-T6, U1) a verdict must judge; any other metric a run reads must
 * be judged too, unless it is optional. */
export const REQUIRED = ["launch_webview_ms", "reload_home_ms", "engine_start_ms", "big_open_ms", "big_conversation_ms",
  "big_sessions_ms", "switch_cold_home_ms", "switch_warm_home_ms", "round_trip_desktop_open_project_ms",
  "memory_engine_mb", "memory_app_mb", "tui_launch_ms", "tui_open_project_ms"];
const KEPT_MAX = 32;

/** Flattens one speed run's records into one value per metric. Inputs: run.mjs's
 * speed-engine.json and the spec's speed-app.json. Output: `{ metric: value }`; a launch the
 * first look already found usable is left out, as its time is only an upper bound. */
export function sampleMetrics(engine, app) {
  assert.ok(Number.isFinite(engine.engine_start_ms), "an engine start time");
  assert.ok(app.launch && app.big && app.switch_cold && app.switch_warm && app.memory, "a complete app record");
  assert.ok(Number.isFinite(app.launch.webview_ms) && Number.isFinite(app.launch.reload_ms), "a webview start and a reload");
  const metrics = {};
  if (!app.launch.first_look_usable) metrics.launch_ms = app.launch.ms;
  metrics.launch_webview_ms = app.launch.webview_ms;
  metrics.reload_home_ms = app.launch.reload_ms;
  metrics.engine_start_ms = engine.engine_start_ms;
  if (engine.onboard_start_ms !== undefined && engine.onboard_start_ms !== null) metrics.onboard_start_ms = engine.onboard_start_ms;
  for (const [name, ms] of Object.entries(app.big)) metrics[`big_${name}`] = ms;
  for (const [page, ms] of Object.entries(app.switch_cold)) metrics[`switch_cold_${page}_ms`] = ms;
  for (const [page, ms] of Object.entries(app.switch_warm)) metrics[`switch_warm_${page}_ms`] = ms;
  for (const [cmd, times] of Object.entries(engine.round_trip_ms)) metrics[`round_trip_${cmd}_ms`] = summary(times).median;
  metrics.memory_engine_mb = app.memory.engine_mb;
  metrics.memory_app_mb = app.memory.app_mb;
  const { webkit_mb: webkit, webkit_kinds: kinds, idle } = app.memory;
  assert.ok(Array.isArray(kinds) && kinds.length === webkit.length, "a kind for each WebKit helper");
  assert.ok(idle && Array.isArray(idle.webkit_mb) && idle.webkit_mb.length === webkit.length, "an idle reading for each WebKit helper");
  metrics.memory_webkit_mb = round(webkit.reduce((total, mb) => total + mb, 0));
  metrics.memory_app_idle_mb = idle.app_mb;
  metrics.memory_webkit_idle_mb = round(idle.webkit_mb.reduce((total, mb) => total + mb, 0));
  for (const [index, kind] of kinds.entries()) {
    assert.ok(WEBKIT_KINDS.includes(kind), `a WebKit helper kind: ${kind}`);
    const name = `memory_webkit_${kind.toLowerCase()}`;
    metrics[`${name}_mb`] = round((metrics[`${name}_mb`] ?? 0) + webkit[index]);
    metrics[`${name}_idle_mb`] = round((metrics[`${name}_idle_mb`] ?? 0) + idle.webkit_mb[index]);
  }
  for (const [metric, value] of Object.entries(metrics)) assert.ok(Number.isFinite(value) && value >= 0, `${metric} is a reading: ${value}`);
  return metrics;
}

/** The run order: one untimed warm-up launch per side (the first exec of a new binary waits in
 * syspolicyd), then the sides alternate so drift on the Mac lands on both. Input: kept runs per
 * side. Output: `[{ side, warm }]`. */
export function schedule(kept) {
  assert.ok(Number.isInteger(kept) && kept >= 1 && kept <= KEPT_MAX, `1 to ${KEPT_MAX} kept runs per side`);
  const order = [{ side: "base", warm: true }, { side: "candidate", warm: true }];
  for (let index = 0; index < kept; index += 1) order.push({ side: "base", warm: false }, { side: "candidate", warm: false });
  assert.equal(order.length, 2 * kept + 2);
  return order;
}

/** Runs every scheduled launch (`schedule`), each only once the Mac is quiet and kept only when
 * the Mac stayed quiet through it; a run it got busy through, failed or not, runs again, and
 * `TAINTED_MAX` in a row refuse. A launch that fails on a quiet Mac is recorded with its error,
 * counted for its side and not run again, and the schedule goes on, so the medians come from the
 * good runs; once a side can no longer reach `KEPT_MIN` good runs the schedule ends, as no
 * verdict could come. Every record keeps the Mac reading after its run. Inputs: kept
 * runs per side, `{ quietBefore, launch, quietAfter }` (null or the busy reasons; `{ run,
 * sample }` for a side and whether it is a warm-up; `{ quiet, reasons }`) and the progress it
 * fills: `{ samples, failed, runs }`. Output: null, or the refusal's reasons. */
export async function runSchedule(kept, { quietBefore, launch, quietAfter }, progress) {
  assert.ok(Number.isInteger(kept) && kept >= KEPT_MIN, `at least ${KEPT_MIN} kept runs per side`);
  const { samples, failed, runs } = progress;
  const order = schedule(kept);
  for (const [at, { side, warm }] of order.entries()) {
    for (let tainted = 0; ; tainted += 1) {
      if (tainted >= TAINTED_MAX) return [`${TAINTED_MAX} runs in a row saw the Mac get busy`];
      const busy = await quietBefore();
      if (busy) return busy;
      let done;
      try {
        done = await launch(side, warm);
      } catch (error) {
        const after = quietAfter();
        runs.push({ side, warm, failed: failureText(error), quiet_after: after.quiet, reasons: after.reasons });
        if (!after.quiet) continue;
        failed[side] += 1;
        break;
      }
      const after = quietAfter();
      runs.push({ side, warm, ...done.run, quiet_after: after.quiet, reasons: after.reasons });
      if (!after.quiet) continue;
      if (!warm) samples[side].push(done.sample);
      break;
    }
    const left = order.slice(at + 1).filter((slot) => slot.side === side && !slot.warm).length;
    if (samples[side].length + left < KEPT_MIN) return null;
  }
  for (const side of ["base", "candidate"]) {
    assert.ok(samples[side].length <= kept && samples[side].length + failed[side] <= kept + 1, `${side}: one result a slot`);
  }
  return null;
}

/** The text a failed run's record keeps: the error's message, its last `FAILURE_TEXT_MAX`
 * characters. Input: what the launch threw. Output: the text. */
function failureText(error) {
  const text = error instanceof Error ? error.message : String(error);
  return text.slice(-FAILURE_TEXT_MAX);
}

/** Runs the keychain tool for a speed state home with only that HOME and the system PATH, as the
 * gate's kc-state.sh did, so it reads and writes that home's keychain settings. Inputs: the home
 * and the tool's arguments. Output: what the tool printed. */
export function securityIn(home, args) {
  return execFileSync(SECURITY, args, {
    cwd: home, env: { HOME: home, PATH: "/usr/bin:/bin" }, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: SECURITY_MS_MAX,
  });
}

/** The paths a keychain tool listing names, one a line, without its indent and quotes. Input:
 * the listing. Output: the paths. */
function listedPaths(text) {
  return text.split("\n").map((line) => line.trim()).filter((line) => line !== "").map((line) => line.replace(/^"(.*)"$/, "$1"));
}

/** Checks the owner has no keychain settings file, as the gate's kc-state.sh did before and
 * after each step: a keychain tool that wrote the owner's settings instead of the home's would
 * leave one. Input: the owner's home. Output: none. */
function ownerSettingsAbsent(realHome) {
  const settings = join(realHome, "Library", "Preferences", "com.apple.security.plist");
  assert.ok(!existsSync(settings), `the owner's keychain settings file stays absent: ${settings}`);
}

/** The owner's own keychain view: the search list and the default keychain the tool prints for
 * the owner's home. Inputs: the owner's home and the keychain tool. Output: `{ list, chosen }`. */
function ownerKeychains(realHome, security) {
  return {
    list: listedPaths(security(realHome, ["list-keychains", "-d", "user"])),
    chosen: listedPaths(security(realHome, ["default-keychain", "-d", "user"])),
  };
}

/** Gives a new speed state home its own empty login keychain as its default and its only
 * search-list entry, as the gate's kc-state.sh prep did by hand (DEC-146), so an app and a TUI
 * run on that home never reach the owner's keychain. Refuses a folder that is not a new speed
 * state home, stops while the owner has a keychain settings file, and reads the owner's search
 * list and default keychain before and after, stopping when either changed. Inputs: the home,
 * the owner's home and the keychain tool. Output: the keychain's path. */
export function prepareKeychain(home, realHome = userInfo().homedir, security = securityIn) {
  assert.ok(basename(home) === "home" && SPEED_STATE.test(dirname(home)), `a speed state home: ${home}`);
  privateFolder(home);
  ownerSettingsAbsent(realHome);
  assert.ok(!existsSync(join(home, "Library")), `a new speed state home holds no Library yet: ${home}`);
  const owner = ownerKeychains(realHome, security);
  for (const folder of ["Library", "Library/Keychains", "Library/Preferences"]) mkdirSync(join(home, folder), { mode: 0o700 });
  const keychain = join(home, "Library", "Keychains", "login.keychain-db");
  security(home, ["create-keychain", "-p", "", keychain]);
  security(home, ["set-keychain-settings", keychain]);
  security(home, ["list-keychains", "-d", "user", "-s", keychain]);
  security(home, ["default-keychain", "-d", "user", "-s", keychain]);
  assert.deepEqual(listedPaths(security(home, ["list-keychains", "-d", "user"])), [keychain], "the home's search list holds only its own keychain");
  assert.deepEqual(listedPaths(security(home, ["default-keychain", "-d", "user"])), [keychain], "the home's default keychain is its own");
  ownerSettingsAbsent(realHome);
  const after = ownerKeychains(realHome, security);
  assert.deepEqual(after.list, owner.list, "the owner's search list is unchanged");
  assert.deepEqual(after.chosen, owner.chosen, "the owner's default keychain is unchanged");
  return keychain;
}

/** The values one metric has across a side's samples. Inputs: the samples and the metric.
 * Output: the values, in run order. */
function valuesOf(samples, metric) {
  return samples.filter((sample) => Object.hasOwn(sample, metric)).map((sample) => sample[metric]);
}

/** Self-noise per metric: one build's two runs of samples against each other. Inputs: the two
 * sample lists. Output: `{ metric: share }` for each metric both measured at least
 * `KEPT_MIN` times with a median above zero; a zero median has no share, so judge leaves
 * only that metric unmeasured. */
export function noiseShares(first, second) {
  assert.ok(first.length >= 1 && second.length >= 1, "two sample lists");
  const shares = {};
  for (const metric of new Set([...first, ...second].flatMap((sample) => Object.keys(sample)))) {
    const left = valuesOf(first, metric);
    const right = valuesOf(second, metric);
    if (Math.min(left.length, right.length) < KEPT_MIN) continue;
    const sides = [summary(left), summary(right)];
    if (Math.min(sides[0].median, sides[1].median) > 0) shares[metric] = noise(sides[0], sides[1]);
  }
  assert.ok(Object.values(shares).every((share) => share >= 0));
  return shares;
}

/** Judges every metric of the candidate against the base (DEC-169 section 3). Inputs: both
 * sides' samples and the self-noise shares. Output: one row per metric, sorted: status pass,
 * fail, report (report-only) or unmeasured (fewer than `KEPT_MIN` valid samples a side, or no
 * noise share), with both summaries and the reasons. */
export function judge(base, candidate, shares) {
  assert.ok(Array.isArray(base) && Array.isArray(candidate), "two sample lists");
  const metrics = [...new Set([...base, ...candidate].flatMap((sample) => Object.keys(sample)))].sort();
  const rows = metrics.map((metric) => {
    const kept = Math.min(valuesOf(base, metric).length, valuesOf(candidate, metric).length);
    if (kept < KEPT_MIN) return { metric, status: "unmeasured", kept };
    const sides = { base: summary(valuesOf(base, metric).slice(0, kept)), candidate: summary(valuesOf(candidate, metric).slice(0, kept)) };
    if (REPORT_ONLY.has(metric)) return { metric, status: "report", ...sides };
    if (!Object.hasOwn(shares, metric)) return { metric, status: "unmeasured", kept, reasons: ["the noise run never measured it"] };
    const share = margin(shares[metric]);
    const floor = floorFor(metric);
    const result = verdict({ ...sides, margin: share, floor });
    return { metric, status: result.pass ? "pass" : "fail", margin: share, floor, ...sides, reasons: result.reasons };
  });
  assert.equal(rows.length, metrics.length);
  return rows;
}

/** The metrics a verdict could not judge: a required one missing, or any one unmeasured that
 * is not optional. A verdict with any is not a pass. Input: judge's rows. Output: the metrics. */
export function incomplete(rows) {
  assert.ok(Array.isArray(rows), "judge's rows");
  const status = new Map(rows.map((row) => [row.metric, row.status]));
  assert.equal(status.size, rows.length, "one row per metric");
  const missing = REQUIRED.filter((metric) => !status.has(metric));
  const unmeasured = rows.filter((row) => row.status === "unmeasured" && !OPTIONAL.has(row.metric)).map((row) => row.metric);
  return [...missing, ...unmeasured];
}

/** The metrics timed from a process start, which take the launch floor. */
const LAUNCHES = ["launch_ms", "launch_webview_ms", "reload_home_ms", "engine_start_ms", "tui_launch_ms"];

/** Joins one TUI run's times (row U1, tui_speed.py) to the same slot's app sample. Inputs: the
 * app sample and the TUI's `{ tui_<step>_ms: ms }`. Output: one sample holding both. */
export function joinSample(sample, tui) {
  for (const [metric, value] of Object.entries(tui)) {
    assert.match(metric, /^tui_[a-z_]+_ms$/, `a TUI time is named tui_<step>_ms: ${metric}`);
    assert.ok(Number.isFinite(value) && value >= 0, `${metric} is a reading: ${value}`);
    assert.ok(!Object.hasOwn(sample, metric), `${metric} is read once a run`);
  }
  const joined = { ...sample, ...tui };
  assert.equal(Object.keys(joined).length, Object.keys(sample).length + Object.keys(tui).length);
  return joined;
}

/** The exit of a compare run: 1 when a metric failed or could not be judged, or when the
 * candidate failed more runs than the base, since a failure only the candidate has is a finding
 * and one both sides met is the Mac's; else 0. Inputs: judge's rows, `incomplete`'s metrics and
 * the failed runs per side. Output: the exit code. */
export function compareExit(rows, gaps, failed) {
  assert.ok(Number.isInteger(failed.base) && Number.isInteger(failed.candidate), "failed runs are counted per side");
  return rows.some((row) => row.status === "fail") || gaps.length > 0 || failed.candidate > failed.base ? 1 : 0;
}

/** The floor a metric's gap must pass to fail. Input: the metric. Output: the floor. */
function floorFor(metric) {
  assert.match(metric, /_(ms|mb)$/, `a metric with a unit: ${metric}`);
  if (metric.startsWith("memory_")) return FLOOR.memory_mb;
  if (LAUNCHES.includes(metric)) return FLOOR.launch_ms;
  if (metric.startsWith("round_trip_")) return FLOOR.round_trip_ms;
  return FLOOR.page_ms;
}

/** Checks that a speed state folder is this user's own: a real folder, not a link, owned by
 * this user, that no group or other user can write, so no other local account can plant an
 * engine's settings or a seeded record in it. Input: the absolute folder. Output: the folder. */
export function privateFolder(folder) {
  assert.ok(typeof folder === "string" && isAbsolute(folder), `an absolute folder: ${folder}`);
  const info = lstatSync(folder);
  assert.ok(info.isDirectory() && !info.isSymbolicLink(), `${folder} is a real folder, not a link`);
  assert.equal(info.uid, process.getuid(), `${folder} is this user's own`);
  assert.equal(info.mode & 0o022, 0, `${folder}: no group or other user can write it`);
  return folder;
}

/** Runs in the page: clicks an element and times, with the page's clock, until the page at its
 * address is usable: the clicked link or button marked current, selected or expanded, a heading
 * in main, no skeleton left in main, and `want` elements matching `extra` when given. A
 * MutationObserver and a 4 ms check both look. Inputs: the selector, the extra condition, the
 * count, the limit in ms and WebDriver's callback. Output: `done` with the ms, or an error
 * at once for a missing or disabled target, which a click could not reach. */
export function clickUntilUsable(selector, extra, want, limitMs, done) {
  const target = document.querySelector(selector);
  if (!target) {
    done({ error: `nothing matches ${selector}` });
    return;
  }
  if (target.disabled) {
    done({ error: `${selector} is disabled, so a click would do nothing` });
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
}
