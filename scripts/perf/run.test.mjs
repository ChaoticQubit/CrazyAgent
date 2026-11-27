import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

test("a row whose runs all print nanoseconds keeps nanoseconds in its median at any size", () => {
  const lines = [];
  const runs = [
    ["2.5", 50375, 57875],
    ["2.4.4", 4875, 5000],
    ["2.4.4", 6208, 6333],
    ["2.5", 59708, 67958],
  ];
  for (const [build, p50, p95] of runs) {
    lines.push(header(build, "loaded", "2.00", "2.5,2.4.4,2.4.4,2.5"));
    lines.push(`PERF 1 sdk provider.settings.save_quit p50_ns=${p50} p95_ns=${p95} n=200 batch=1`);
    lines.push(`PERF 1 sdk provider.sandbox.save_settings p50=${p50 * 3} p95=${p95 * 3} n=200`);
  }
  assert.deepEqual(median(lines, 2), [
    "PERF-RUN build=2.5 commit=aaa date=2026-11-23T22:00 condition=loaded load1=2.00 slots=0 runs=2 order=2.5,2.4.4,2.4.4,2.5",
    "PERF 1 sdk provider.settings.save_quit p50_ns=55041 p95_ns=62916 n=200 batch=1",
    "PERF 1 sdk provider.sandbox.save_settings p50=165124 p95=188749 n=200",
    "PERF-RUN build=2.4.4 commit=bbb date=2026-11-23T22:00 condition=loaded load1=2.00 slots=0 runs=2 order=2.5,2.4.4,2.4.4,2.5",
    "PERF 1 sdk provider.settings.save_quit p50_ns=5541 p95_ns=5666 n=200 batch=1",
    "PERF 1 sdk provider.sandbox.save_settings p50=16624 p95=16999 n=200",
  ]);
});

