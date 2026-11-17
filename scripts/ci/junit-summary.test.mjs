import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { count, summarize } from "./junit-summary.mjs";

const SCRIPT = fileURLToPath(new URL("./junit-summary.mjs", import.meta.url));

const NEXTEST = `<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1" errors="0" uuid="u" timestamp="t" time="812.5">
  <testsuite name="crazyagent::accounts" tests="3" disabled="0" errors="0" failures="1">
    <testcase name="a_row_passes" classname="crazyagent::accounts" timestamp="t" time="0.2">
    </testcase>
    <testcase name="a_row_&lt;fails&gt;" classname="crazyagent::accounts" timestamp="t" time="1.0">
      <failure type="test failure">thread panicked</failure>
      <system-out>out</system-out>
    </testcase>
    <testcase name="a_row_is_skipped" classname="crazyagent::accounts" time="0"><skipped/></testcase>
  </testsuite>
</testsuites>
`;

const NODE = `<?xml version="1.0" encoding="utf-8"?>
<testsuites>
  <testsuite name="outer" tests="2" failures="0" skipped="0">
    <testcase name="plain row" time="0.01" classname="test"/>
    <testsuite name="inner">
      <testcase name="broken row" time="0.02" classname="test">
        <error message="boom"/>
      </testcase>
    </testsuite>
  </testsuite>
</testsuites>
`;

const NODE_RAW_GT = `<?xml version="1.0" encoding="utf-8"?>
<testsuites>
  <testcase name="reads farm-junit-&lt;part> folders" time="0.4" classname="test"/>
  <testcase name="a build-&lt;version> tag row" time="0.1" classname="test">
    <failure type="testCodeFailure" message="x"/>
  </testcase>
  <testcase name="last row" time="0.1" classname="test"/>
</testsuites>
`;

test("one JUnit file counts its test cases, failures (failure or error), skips and the run's seconds, names decoded", () => {
  assert.deepEqual(count(NEXTEST), { tests: 3, failed: ["crazyagent::accounts a_row_<fails>"], skipped: 1, seconds: 812.5 });
  assert.deepEqual(count(NODE), { tests: 2, failed: ["test broken row"], skipped: 0, seconds: null });
});

test("a raw > inside a quoted attribute, as node's JUnit reporter writes it, neither hides nor merges test cases", () => {
  assert.deepEqual(count(NODE_RAW_GT), { tests: 3, failed: ["test a build-<version> tag row"], skipped: 0, seconds: null });
});

test("a numeric entity outside Unicode stays as written instead of stopping the report", () => {
  const xml = '<testsuites><testcase name="a &#x110000; b &#99999999999; c &#65;" classname="t"><failure/></testcase></testsuites>';
  assert.deepEqual(count(xml).failed, ["t a &#x110000; b &#99999999999; c A"]);
});

test("a job the report needs that did not succeed fails the run and is named, even when every part's JUnit passed", () => {
  const pass = NEXTEST.replace(/<failure[^]*?<\/failure>/, "");
  const files = new Map([["linux-rust-1", [pass]]]);
  assert.equal(summarize(["linux-rust-1"], files, { rust: { result: "success" }, typecheck: { result: "success" } }).ok, true);
  const red = summarize(["linux-rust-1"], files, { rust: { result: "success" }, typecheck: { result: "failure" }, node: { result: "cancelled" } });
  assert.equal(red.ok, false);
  assert.match(red.text, /^job typecheck: failure$/m);
  assert.match(red.text, /^job node: cancelled$/m);
  assert.doesNotMatch(red.text, /^job rust:/m);
});

test("the summary is one table row per expected part, a part with no JUnit is missing and fails the run", () => {
  const result = summarize(["linux-rust-1", "linux-node-1", "macos-rust-1"], new Map([["linux-rust-1", [NEXTEST]], ["linux-node-1", [NODE]]]), {});
  assert.equal(result.ok, false);
  assert.match(result.text, /^\| part \| tests \| failed \| skipped \| seconds \|$/m);
  assert.match(result.text, /^\| linux-rust-1 \| 3 \| 1 \| 1 \| 813 \|$/m);
  assert.match(result.text, /^\| linux-node-1 \| 2 \| 1 \| 0 \| - \|$/m);
  assert.match(result.text, /^\| macos-rust-1 \| missing \| - \| - \| - \|$/m);
  assert.match(result.text, /^\| all \| 5 \| 2 \| 1 \| - \|$/m);
  assert.match(result.text, /^- linux-rust-1 crazyagent::accounts a_row_<fails>$/m);
  assert.match(result.text, /^- linux-node-1 test broken row$/m);
  assert.match(result.text, /missing: macos-rust-1/);
});

test("a run whose every part passed is ok, and an empty JUnit file is a failure, not a pass", () => {
  const pass = NEXTEST.replace(/<failure[^]*?<\/failure>/, "");
  assert.equal(summarize(["linux-rust-1"], new Map([["linux-rust-1", [pass]]]), {}).ok, true);
  const empty = summarize(["linux-rust-1"], new Map([["linux-rust-1", ["<testsuites></testsuites>"]]]), {});
  assert.equal(empty.ok, false);
  assert.match(empty.text, /no test case: linux-rust-1/);
});

test("the command line reads farm-junit-<part> folders for FARM_PARTS and the needed jobs' results from FARM_NEEDS, and exits 1 on a failure, a missing part or a red job", () => {
  const root = mkdtempSync(join(tmpdir(), "farm-junit-"));
  try {
    mkdirSync(join(root, "farm-junit-linux-rust-1"));
    writeFileSync(join(root, "farm-junit-linux-rust-1", "junit.xml"), NEXTEST.replace(/<failure[^]*?<\/failure>/, ""));
    const green = JSON.stringify({ rust: { result: "success", outputs: {} }, typecheck: { result: "success", outputs: {} } });
    const run = (parts, needs = green) => spawnSync(process.execPath, [SCRIPT, root], { env: { PATH: process.env.PATH, FARM_PARTS: parts, FARM_NEEDS: needs }, encoding: "utf8" });
    const good = run("linux-rust-1");
    assert.equal(good.status, 0, good.stderr);
    assert.match(good.stdout, /^\| linux-rust-1 \| 3 \| 0 \| 1 \| 813 \|$/m);
    const short = run("linux-rust-1\n  windows-rust-1");
    assert.equal(short.status, 1);
    assert.match(short.stdout, /^\| windows-rust-1 \| missing \|/m);
    const typecheck = run("linux-rust-1", JSON.stringify({ rust: { result: "success" }, typecheck: { result: "failure" } }));
    assert.equal(typecheck.status, 1);
    assert.match(typecheck.stdout, /^job typecheck: failure$/m);
    for (const needs of ["", "[]", "null", '{"rust":{}}', "{"]) assert.notEqual(run("linux-rust-1", needs).status, 0, `FARM_NEEDS ${needs}`);
    assert.notEqual(spawnSync(process.execPath, [SCRIPT, root], { env: { PATH: process.env.PATH, FARM_NEEDS: green } }).status, 0);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
