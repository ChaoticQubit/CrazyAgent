import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { HOSTED_CONFIGS, KEPT_TABLES, NOT_RUN, SUITES, classify, denialsUnder, keptQueries, fixtureFor, logStart, onlyFiles, specRows } from "./e2e-hosted.mjs";
import rowReporter, { rowLine } from "./row-reporter.mjs";

const NATIVE = new URL("../../apps/desktop/test/native/", import.meta.url);
const listing = readdirSync(NATIVE);
const read = (file) => readFileSync(new URL(file, NATIVE), "utf8");

test("every native e2e file is run by the hosted job or named as not run, exactly once", () => {
  const sorted = classify(listing.filter((file) => file.endsWith(".e2e.mjs")), listing.filter((file) => file.endsWith(".wdio.conf.mjs")), read);
  assert.deepEqual([sorted.unplaced, sorted.twice], [[], []]);
  assert.ok(sorted.nodeTest.length > 50 && sorted.hosted.length >= HOSTED_CONFIGS.length && sorted.suites.length > 10, JSON.stringify(sorted));
  for (const name of [...Object.keys(NOT_RUN), ...HOSTED_CONFIGS]) assert.ok(existsSync(new URL(name, NATIVE)), name);
});

test("a file no configuration names and that is not a node:test file is unplaced", () => {
  const texts = { "a.e2e.mjs": "describe('x')", "b.e2e.mjs": 'import { test } from "node:test";' };
  const sorted = classify(["a.e2e.mjs", "b.e2e.mjs"], [], (file) => texts[file] ?? "");
  assert.deepEqual([sorted.unplaced, sorted.nodeTest], [["a.e2e.mjs"], ["b.e2e.mjs"]]);
});

test("a file that names the clean-Mac gate feature runs on the gated owner fixture, every other on the plain one", () => {
  assert.equal(fixtureFor("must be built with --features crazyagent/clean-mac-gate", "/plain", "/gated"), "/gated");
  assert.equal(fixtureFor("describe('x')", "/plain", "/gated"), "/plain");
  assert.throws(() => fixtureFor("", "/same", "/same"), /two distinct absolute fixtures/);
  assert.throws(() => fixtureFor("", "plain", "/gated"), /two distinct absolute fixtures/);
  const sorted = classify(listing.filter((file) => file.endsWith(".e2e.mjs")), listing.filter((file) => file.endsWith(".wdio.conf.mjs")), read);
  for (const suite of SUITES) assert.ok(sorted.suites.includes(`${suite}.e2e.mjs`), suite);
  const gated = SUITES.filter((suite) => fixtureFor(read(`${suite}.e2e.mjs`), "/plain", "/gated") === "/gated");
  assert.deepEqual(gated, ["first-run-harnesses-node", "refused-tool"]);
  const nodeGated = sorted.nodeTest.filter((file) => fixtureFor(read(file), "/plain", "/gated") === "/gated");
  assert.deepEqual(nodeGated, []);
});

test("the spec reporter's rows are read with their spec file and result", () => {
  const text = [
    "[0-0] Running: tauri",
    "[Tauri #0-0] » /home/runner/work/x/apps/desktop/test/native/master-switch.e2e.mjs",
    "[Tauri #0-0] master switch",
    "[Tauri #0-0]    ✓ saves the pick",
    "[Tauri #0-0]    ✖ shows the pick again",
    "[Tauri #0-0]    - a skipped row",
    "------------------------------------------------------------------",
    "[Tauri #0-0] 1 passing (3.2s)",
  ].join("\n");
  assert.deepEqual(specRows(text, "fallback"), [
    { file: "master-switch.e2e.mjs", name: "saves the pick", result: "ok" },
    { file: "master-switch.e2e.mjs", name: "shows the pick again", result: "not ok" },
    { file: "master-switch.e2e.mjs", name: "a skipped row", result: "skip" },
  ]);
});

test("the row reporter prints a line per row that is not a suite, then the tally", async () => {
  const file = `${process.cwd()}/a.e2e.mjs`;
  const events = [
    { type: "test:pass", data: { name: "one", file, details: {} } },
    { type: "test:fail", data: { name: "two", file, details: { error: { message: "wrong\nmore" } } } },
    { type: "test:pass", data: { name: "three", file, skip: "no keys", details: {} } },
    { type: "test:pass", data: { name: "group", file, details: { type: "suite" } } },
  ];
  const lines = [];
  for await (const line of rowReporter((async function* () { yield* events; })())) lines.push(line);
  assert.deepEqual(lines, [rowLine("a.e2e.mjs", "one", "ok"), rowLine("a.e2e.mjs", "two", "not ok", "wrong"),
    rowLine("a.e2e.mjs", "three", "skip", "no keys"), "ROWS ok 1 not-ok 1 skip 1\n"]);
});

test("E2E_HOSTED_ONLY keeps the named node:test files in job order and refuses a name the job does not run", () => {
  const files = ["a.e2e.mjs", "b.e2e.mjs", "c.e2e.mjs"];
  assert.deepEqual(onlyFiles(files, ""), files);
  assert.deepEqual(onlyFiles(files, undefined), files);
  assert.deepEqual(onlyFiles(files, " c.e2e.mjs,a.e2e.mjs "), ["a.e2e.mjs", "c.e2e.mjs"]);
  assert.throws(() => onlyFiles(files, "a.e2e.mjs,d.e2e.mjs"), /d\.e2e\.mjs/);
});

test("the Sandbox lines kept are those naming the temporary folder, with or without /private", () => {
  const folder = "/private/var/folders/x/T";
  const text = ["deny(1) file-write-create /private/var/folders/x/T/ca-golden-1/a", "deny(1) file-read-data /var/folders/x/T/ca-ch-2/b", "deny(1) file-read-data /Users/runner/c", ""].join("\n");
  assert.deepEqual(denialsUnder(text, folder), text.split("\n").slice(0, 2));
  assert.throws(() => denialsUnder(text, "var/folders"));
});

test("log show starts at the run's local time to the second", () => {
  assert.equal(logStart(new Date(2026, 10, 4, 5, 6, 7, 800)), "2026-11-04 05:06:07");
});

test("an engine database's kept tables are read in their fixed order, and only those it holds", () => {
  assert.deepEqual(keptQueries(["landing_change_sets", "team_agents", "pools"]), [
    "SELECT * FROM team_agents ORDER BY rowid DESC LIMIT 200;",
    "SELECT * FROM landing_change_sets ORDER BY rowid DESC LIMIT 200;",
  ]);
  assert.equal(keptQueries([...KEPT_TABLES]).length, KEPT_TABLES.length);
  assert.deepEqual(keptQueries([]), []);
});
