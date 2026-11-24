import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./run.sh", import.meta.url));

/**
 * One run's header line. Inputs: the build, the condition and load1. Output: the line.
 */
function header(build, condition, load1, order = "") {
  const named = order ? ` order=${order}` : "";
  return `PERF-RUN build=${build} commit=${build === "2.5" ? "aaa" : "bbb"} date=2026-11-23T22:00 condition=${condition} load1=${load1} slots=0 runs=1${named}`;
}

/**
 * Runs the median step on a log of interleaved runs. Inputs: the log lines and the run
 * count. Output: the printed lines.
 */
function median(lines, runs) {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const log = path.join(folder, "runs.log");
    writeFileSync(log, `${lines.join("\n")}\n`);
    return execFileSync("bash", [script, "--median", log, String(runs)], { encoding: "utf8" }).trim().split("\n");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
}

test("three interleaved runs print one header per build and the median of each row", () => {
  const lines = [];
  const p50s = [[30, 12_000], [10, 9_000], [20, 15_000]];
  p50s.forEach(([fast, slow], index) => {
    lines.push(header("2.5", "quiet", `${index + 1}.00`));
    lines.push(`PERF 1 common names.is_snake p50_ns=${fast} p95_ns=${fast + 1} n=200 batch=1000`);
    lines.push(slow < 10_000 ? `PERF 1 common hash p50_ns=${slow} p95_ns=${slow} n=200 batch=4` : `PERF 1 common hash p50=${slow / 1000} p95=${slow / 1000} n=200 batch=4`);
    lines.push(header("2.4.4", index === 2 ? "loaded" : "quiet", "3.50"));
    lines.push(`PERF 1 common names.single_component p50_ns=${fast * 2} p95_ns=${fast * 2} n=200 batch=1000`);
  });
  assert.deepEqual(median(lines, 3), [
    "PERF-RUN build=2.5 commit=aaa date=2026-11-23T22:00 condition=quiet load1=2.00 slots=0 runs=3",
    "PERF 1 common names.is_snake p50_ns=20 p95_ns=21 n=200 batch=1000",
    "PERF 1 common hash p50=12 p95=12 n=200 batch=4",
    "PERF-RUN build=2.4.4 commit=bbb date=2026-11-23T22:00 condition=loaded load1=3.50 slots=0 runs=3",
    "PERF 1 common names.single_component p50_ns=40 p95_ns=40 n=200 batch=1000",
  ]);
});

test("a row missing from one run prints nothing", () => {
  const lines = [];
  for (let index = 0; index < 3; index += 1) {
    lines.push(header("2.5", "loaded", "9.00"));
    lines.push("PERF 1 common names.is_snake p50_ns=5 p95_ns=6 n=200 batch=1000");
    if (index !== 1) lines.push("PERF 1 common hash p50_ns=120 p95_ns=125 n=200 batch=100");
  }
  assert.deepEqual(median(lines, 3), [
    "PERF-RUN build=2.5 commit=aaa date=2026-11-23T22:00 condition=loaded load1=9.00 slots=0 runs=3",
    "PERF 1 common names.is_snake p50_ns=5 p95_ns=6 n=200 batch=1000",
  ]);
});

test("a build with fewer runs than asked fails", () => {
  const lines = [header("2.5", "quiet", "1.00"), "PERF 1 common names.is_snake p50_ns=5 p95_ns=6 n=200 batch=1000"];
  assert.throws(() => median(lines, 3));
});

test("two ABBA blocks print each build's median of its four runs and name the order", () => {
  const lines = [];
  const runs = [
    ["2.5", 30, "2.00"],
    ["2.4.4", 40, "3.00"],
    ["2.4.4", 50, "3.00"],
    ["2.5", 35, "4.00"],
    ["2.5", 90, "5.00"],
    ["2.4.4", 41, "6.00"],
    ["2.4.4", 70, "6.00"],
    ["2.5", 31, "7.00"],
  ];
  for (const [build, p50, load1] of runs) {
    lines.push(header(build, "loaded", load1, "ABBA"));
    lines.push(`PERF 1 common names.is_snake p50_ns=${p50} p95_ns=${p50 + 4} n=200 batch=1000`);
  }
  assert.deepEqual(median(lines, 4), [
    "PERF-RUN build=2.5 commit=aaa date=2026-11-23T22:00 condition=loaded load1=4.50 slots=0 runs=4 order=ABBA",
    "PERF 1 common names.is_snake p50_ns=33 p95_ns=37 n=200 batch=1000",
    "PERF-RUN build=2.4.4 commit=bbb date=2026-11-23T22:00 condition=loaded load1=4.50 slots=0 runs=4 order=ABBA",
    "PERF 1 common names.is_snake p50_ns=45 p95_ns=49 n=200 batch=1000",
  ]);
});
