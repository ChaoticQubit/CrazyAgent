import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

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

const round = (value) => Number(value.toFixed(2));

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
 * candidate calls that the base never did, or more than `COUNT_SLACK` times more often, sorted
 * by scenario and command. */
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
      if ((was === 0 && now > 0) || now > was + COUNT_SLACK) findings.push({ scenario, cmd, base: was, candidate: now });
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
function output(program, args) {
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

/** WebKit helper processes run under launchd, not under the app, and their command lines name
 * no client; the speed rows find the ones the app started as the pids new since its launch.
 * Input: none. Output: the pids of every WebKit WebContent, Networking and GPU process. */
export function webkitHelpers() {
  const listed = output("/bin/ps", ["-axo", "pid=,comm="]).split("\n");
  const pids = listed.flatMap((line) => {
    const found = line.trim().match(/^(\d+)\s+(?:.*\/)?com\.apple\.WebKit\.(WebContent|Networking|GPU)$/);
    return found ? [Number(found[1])] : [];
  });
  assert.ok(pids.every((pid) => Number.isInteger(pid) && pid > 1));
  return pids;
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
export const REPORT_ONLY = new Set(["memory_webkit_mb"]);
const KEPT_MAX = 32;

/** Flattens one speed run's records into one value per metric. Inputs: run.mjs's
 * speed-engine.json and the spec's speed-app.json. Output: `{ metric: value }`; a launch the
 * first look already found usable is left out, as its time is only an upper bound. */
export function sampleMetrics(engine, app) {
  assert.ok(Number.isFinite(engine.engine_start_ms), "an engine start time");
  assert.ok(app.launch && app.big && app.switch_cold && app.switch_warm && app.memory, "a complete app record");
  const metrics = {};
  if (!app.launch.first_look_usable) metrics.launch_ms = app.launch.ms;
  metrics.engine_start_ms = engine.engine_start_ms;
  for (const [name, ms] of Object.entries(app.big)) metrics[`big_${name}`] = ms;
  for (const [page, ms] of Object.entries(app.switch_cold)) metrics[`switch_cold_${page}_ms`] = ms;
  for (const [page, ms] of Object.entries(app.switch_warm)) metrics[`switch_warm_${page}_ms`] = ms;
  for (const [cmd, times] of Object.entries(engine.round_trip_ms)) metrics[`round_trip_${cmd}_ms`] = summary(times).median;
  metrics.memory_engine_mb = app.memory.engine_mb;
  metrics.memory_app_mb = app.memory.app_mb;
  metrics.memory_webkit_mb = round(app.memory.webkit_mb.reduce((total, mb) => total + mb, 0));
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

/** The values one metric has across a side's samples. Inputs: the samples and the metric.
 * Output: the values, in run order. */
function valuesOf(samples, metric) {
  return samples.filter((sample) => Object.hasOwn(sample, metric)).map((sample) => sample[metric]);
}

/** Self-noise per metric: one build's two runs of samples against each other. Inputs: the two
 * sample lists. Output: `{ metric: share }` for each metric both measured at least
 * `KEPT_MIN` times. */
export function noiseShares(first, second) {
  assert.ok(first.length >= 1 && second.length >= 1, "two sample lists");
  const shares = {};
  for (const metric of new Set([...first, ...second].flatMap((sample) => Object.keys(sample)))) {
    const left = valuesOf(first, metric);
    const right = valuesOf(second, metric);
    if (Math.min(left.length, right.length) >= KEPT_MIN) shares[metric] = noise(summary(left), summary(right));
  }
  assert.ok(Object.values(shares).every((share) => share >= 0));
  return shares;
}

/** Judges every metric of the candidate against the base (DEC-169 section 3). Inputs: both
 * sides' samples and the self-noise shares. Output: one row per metric, sorted: status pass,
 * fail, report (report-only) or unmeasured (fewer than `KEPT_MIN` valid samples a side), with
 * both summaries and the reasons. */
export function judge(base, candidate, shares) {
  assert.ok(Array.isArray(base) && Array.isArray(candidate), "two sample lists");
  const metrics = [...new Set([...base, ...candidate].flatMap((sample) => Object.keys(sample)))].sort();
  const rows = metrics.map((metric) => {
    const kept = Math.min(valuesOf(base, metric).length, valuesOf(candidate, metric).length);
    if (kept < KEPT_MIN) return { metric, status: "unmeasured", kept };
    const sides = { base: summary(valuesOf(base, metric).slice(0, kept)), candidate: summary(valuesOf(candidate, metric).slice(0, kept)) };
    if (REPORT_ONLY.has(metric)) return { metric, status: "report", ...sides };
    assert.ok(Object.hasOwn(shares, metric), `a noise for ${metric}; run the noise mode first`);
    const share = margin(shares[metric]);
    const floor = floorFor(metric);
    const result = verdict({ ...sides, margin: share, floor });
    return { metric, status: result.pass ? "pass" : "fail", margin: share, floor, ...sides, reasons: result.reasons };
  });
  assert.equal(rows.length, metrics.length);
  return rows;
}

/** The metrics timed from a process start, which take the launch floor. */
const LAUNCHES = ["launch_ms", "engine_start_ms", "tui_launch_ms"];

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

/** The floor a metric's gap must pass to fail. Input: the metric. Output: the floor. */
function floorFor(metric) {
  assert.match(metric, /_(ms|mb)$/, `a metric with a unit: ${metric}`);
  if (metric.startsWith("memory_")) return FLOOR.memory_mb;
  if (LAUNCHES.includes(metric)) return FLOOR.launch_ms;
  if (metric.startsWith("round_trip_")) return FLOOR.round_trip_ms;
  return FLOOR.page_ms;
}
