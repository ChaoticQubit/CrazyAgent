import assert from "node:assert/strict";

/** The switch time the owner asked for, in ms (DEC-187). A row past it still passes. */
export const TARGET_MS = 30;

/** The longest switch time the owner accepts, in ms (DEC-187). A row past it fails. */
export const LIMIT_MS = 50;

/** How many timed switches one row takes, after its warm-up switch. */
export const SWITCHES = 20;

/** Most samples one report takes. */
export const SAMPLES_MAX = 200;

/** The share of a row's switches that must be within the limit: with 20 switches the second
 * worst decides, so one switch a busy Mac delayed does not fail a row and two do. */
export const GATE_SHARE = 0.95;

const round = (value) => Number(value.toFixed(1));

/** The nearest-rank percentile of sorted numbers. Inputs: 1 or more numbers in rising order
 * and a share above 0, up to 1. Output: the smallest number with at least that share of the
 * numbers at or under it. */
export function rank(sorted, share) {
  assert.ok(Array.isArray(sorted) && sorted.length >= 1, "a rank needs at least one number");
  assert.ok(share > 0 && share <= 1, `a share is above 0 and at most 1: ${share}`);
  for (let index = 1; index < sorted.length; index += 1) assert.ok(sorted[index - 1] <= sorted[index], "the numbers rise");
  const found = sorted[Math.ceil(share * sorted.length) - 1];
  assert.ok(sorted[0] <= found && found <= sorted.at(-1), "the rank lies inside the numbers");
  return found;
}

/** One switch's sample from stamps two pages took on the Mac's one clock, for a target that
 * lives in another page than the click (the IDE webview). Inputs: the click's stamp and the
 * stamps of the frames that showed the target, both in ms since 1970. Output: `{ ms }`, the
 * time from the click to the first show at or after it, or null when none followed. */
export function stampSample(clickAt, shows) {
  assert.ok(Number.isFinite(clickAt) && clickAt > 0, `a click stamp is a time: ${clickAt}`);
  assert.ok(Array.isArray(shows) && shows.length <= SAMPLES_MAX, "show stamps are a bounded list");
  for (const show of shows) assert.ok(Number.isFinite(show) && show > 0, `a show stamp is a time: ${show}`);
  const after = shows.filter((show) => show >= clickAt);
  const ms = after.length === 0 ? null : Math.min(...after) - clickAt;
  assert.ok(ms === null || ms >= 0, "a show counted for a click is not before it");
  return { ms };
}

/** Whether a native view is on screen by one probe read (DEC-203): not hidden, a frame
 * presented since its uncover, and its rows hold `text`. Inputs: the read (`hidden`, `rows`,
 * `uncovered_ms`, `presented_ms`) and the text. Output: the answer. */
export function nativeShown(seen, text) {
  assert.equal(typeof seen?.hidden, "boolean", `a native read says whether the view is hidden: ${JSON.stringify(seen)}`);
  assert.ok(Array.isArray(seen.rows), "a native read has the view's rows");
  assert.match(text ?? "", /\S/, "a native target names its text");
  assert.ok(!seen.hidden || (seen.uncovered_ms === null && seen.presented_ms === null), "a hidden view keeps no stamps");
  if (seen.hidden || seen.presented_ms === null) return false;
  assert.ok(Number.isFinite(seen.uncovered_ms) && Number.isFinite(seen.presented_ms), "a presented view has both its stamps");
  assert.ok(seen.presented_ms >= seen.uncovered_ms, "a view presents only after its uncover");
  return seen.rows.join("\n").includes(text);
}

/** When a read's last cover(false) command started, from the click: null when none started
 * since it, as an earlier one was another switch's. Inputs: the click's stamp and the read.
 * Output: the ms, or null. */
function askedSince(clickAt, seen) {
  assert.ok(seen.asked_ms === null || (Number.isFinite(seen.asked_ms) && seen.asked_ms > 0), `a cover entry stamp is a time or null: ${seen.asked_ms}`);
  return seen.asked_ms === null || seen.asked_ms < clickAt ? null : seen.asked_ms - clickAt;
}

/** The draws a read's view skipped since its uncover and the first one's reason. Input: the
 * read. Output: `{ skipped, skip_first }`. */
function skipsOf(seen) {
  assert.ok(Number.isInteger(seen.skipped) && seen.skipped >= 0, `a read counts its skipped draws: ${seen.skipped}`);
  assert.equal(seen.skip_first !== null, seen.skipped > 0, "a read names its first skip exactly when it skipped a draw");
  return { skipped: seen.skipped, skip_first: seen.skip_first };
}

/** One native switch from the click's stamp and one probe read, both in ms since 1970 on the
 * Mac's one clock: once the view is on screen (`nativeShown`), `ms` runs from the click to
 * the first frame it presented, `uncovered_ms` to its uncover (the page and bridge share) and
 * `asked_ms` to the start of its cover(false) command (the page's share), with the draws
 * skipped since the uncover. Inputs: the click's stamp, the read and the text. Output:
 * `{ ms, uncovered_ms, asked_ms, skipped, skip_first }`, or null while it is not on screen;
 * throws for a view shown before the click. */