test("a caller that still sets PERF_RUNS is refused before any run", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const env = { ...process.env, PERF_RUNS: "3", PERF_SPRINT: path.join(folder, "missing") };
    const result = spawnSync("bash", [script, "1"], { cwd: folder, env, encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /PERF_RUNS is gone, use PERF_BLOCKS/);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("the plan times 2.5 first and names the builds in order, whichever side calls", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const old = path.join(folder, "v244");
    const kernel = path.join(folder, "v250");
    mkdirSync(path.join(old, "crates", "crazyagent"), { recursive: true });
    mkdirSync(path.join(kernel, "crates", "kernel"), { recursive: true });
    const expected = ["order=2.5,2.4.4,2.4.4,2.5", kernel, old];
    for (const dirs of [[old, kernel], [kernel, old]]) {
      const planned = execFileSync("bash", [script, "--plan", ...dirs], { encoding: "utf8" }).trim().split("\n");
      assert.deepEqual(planned, expected);
    }
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

/**
 * A checkout with one 2.5 perf crate in a new git repository, and a folder of stand-in
 * tools put first on PATH: `cargo` prints one PERF row, and each other tool runs the shell
 * text given for it. Inputs: the temp folder and the stand-ins as name to shell text.
 * Output: the checkout and the environment that runs with the stand-ins.
 */
function standIns(folder, tools) {
  const checkout = path.join(folder, "checkout");
  const tests = path.join(checkout, "crates", "kernel", "storage", "src");
  mkdirSync(tests, { recursive: true });
  writeFileSync(path.join(checkout, "crates", "kernel", "storage", "Cargo.toml"), 'name = "stand-in"\n');
  writeFileSync(path.join(tests, "perf_tests.rs"), "\n");
  const git = (...words) => execFileSync("git", ["-C", checkout, "-c", "user.name=perf", "-c", "user.email=perf@example.invalid", ...words]);
  git("init", "-q");
  git("add", ".");
  git("commit", "-q", "-m", "stand-in");
  const bin = path.join(folder, "bin");
  mkdirSync(bin);
  const all = {
    cargo: 'echo "test perf_tests::rows ... PERF 1 storage stand.in p50_ns=5 p95_ns=6 n=200 batch=1000"',
    ...tools,
  };
  for (const [name, text] of Object.entries(all)) {
    writeFileSync(path.join(bin, name), `#!/bin/sh\n${text}\n`, { mode: 0o755 });
  }
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}` };
  return { checkout, env };
}

test("the runner reads load1 where sysctl has no vm.loadavg, as on Linux", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const { checkout, env } = standIns(folder, {
      sysctl: 'echo "sysctl: cannot stat /proc/sys/vm/loadavg: No such file or directory" >&2; exit 255',
    });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const result = spawnSync("bash", [script, "--inner", sprint, "1", log, checkout], { env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^PERF-RUN build=2\.5 .* load1=\d+(\.\d+)? /m);
    assert.match(result.stdout, /^PERF 1 storage stand\.in p50_ns=5 p95_ns=6 n=200 batch=1000$/m);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("the quiet wait refuses a load reader that prints nothing and never starts the runs", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const { checkout, env } = standIns(folder, { sysctl: "exit 0", python3: "exit 0" });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const calls = path.join(folder, "calls");
    writeFileSync(path.join(sprint, "env.sh"), "\n");
    writeFileSync(path.join(sprint, "slot.sh"), `printf '%s\\n' "$*" >> '${calls}'\n"$@"\n`);
    const quiet = { ...env, PERF_SPRINT: sprint, PERF_WAIT_QUIET: "1" };
    const result = spawnSync("bash", [script, "1"], { cwd: checkout, env: quiet, encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /run\.sh: no load average/);
    const called = readFileSync(calls, "utf8");
    assert.match(called, /--no-run/);
    assert.doesNotMatch(called, /--inner/);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a run refuses a load reader that prints text that is not a number and writes no quiet header", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const { checkout, env } = standIns(folder, { sysctl: "echo not-a-load", python3: "echo not-a-load" });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const result = spawnSync("bash", [script, "--inner", sprint, "1", log, checkout], { env, encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /run\.sh: no load average \(read 'not-a-load'\)/);
    assert.doesNotMatch(`${result.stdout}${existsSync(log) ? readFileSync(log, "utf8") : ""}`, /condition=quiet/);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("each measured run names its label and block where the ratio reader looks, and prints a start line", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const { checkout, env } = standIns(folder, {});
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const result = spawnSync("bash", [script, "--inner", sprint, "2", log, checkout], { env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    const heads = readFileSync(log, "utf8").split("\n").filter((line) => line.startsWith("PERF-RUN "));
    assert.equal(heads.length, 4);
    const blocks = heads.map((line) => line.match(/^PERF-RUN build=(\S+) label=(\S+) .* block=(\S+) load1=\S+/));
    assert.ok(blocks.every(Boolean), heads.join("\n"));
    assert.deepEqual(blocks.map((head) => [head[1], head[2], head[3]]), [["2.5", "2.5", "1"], ["2.5", "2.5", "1"], ["2.5", "2.5", "2"], ["2.5", "2.5", "2"]]);
    assert.equal((result.stderr.match(/^RUN build=2\.5 block=(warmup|1|2) start=/gm) || []).length, 5);
    assert.equal((result.stderr.match(/^RUN build=2\.5 block=(warmup|1|2) end=\S+ rc=0$/gm) || []).length, 5);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a measure past its deadline is stopped with its own process group and names its block", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const marker = `perf-deadline-${process.pid}`;
    const { checkout, env } = standIns(folder, { cargo: `exec /bin/sh -c 'sleep 30; :' ${marker}` });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const started = Date.now();
    const result = spawnSync("bash", [script, "--inner", sprint, "1", log, checkout], { env: { ...env, PERF_MEASURE_SECS: "1" }, encoding: "utf8", timeout: 20000 });
    assert.ok(Date.now() - started < 15000, `took ${Date.now() - started} ms`);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /^PERF-TIMEOUT build=2\.5 block=warmup after=1s$/m);
    const left = spawnSync("pgrep", ["-f", marker], { encoding: "utf8" });
    assert.equal(left.stdout.trim(), "", "the hung measure outlived its deadline");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("an exact filter reaches the test binary after the --, where cargo takes no --exact", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const cargo = [
      'seen=""; for word in "$@"; do',
      '  [ "$word" = "--" ] && { seen=1; continue; }',
      '  [ "$word" = "--exact" ] && [ -z "$seen" ] && { echo "error: unexpected argument \'--exact\' found"; exit 1; }',
      '  [ "$word" = "--exact" ] && exact=1',
      "done",
      '[ -n "$exact" ] || { echo "no exact flag for the test binary"; exit 1; }',
      'echo "test perf_tests::rows ... PERF 1 storage stand.in p50_ns=5 p95_ns=6 n=200 batch=1000"',
    ].join("\n");
    const { checkout, env } = standIns(folder, { cargo });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const result = spawnSync("bash", [script, "--inner", sprint, "1", log, checkout], { env: { ...env, PERF_FILTER: "perf_tests::rows", PERF_EXACT: "1" }, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.equal((readFileSync(log, "utf8").match(/^PERF 1 storage stand\.in /gm) || []).length, 2);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a failed warm-up measure prints the tail of its own output on stderr", () => {
  const folder = mkdtempSync(path.join(tmpdir(), "perf-run-"));
  try {
    const { checkout, env } = standIns(folder, { cargo: 'echo "error: the stand-in refused its arguments"; exit 1' });
    const sprint = path.join(folder, "sprint");
    mkdirSync(sprint);
    const log = path.join(folder, "runs.log");
    const result = spawnSync("bash", [script, "--inner", sprint, "1", log, checkout], { env, encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /^RUN build=2\.5 block=warmup end=\S+ rc=1$/m);
    assert.match(result.stderr, /^MEASURE-FAIL build=2\.5 block=warmup error: the stand-in refused its arguments$/m);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});
