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