export function nativeSample(clickAt, seen, text) {
  assert.ok(Number.isFinite(clickAt) && clickAt > 0, `a click stamp is a time: ${clickAt}`);
  if (!nativeShown(seen, text)) return null;
  assert.ok(seen.uncovered_ms >= clickAt, `the view was shown before the click: uncovered ${clickAt - seen.uncovered_ms} ms before it`);
  const { ms } = stampSample(clickAt, [seen.presented_ms]);
  assert.ok(ms !== null && ms >= seen.uncovered_ms - clickAt);
  return { ms, uncovered_ms: seen.uncovered_ms - clickAt, asked_ms: askedSince(clickAt, seen), ...skipsOf(seen) };
}

/** The last read of a switch the view never showed in, kept so the run says why: whether the
 * view was hidden, its stamps and its cover(false) command's start from the click, its draws
 * skipped since the uncover and whether its rows held the marker. Inputs: the click's stamp,
 * the read and the text. Output: `{ hidden, uncovered_ms, presented_ms, asked_ms, marker,
 * skipped, skip_first }`. */
export function nativeLook(clickAt, seen, text) {
  assert.ok(Number.isFinite(clickAt) && clickAt > 0, `a click stamp is a time: ${clickAt}`);
  assert.equal(typeof seen?.hidden, "boolean", `a native read says whether the view is hidden: ${JSON.stringify(seen)}`);
  assert.ok(Array.isArray(seen.rows), "a native read has the view's rows");
  assert.match(text ?? "", /\S/, "a native target names its text");
  const from = (stamp) => (stamp === null ? null : stamp - clickAt);
  return {
    hidden: seen.hidden,
    uncovered_ms: from(seen.uncovered_ms),
    presented_ms: from(seen.presented_ms),
    asked_ms: askedSince(clickAt, seen),
    marker: seen.rows.join("\n").includes(text),
    ...skipsOf(seen),
  };
}

/** The page commands the host answered busy, and the covers it told refused, between two
 * reads of one switch: the read before the click and the last one. Inputs: both reads.
 * Output: `{ busy, refused }`. */
export function nativeCounts(before, after) {
  for (const read of [before, after]) {
    for (const count of ["busy_answers", "cover_refused"]) {
      assert.ok(Number.isInteger(read?.[count]) && read[count] >= 0, `a native read has its ${count} count: ${read?.[count]}`);
    }
  }
  assert.ok(after.busy_answers >= before.busy_answers && after.cover_refused >= before.cover_refused, "the host's counts never fall");
  return { busy: after.busy_answers - before.busy_answers, refused: after.cover_refused - before.cover_refused };
}

/** How far the host's clock at a read falls outside the page's stamps just before and after
 * it, in ms: 0 when the two clocks agree within the read's round trip. Inputs: the page's
 * stamp before, the host's and the page's after. Output: the skew. */
export function clockSkew(before, host, after) {
  assert.ok(Number.isFinite(before) && Number.isFinite(after) && before <= after, `the page stamps in order: ${before}, ${after}`);
  assert.ok(Number.isFinite(host), `a host stamp is a time: ${host}`);
  const skew = Math.max(0, before - host, host - after);
  assert.ok(skew >= 0);
  return skew;
}

/** Judges one row's switches (DEC-187). Input: 1 to `SAMPLES_MAX` samples `{ ms }`, the time
 * from the click to the target on screen, null for a switch that never showed it, which
 * counts as past every limit. Output: the count, p50, p95 and max by nearest rank (null
 * when that switch never showed), how many passed the target, the limit and never showed,
 * and `pass`: the p95 is at or under `LIMIT_MS`. */
export function report(samples) {
  assert.ok(Array.isArray(samples) && samples.length >= 1 && samples.length <= SAMPLES_MAX, `a report takes 1 to ${SAMPLES_MAX} samples`);
  for (const sample of samples) {
    assert.ok(sample.ms === null || (Number.isFinite(sample.ms) && sample.ms >= 0), `a sample is a time or null: ${sample.ms}`);
  }
  const times = samples.map((sample) => sample.ms ?? Infinity).sort((left, right) => left - right);
  const told = (value) => (value === Infinity ? null : round(value));
  const p95 = rank(times, GATE_SHARE);
  const result = {
    n: times.length,
    p50_ms: told(rank(times, 0.5)),
    p95_ms: told(p95),
    max_ms: told(times.at(-1)),
    over_target: times.filter((time) => time > TARGET_MS).length,
    over_limit: times.filter((time) => time > LIMIT_MS).length,
    never_shown: times.filter((time) => time === Infinity).length,
    pass: p95 <= LIMIT_MS,
  };
  assert.ok(result.never_shown <= result.over_limit && result.over_limit <= result.over_target && result.over_target <= result.n, "the counts nest");
  assert.ok(!result.pass || result.p95_ms !== null, "a passing row showed its p95 switch");
  return result;
}

/** One row's result as the line a run prints and a status line can carry. Inputs: the row's
 * name and its report. Output: the line. */
export function line(name, result) {
  assert.match(name, /\S/, "a row has a name");
  const ms = (value) => (value === null ? "never shown" : `${value} ms`);
  const target = result.p95_ms !== null && result.p95_ms <= TARGET_MS ? "meets" : "misses";
  return `${name}: p50 ${ms(result.p50_ms)}, p95 ${ms(result.p95_ms)}, max ${ms(result.max_ms)}; over ${TARGET_MS} ms: ${result.over_target} of ${result.n}, `
    + `over ${LIMIT_MS} ms: ${result.over_limit} of ${result.n}, never shown: ${result.never_shown} -> ${result.pass ? "PASS" : "FAIL"} `
    + `(fails when the p95 is over ${LIMIT_MS} ms; the p95 ${target} the ${TARGET_MS} ms target)`;
}
