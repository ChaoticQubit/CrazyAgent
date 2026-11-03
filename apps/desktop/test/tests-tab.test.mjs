import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const api = await import("../src/api-checks.ts");
const tests = await import("../src/screens/project/tests.tsx");
const runView = await import("../src/screens/project/test-run.tsx");
const checks = await import("../src/screens/settings/checks.tsx");
const router = await import("../src/ui/router.ts");
const projectPage = await import("../src/screens/project/index.tsx");

const repo = new URL("../../../", import.meta.url);
const source = (path) => readFileSync(new URL(path, repo), "utf8");
const render = (element) => renderToStaticMarkup(element);
const shape = source("crates/trackwright-core/src/runs/shape.rs");
const runsSource = source("crates/trackwright-core/src/runs.rs");
const named = source("crates/trackwright-core/src/tests_model.rs");
const catalog = source("crates/trackwright-core/src/catalog.rs");
const settingsSource = source("crates/crazyagent/src/checks/settings.rs");

const hex = (char, length) => char.repeat(length);
const markup = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#x27;");
const escapeRe = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const COMMIT_A = hex("a", 40);
const PROFILE = hex("d", 64);
const APPROVED_MS = 1_699_999_000_000;

function structFields(text, name) {
  const match = new RegExp(`pub struct ${name} \\{([^}]*)\\}`).exec(text);
  assert.ok(match, `${name} is a struct in the source`);
  return [...match[1].matchAll(/pub (\w+):/g)].map((field) => field[1]);
}

function variantFields(text, enumName, variant) {
  const body = text.slice(text.indexOf(`pub enum ${enumName} {`));
  const match = new RegExp(`${variant} \\{([^}]*)\\}`).exec(body);
  assert.ok(match, `${enumName}::${variant} is in the source`);
  return [...match[1].matchAll(/(\w+): /g)].map((field) => field[1]);
}

function wireNames(text, name) {
  const match = new RegExp(`\\n\\s*${name} \\{([^}]*)\\}`).exec(text);
  assert.ok(match, `${name} is a named enum in the source`);
  return [...match[1].matchAll(/=> "([a-z_0-9]+)"/g)].map((wire) => wire[1]);
}

function opFields(kind) {
  const start = catalog.indexOf(`kind: OpKind::${kind},`);
  assert.ok(start > 0, `${kind} is in the catalog`);
  const block = catalog.slice(start, catalog.indexOf("OpSpec {", start));
  const constants = { PROJECT: "project", CURSOR: "cursor", CASE_ID: "id", RUN_ID: "id", PLAN_ID: "id", SET_ID: "id" };
  const listed = [...block.matchAll(/(?:opt|req)\(\s*"(\w+)"/g)].map((field) => field[1]);
  const shared = [...block.matchAll(/\b([A-Z_]+),/g)].map((field) => constants[field[1]]).filter(Boolean);
  return new Set([...listed, ...shared]);
}

function snake(variant) {
  return variant.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}

function enumOptions(name) {
  const match = new RegExp(`impl ${name} \\{[^}]*pub const ALL: \\[Self; \\d+\\] = \\[([^\\]]*)\\]`).exec(settingsSource);
  assert.ok(match, `${name}::ALL is in settings.rs`);
  return [...match[1].matchAll(/Self::(\w+)/g)].map((variant) => snake(variant[1]));
}

function enumDefault(name) {
  const match = new RegExp(`pub enum ${name} \\{[^}]*#\\[default\\]\\s*(\\w+)`).exec(settingsSource);
  assert.ok(match, `${name} has a default`);
  return snake(match[1]);
}

function numberOptions(name) {
  const match = new RegExp(`pub const ${name}: \\[u8; \\d+\\] = \\[([^\\]]*)\\]`).exec(settingsSource);
  assert.ok(match, `${name} is in settings.rs`);
  return match[1].split(",").map((item) => Number(item.trim()));
}

function numberDefault(key) {
  const block = settingsSource.slice(settingsSource.indexOf("impl Default for ChecksSettings"));
  const match = new RegExp(`${key}: (\\d+),`).exec(block);
  assert.ok(match, `${key} has a default`);
  return Number(match[1]);
}

const ENUM_OF = { gate: "GateMode", inventory: "InventoryMode", skip_markers: "SkipMarkers", protected_change: "ProtectedChange", removed_tests: "RemovedTests" };
const NUMBERS_OF = { parallel: "PARALLEL_OPTIONS", keep_failed_snapshots: "KEEP_FAILED_OPTIONS", agent_requests: "AGENT_REQUESTS_OPTIONS" };

function engineChoices() {
  const block = settingsSource.slice(settingsSource.indexOf("pub fn choices()"));
  const keys = [...block.matchAll(/listed\(\s*"(\w+)"/g)].map((key) => key[1]);
  assert.ok(keys.length >= 8, "choices() lists every checks key");
  return keys.map((key) => {
    if (Object.hasOwn(ENUM_OF, key)) return { key, options: enumOptions(ENUM_OF[key]), proposed: enumDefault(ENUM_OF[key]), custom: null };
    assert.ok(Object.hasOwn(NUMBERS_OF, key), `${key} is a known checks key`);
    return { key, options: numberOptions(NUMBERS_OF[key]), proposed: numberDefault(key), custom: null };
  });
}

function settingsView(project, values = {}, below = null) {
  const choices = engineChoices();
  const proposal = Object.fromEntries(choices.map((choice) => [choice.key, choice.proposed]));
  const under = below ?? proposal;
  const effective = Object.fromEntries(choices.map((choice) => [choice.key, values[choice.key] ?? under[choice.key]]));
  const all = Object.fromEntries(choices.map((choice) => [choice.key, values[choice.key] ?? null]));
  return { project, values: all, version: 4, below: under, effective, choices };
}

function runHeader(over = {}) {
  return {
    id: "CA-R7", plan: "CA-P2", batch: "b-1", title: "unit on change 12", state: "finished", verdict: "passed", reason: null,
    started_ms: 1_700_000_000_000, finished_ms: 1_700_000_060_000, recorded_by: "engine", totals: { total: 4, passed: 2, failed: 0, skipped: 1, errored: 0 }, version: 3, ...over,
  };
}

function runShowData(over = {}) {
  return {
    run: runHeader(),
    source: { kind: "git", repository: "/Users/owner/app", commit: COMMIT_A, tree: hex("b", 40), base: hex("c", 40), change_set: "12" },
    policy: {
      profile: PROFILE, profile_approved_ms: APPROVED_MS, check: "unit", kind: "unit", argv: ["cargo", "test", "--", "two words", ""],
      toolchain: { family: "rust", version: "1.95.0" }, sandbox: hex("e", 64), network: "off", timeout_s: 600, environment: hex("f", 64),
    },
    integrity: { executed: 2, inventory: { baseline_run: "CA-R3", removed: [], removed_total: 0 }, new_skips: [], protected_touched: [], holdouts_ran: true, build_ok: true },
    unlinked_total: 3,
    unlinked: ["tests::stray_one", "tests::\u202estray"],
    results: [
      { position: 1, name: "unit::adds", outcome: "passed", case: "CA-T1", duration_ms: 12, message: null, artifact: null },
      { position: 2, name: "unit::\u200bhidden", outcome: "skipped", case: null, duration_ms: null, message: "skipped by marker", artifact: null },
      { position: 4, name: "holdout::kept", outcome: "passed", case: "CA-T9", duration_ms: 30, message: "owner sees this", artifact: "report.xml" },
    ],
    ...over,
  };
}

test("the_covered_block_names_every_binding_field", () => {
  const header = Object.keys(runHeader()).sort();
  const viewFields = structFields(runsSource, "RunView").filter((field) => !["source", "policy", "integrity"].includes(field)).sort();
  assert.deepEqual(header, viewFields, "the fixture's run header is RunView without its binding, as run_show sends it");
  const policyFields = structFields(shape, "PolicyRef");
  const gitFields = variantFields(shape, "SourceRef", "Git");
  assert.deepEqual(Object.keys(runShowData().policy).sort(), [...policyFields].sort());
  assert.deepEqual(Object.keys(runShowData().source).filter((key) => key !== "kind").sort(), [...gitFields].sort());
  const page = api.runPage(runShowData(), "result:4");
  assert.ok(page, "a run_show first page reads");
  const rows = api.coveredRows(page.detail);
  const expected = [...gitFields.map((field) => `source.${field}`), ...policyFields.map((field) => `policy.${field}`)];
  assert.deepEqual(rows.map((row) => row.field).sort(), expected.sort());
  const value = (field) => rows.find((row) => row.field === field).value;
  assert.equal(value("source.commit"), COMMIT_A);
  assert.equal(value("policy.profile"), PROFILE);
  assert.equal(value("policy.profile_approved_ms"), api.whenText(APPROVED_MS));
  assert.equal(value("policy.argv"), "cargo test -- 'two words' ''");
  assert.equal(value("policy.toolchain"), "rust 1.95.0");
  assert.equal(value("policy.network"), "Off, no sockets");
  assert.equal(value("policy.timeout_s"), "600 s");
  const html = render(createElement(runView.RunDetailView, { page: { kind: "ok", value: page }, more: [], busy: false, onMore: () => {}, onBack: () => {}, onRetry: () => {} }));
  for (const row of rows) {
    assert.ok(html.includes(row.label), `Covered shows ${row.label}`);
    assert.ok(html.includes(row.value.replaceAll("'", "&#x27;")), `Covered shows the value of ${row.field}`);
  }
  assert.ok(html.includes("Covered"));
  const cut = api.runPage(runShowData({ policy: undefined, policy_cut: true, source: undefined, source_cut: true }), null);
  assert.ok(cut, "a page whose binding was cut still reads");
  assert.equal(cut.detail.policy, "cut");
  const cutRows = api.coveredRows(cut.detail);
  assert.deepEqual(cutRows.map((row) => row.field).sort(), expected.sort(), "a cut binding still names every field");
  assert.ok(cutRows.every((row) => row.value === api.CUT_TEXT));
  const declared = api.coveredRows({ source: { kind: "declared", ref: "ci-build-41" }, policy: page.detail.policy });
  assert.deepEqual(declared[0], { field: "source.ref", label: "Declared source", value: "ci-build-41" });
});

test("a run page of another shape is refused, not shown", () => {
  assert.equal(api.runPage({ ...runShowData(), run: runHeader({ state: "finished", verdict: null }) }, null), null);
  assert.equal(api.runPage(runShowData({ policy: { ...runShowData().policy, profile: "short" } }), null), null);
  assert.equal(api.runPage(runShowData({ source: { kind: "git", repository: "r", commit: "nothex", tree: COMMIT_A, base: null, change_set: null } }), null), null);
  assert.equal(api.runPage(runShowData(), "case:3"), null, "a cursor of another op is refused");
  assert.equal(api.runPage(runShowData({ results: [runShowData().results[1], runShowData().results[0]] }), null), null, "results come in position order");
  assert.equal(api.runPage(runShowData({ unlinked_total: 1 }), null), null, "listed unlinked names are some of the total");
  assert.equal(api.runPage(runShowData({ integrity: null }), null).detail.integrity, null);
  const more = api.runResults({ id: "CA-R7", results: runShowData().results.slice(0, 1) }, null, "CA-R7");
  assert.deepEqual(more, { results: runShowData().results.slice(0, 1), cursor: null });
  assert.equal(api.runResults({ id: "CA-R8", results: [] }, null, "CA-R7"), null, "a later page of another run is refused");
});

test("every verdict, state, outcome and stop reason has its own words", () => {
  assert.deepEqual([...api.VERDICTS], wireNames(named, "Verdict"));
  assert.deepEqual([...api.RUN_STATES], wireNames(named, "RunState"));
  assert.deepEqual([...api.OUTCOMES], wireNames(named, "ResultOutcome"));
  assert.deepEqual([...api.CASE_KINDS], wireNames(named, "CaseKind"));
  assert.deepEqual([...api.CASE_STATUSES], wireNames(named, "CaseStatus"));
  assert.deepEqual([...api.PLAN_STATUSES], wireNames(named, "PlanStatus"));
  assert.deepEqual([...api.STOP_REASONS], wireNames(shape, "StopReason"));
  const labels = api.VERDICTS.map((verdict) => api.verdictText(verdict).label);
  const lines = api.VERDICTS.map((verdict) => api.verdictText(verdict).line);
  assert.equal(new Set(labels).size, labels.length, "every verdict reads differently");
  assert.equal(new Set(lines).size, lines.length);
  assert.deepEqual(labels, ["Passed", "Failed", "No tests ran", "Build failed", "Result unclear", "Couldn't run"]);
  for (const verdict of api.VERDICTS) assert.equal(api.runLabel({ state: "finished", verdict, reason: null }), api.verdictText(verdict).label);
  assert.equal(api.runLabel({ state: "running", verdict: null, reason: null }), "Running");
  assert.equal(api.runLabel({ state: "interrupted", verdict: null, reason: "cancelled" }), "Cancelled");
  assert.equal(api.runLabel({ state: "interrupted", verdict: null, reason: "timed_out" }), "Timed out");
  assert.throws(() => api.runLabel({ state: "finished", verdict: null, reason: null }), /a finished run has a verdict/);
  assert.equal(api.runTone({ state: "finished", verdict: "passed" }), "ok");
  assert.equal(api.runTone({ state: "finished", verdict: "failed" }), "danger");
  assert.equal(api.runTone({ state: "finished", verdict: "no_tests" }), "warn");
  const outcomeLabels = api.OUTCOMES.map(api.outcomeLabel);
  assert.equal(new Set(outcomeLabels).size, outcomeLabels.length);
  const rows = api.VERDICTS.map((verdict, at) => ({ id: `CA-R${at + 1}`, title: `run ${verdict}`, elided: false, state: "finished", verdict, started_ms: 1_000, finished_ms: 2_000, totals: { total: 1, passed: 1, failed: 0, skipped: 0, errored: 0 } }));
  const html = render(createElement(tests.RunsView, { page: { kind: "ok", value: { rows, cursor: null } }, filter: { verdict: null, plan: null, batch: null, commit: null }, pages: 1, onFilter: () => {}, onOpen: () => {}, onPrevious: () => {}, onNext: () => {}, onRetry: () => {} }));
  for (const label of labels) assert.ok(html.includes(label.replaceAll("'", "&#x27;")), `the runs list says ${label}`);
  assert.equal(api.totalsLine({ total: 4, passed: 2, failed: 0, skipped: 1, errored: 0 }), "2 passed, 0 failed, 1 skipped, 1 not run of 4");
  assert.equal(api.totalsLine({ total: 0, passed: 0, failed: 0, skipped: 0, errored: 0 }), "No results");
});

test("the Tests tab filters send only the fields the catalog names, and offer every option", () => {
  const caseFields = opFields("CaseList");
  const runFields = opFields("RunList");
  const none = { kind: null, status: null, set: null, unlinked: false };
  assert.deepEqual(api.caseListArgs(none, null), {});
  const every = api.caseListArgs({ kind: "e2e", status: "ready", set: "CA-S2", unlinked: true }, "case:9");
  assert.deepEqual(every, { kind: "e2e", status: "ready", set: "CA-S2", unlinked: true, cursor: "case:9" });
  for (const key of Object.keys(every)) assert.ok(caseFields.has(key), `case_list takes ${key}`);
  assert.ok(caseFields.has("project"), "the bridge adds the project case_list needs");
  const runs = api.runListArgs({ verdict: "failed", plan: "CA-P1", batch: "b-2", commit: COMMIT_A }, "run:40");
  assert.deepEqual(runs, { verdict: "failed", plan: "CA-P1", batch: "b-2", commit: COMMIT_A, cursor: "run:40" });
  for (const key of Object.keys(runs)) assert.ok(runFields.has(key), `run_list takes ${key}`);
  assert.deepEqual(api.runListArgs({ verdict: null, plan: null, batch: null, commit: null }, null), {});
  const cases = [
    { id: "CA-T1", title: "adds \u202enumbers", elided: false, protected: false, kind: "unit", status: "ready", latest: "passed" },
    { id: "CA-T2", title: "holdout case", elided: true, protected: true, kind: null, status: null, latest: null },
  ];
  const html = render(createElement(tests.CasesView, { page: { kind: "ok", value: { rows: cases, cursor: "case:2" } }, filter: none, pages: 1, onFilter: () => {}, onPrevious: () => {}, onNext: () => {}, onRetry: () => {} }));
  for (const kind of api.CASE_KINDS) assert.ok(html.includes(`value="${kind}"`), `the kind filter offers ${kind}`);
  for (const status of api.CASE_STATUSES) assert.ok(html.includes(`value="${status}"`), `the status filter offers ${status}`);
  assert.ok(html.includes("Only cases linked to no criterion"));
  assert.ok(html.includes("adds \\u202enumbers"), "agent titles show hidden characters as escapes");
  assert.ok(!html.includes("\u202e"));
  assert.ok(html.includes("Protected"));
  const runsHtml = render(createElement(tests.RunsView, { page: { kind: "ok", value: { rows: [], cursor: null } }, filter: { verdict: null, plan: null, batch: null, commit: null }, pages: 1, onFilter: () => {}, onOpen: () => {}, onPrevious: () => {}, onNext: () => {}, onRetry: () => {} }));
  for (const verdict of api.VERDICTS) assert.ok(runsHtml.includes(`value="${verdict}"`), `the verdict filter offers ${verdict}`);
  assert.ok(runsHtml.includes("Change set batch") && runsHtml.includes("Commit"), "runs filter by a change set's batch and commit");
  assert.deepEqual(api.casePage({ cases }, "case:2"), { rows: cases, cursor: "case:2" });
  assert.equal(api.casePage({ cases }, "run:2"), null);
  assert.equal(api.casePage({ rows: cases }, null), null, "case_list sends its rows as cases");
  assert.equal(api.runListPage({ runs: [] }, null).rows.length, 0);
  assert.equal(api.planPage({ plans: [{ id: "CA-P1", name: "release", status: "active", items: 3 }] }, null).rows[0].items, 3);
  assert.deepEqual(api.versionNames({ unplanned: { closed: 0, total: 0 }, versions: [{ name: "1.0" }, { name: "1.1" }], cycles: [] }), ["1.0", "1.1"]);
});

test("run detail shows results, unlinked names and integrity, agent text escaped", () => {
  const page = api.runPage(runShowData({ integrity: { executed: 0, inventory: { baseline_run: "CA-R3", removed: ["unit::gone"], removed_total: 2 }, new_skips: [{ path: "src/a.rs", line: 9, marker: "#[ignore]" }], protected_touched: ["tests/"], holdouts_ran: false, build_ok: false } }), "result:4");
  const lines = api.integrityLines(page.detail.integrity);
  assert.deepEqual(lines.map((line) => line.text), [
    "0 tests ran.",
    "2 tests CA-R3 ran did not run: unit::gone, and 1 more",
    "New skip marker #[ignore] at src/a.rs:9",
    "Protected paths changed: tests/",
    "No holdout checks ran.",
    "The build failed.",
  ]);
  assert.deepEqual(lines.map((line) => line.warn), [true, true, true, true, false, true]);
  assert.equal(api.integrityLines(null)[0].text, "No integrity evidence was recorded for this run.");
  const html = render(createElement(runView.RunDetailView, { page: { kind: "ok", value: page }, more: [], busy: false, onMore: () => {}, onBack: () => {}, onRetry: () => {} }));
  for (const line of lines) assert.ok(html.includes(line.text.replaceAll("'", "&#x27;")));
  assert.ok(html.includes("unit::\\u200bhidden"), "result names show hidden characters as escapes");
  assert.ok(html.includes("tests::\\u202estray"), "unlinked names show hidden characters as escapes");
  assert.ok(html.includes("1 more unlinked name"), "unlinked names say how many are not listed");
  assert.ok(html.includes("owner sees this"), "the owner sees a protected case's message");
  assert.ok(html.includes("Load more results"), "a page with a cursor offers the next results");
  assert.ok(!html.includes("\u200b") && !html.includes("\u202e"));
  const withheld = api.runPage(runShowData({ results: [{ position: 1, name: "holdout::x", outcome: "failed", case: "CA-T9", duration_ms: null, message: null, artifact: null, withheld: true }] }), null);
  const hidden = render(createElement(runView.RunDetailView, { page: { kind: "ok", value: withheld }, more: [], busy: false, onMore: () => {}, onBack: () => {}, onRetry: () => {} }));
  assert.ok(hidden.includes("Message withheld"), "a withheld result says so");
  assert.ok(!hidden.includes("Load more results"));
});

test("the stale sentence names why a change set's run stopped counting", () => {
  const head = COMMIT_A;
  const current = { id: PROFILE, approved_ms: APPROVED_MS };
  const run = { commit: head, profile: PROFILE, profile_approved_ms: APPROVED_MS };
  assert.equal(api.staleLine(run, head, current), null);
  assert.equal(api.staleLine({ ...run, commit: hex("9", 40) }, head, current), "This run covered an earlier version of the change, so it doesn't count for Accept. Run checks again.");
  assert.equal(api.staleLine({ ...run, profile: hex("8", 64) }, head, current), "This run used a profile that is no longer the approved one, so it doesn't count for Accept. Run checks again.");
  assert.equal(api.staleLine({ ...run, profile_approved_ms: APPROVED_MS + 1 }, head, current), "This run used a profile that is no longer the approved one, so it doesn't count for Accept. Run checks again.");
  assert.equal(api.staleLine(run, head, null), "No check profile is approved now, so this run doesn't count for Accept.");
  assert.throws(() => api.staleLine(run, "", current), /a change set has a head/);
  const row = (state, approved) => ({ id: PROFILE, state, proposed_by: "agent:3", proposed_ms: 1, approved_ms: approved, revoked_ms: null, question: null, checks: 2, holdouts: 1 });
  assert.deepEqual(api.currentProfile([row("proposed", null), row("current", APPROVED_MS)]), current);
  assert.equal(api.currentProfile([row("revoked", APPROVED_MS)]), null);
  assert.throws(() => api.currentProfile([row("current", APPROVED_MS), row("current", APPROVED_MS)]), /at most one profile is in use/);
  const latest = [
    { check: "unit", run_id: "CA-R7", batch: "b-1", state: "finished", verdict: "passed", commit: hex("9", 40), tree: hex("b", 40), profile: PROFILE, profile_approved_ms: APPROVED_MS, protected_touched: 0, holdout: false, finished_ms: 2_000 },
    { check: "secret", run_id: "CA-R8", batch: "b-1", state: "finished", verdict: "failed", commit: head, tree: hex("b", 40), profile: PROFILE, profile_approved_ms: APPROVED_MS, protected_touched: 1, holdout: true, finished_ms: 2_000 },
  ];
  const html = render(createElement(tests.ChangeSetChecksView, { checks: { kind: "ok", value: { jobs: [], latest, gate: { gate: "warn", removed_tests: "flag", head, profile: null, refusal: null, gaps: [], gaps_total: 0 } } }, head, current, busy: false, note: "", onRun: () => {}, onCancel: () => {}, onOpen: () => {}, onRetry: () => {} }));
  assert.ok(html.includes("This run covered an earlier version of the change"), "the change set view says which run is stale");
  assert.ok(html.includes("Run checks"));
  assert.ok(html.includes("Holdout"), "a holdout run is labelled");
  assert.ok(html.includes("Touched a protected path"));
  assert.ok(html.includes("Failed") && html.includes("Passed"));
});

test("the gate line says what the engine's gate says: blocked only on its refusal", () => {
  const gate = source("crates/crazyagent/src/checks/gate.rs");
  for (const code of api.GATE_CODES) assert.ok(gate.includes(`"${code}"`), `${code} is a gate code in gate.rs`);
  assert.deepEqual([...api.GATE_CODES], ["checks_protected_change", "checks_new_skips", "checks_tests_removed", "checks_failed", "checks_stale", "checks_not_run"]);
  const owner = source("crates/crazyagent/src/engine/owner/transport/checks.rs");
  assert.deepEqual(structFields(owner, "CheckGateView"), ["gate", "removed_tests", "head", "profile", "refusal", "gaps", "gaps_total"]);
  assert.deepEqual(structFields(owner, "CheckGapView"), ["code", "check", "run", "paths", "paths_total"]);
  for (const code of api.GATE_CODES) {
    assert.ok(api.gapText(code).length > 10 && api.gapText(code) === api.checksRefusal({ code }), `${code} has a sentence`);
  }
  const view = (gate, codes, refusal = null, total = codes.length) => ({ gate, removed_tests: "flag", head: COMMIT_A, profile: PROFILE, refusal, gaps_total: total,
    gaps: codes.map((code) => ({ code, check: "unit\u202e", run: null, paths: ["a.rs", "b.rs"], paths_total: 5 })) });
  assert.deepEqual(api.gateLine(view("off", ["checks_failed"])), { text: "Checks don't affect Accept for this project.", tone: "neutral" });
  assert.deepEqual(api.gateLine(view("require", [])), { text: "Every required check passed on this change.", tone: "ok" });
  assert.deepEqual(api.gateLine(view("warn", ["checks_failed"])), { text: "A required check didn't pass on this change. Accept will go ahead and note it.", tone: "warn" });
  assert.deepEqual(api.gateLine(view("warn", ["checks_new_skips", "checks_failed"], "checks_new_skips")),
    { text: "This change adds a skip marker to the tests. Accept is blocked until this is fixed. 1 more gap.", tone: "danger" });
  assert.equal(api.gateLine(view("require", ["checks_failed"], "checks_failed", 4)).text, "A required check didn't pass on this change. Accept is blocked until this is fixed. 3 more gaps.");
  assert.throws(() => api.gateLine(view("require", ["checks_failed", "checks_stale"], "checks_failed", 1)), /counts every gap/);
  const html = render(createElement(tests.ChangeSetChecksView, { checks: { kind: "ok", value: { jobs: [], latest: [], gate: view("require", ["checks_not_run"], "checks_not_run") } }, head: COMMIT_A, current: null, busy: false, note: "", onRun: () => {}, onCancel: () => {}, onOpen: () => {}, onRetry: () => {} }));
  assert.ok(html.includes("Accept is blocked until this is fixed."));
  assert.ok(html.includes("Check: unit\\u202e."), "a gap's check name shows hidden characters as escapes");
  assert.ok(html.includes("a.rs, b.rs and 3 more"), "a gap names its first paths and counts the rest");
});

test("every checks setting lists every engine option with what it does, the default labelled", () => {
  assert.deepEqual(engineChoices().map((choice) => choice.key), [...api.CHECKS_KEYS], "the app lists the engine's keys in its order");
  for (const scope of ["app", "project"]) {
    const projectId = scope === "app" ? null : "p1";
    const page = api.checksPage(settingsView(projectId, scope === "app" ? {} : { gate: "require" }), projectId);
    const html = render(createElement(checks.ChecksSettingsView, { page: { kind: "ok", value: page }, scope, projectId, picked: {}, busy: false, note: "", onPick: () => {}, onSave: () => {}, onUseAppDefault: () => {}, onRetry: () => {} }));
    for (const row of page.keys) {
      assert.ok(html.includes(api.checksKeyText(row.key).label));
      assert.ok(html.includes(api.checksBelowLabel(row, scope)), `${row.key} labels the ${scope === "app" ? "default" : "app default"}`);
      for (const option of row.options) {
        const words = api.checksOptionText(row.key, option);
        assert.ok(html.includes(words.label.replaceAll("'", "&#x27;")), `${row.key} lists ${option}`);
        assert.ok(html.includes(words.line.replaceAll("'", "&#x27;")), `${row.key} says what ${option} does`);
        assert.ok(html.includes(`value="${option}"`), `${row.key} offers ${option} as a choice`);
      }
    }
    assert.ok(html.includes(scope === "app" ? "Default (Warn)" : "App default (Warn)"));
    assert.equal(html.includes("Use app default"), scope === "project", "only a project page that sets a key offers the app default");
  }
  const page = api.checksPage(settingsView(null), null);
  for (const row of page.keys) {
    for (const option of row.options) assert.deepEqual(api.checksValues(page.keys, { [row.key]: option }), { [row.key]: option }, `${row.key} saves ${option}`);
    assert.deepEqual(api.checksValues(page.keys, { [row.key]: null }), {});
  }
  assert.equal(api.checksChanged(page.keys, {}), false);
  assert.equal(api.checksChanged(page.keys, { gate: "off" }), true);
  assert.equal(api.checksChanged(page.keys, { gate: null }), false);
  assert.throws(() => api.checksOptionText("gate", "sometimes"), /gate has no option sometimes/);
  assert.throws(() => api.checksPage(settingsView("p2"), "p1"), /for the scope it was read for/);
  const project = api.checksPage(settingsView("p1", { parallel: 3 }, { ...settingsView(null).below, parallel: 2 }), "p1");
  assert.deepEqual(project.keys.find((row) => row.key === "parallel"), { key: "parallel", options: [1, 2, 3, 4], proposed: 1, here: 3, below: 2, effective: 3 });
  assert.equal(checks.saveNote({ kind: "error", code: "checks_settings_stale", message: "" }), api.checksRefusal({ code: "checks_settings_stale" }));
  assert.equal(checks.saveNote({ kind: "ok", value: undefined }), "Saved.");
});

test("profiles: approve and revoke follow the state, holdout files show digests only", () => {
  assert.deepEqual(api.PROFILE_STATES.map(api.profileActions), [{ approve: true, revoke: false }, { approve: false, revoke: true }, { approve: true, revoke: false }, { approve: false, revoke: false }]);
  assert.equal(api.proposerText("owner"), "You");
  assert.equal(api.proposerText("agent:4"), "Agent 4");
  assert.equal(api.proposerText("master:9"), "Master 9");
  assert.equal(api.profileTextProblem(""), "Write the profile as JSON first.");
  assert.equal(api.profileTextProblem("[1]"), "A profile is one JSON object.");
  assert.equal(api.profileTextProblem("{"), "The profile isn't valid JSON.");
  assert.equal(api.profileTextProblem(`{"x":"${"a".repeat(api.PROFILE_INPUT_BYTES_MAX)}"}`), "The profile is too large. Profile text is at most 32 KiB.");
  assert.equal(api.profileTextProblem('{"version":1,"checks":[]}'), null);
  const check = { id: "unit", kind: "unit", toolchain: "rust", argv: ["cargo", "test"], cwd: ".", format: "libtest", report: null, timeout_s: 600, network: "off", when: ["turn_end", "before_accept"], required: true, protected: false };
  const holdout = { ...check, id: "secret\u202e", argv: ["cargo", "test", "--test", "holdout"], protected: true };
  const profile = { id: PROFILE, project: "p1", profile: { version: 1, checks: [check], holdouts: [{ check: holdout, files: { "tests/holdout.rs": hex("1", 64), "tests/a.rs": hex("2", 64) } }], protected_paths: ["tests/"] }, proposed_by: "agent:4", proposed_ms: 1_000, approved_ms: null, revoked_ms: null, question: 12, state: "proposed" };
  assert.deepEqual(api.holdoutFiles(profile.profile.holdouts[0].files), [{ path: "tests/a.rs", digest: hex("2", 64) }, { path: "tests/holdout.rs", digest: hex("1", 64) }]);
  const html = render(createElement(checks.ProfileDetailView, { profile, busy: false, onApprove: () => {}, onRevoke: () => {} }));
  assert.ok(html.includes("Waiting for your approval"));
  assert.ok(html.includes("Agent 4"));
  assert.ok(html.includes("cargo test --test holdout"), "the owner sees a holdout check's command in full");
  assert.ok(html.includes(hex("1", 64)) && html.includes("tests/holdout.rs"), "holdout files show path and digest");
  assert.ok(html.includes("secret\\u202e"), "agent-proposed ids show hidden characters as escapes");
  assert.ok(html.includes("Approve") && !html.includes(">Revoke<"));
  const declared = structFields(source("crates/crazyagent/src/checks/profile.rs"), "CheckSpec");
  assert.deepEqual(Object.keys(check), declared, "the fixture has every CheckSpec field");
  for (const shown of [check, holdout]) {
    const fields = api.checkRows(shown);
    assert.deepEqual(fields.map((row) => row.field), declared, "every field Approve stands on has a row, in engine order");
    for (const row of fields) {
      const cell = new RegExp(`<dt>${row.label}</dt><dd[^>]*>${escapeRe(markup(row.value.replace("\u202e", "\\u202e")))}</dd>`);
      assert.match(html, cell, `${shown.id} shows ${row.field}`);
    }
  }
  const words = Object.fromEntries(api.checkRows(check).map((row) => [row.field, row.value]));
  assert.deepEqual(words, {
    id: "unit", kind: "Unit", toolchain: "Rust", argv: "cargo test", cwd: ".", format: "libtest text on standard output", report: "None",
    timeout_s: "600 s", network: "Off, no sockets", when: "at an agent's turn end, before you accept", required: "Required to pass", protected: "Not protected",
  });
  const reach = { ...check, toolchain: null, format: "junit", report: "target/junit.xml", network: "remotes", when: [], required: false, protected: true };
  assert.deepEqual(api.checkRows(reach).filter((row) => ["toolchain", "format", "report", "network", "when", "required", "protected"].includes(row.field)).map((row) => row.value),
    ["None managed", "A JUnit XML file", "target/junit.xml", "The project's sandbox remotes", "Never on its own", "Not required", "Protected"]);
  assert.ok(html.includes('Protected paths: <span class="pj-mono">tests/</span>'), "the protected paths are listed");
  const rows = [{ id: PROFILE, state: "current", proposed_by: "owner", proposed_ms: 1, approved_ms: APPROVED_MS, revoked_ms: null, question: null, checks: 2, holdouts: 1 }];
  const list = render(createElement(checks.ProfilesView, { profiles: { kind: "ok", value: { rows, next: null } }, selected: null, onSelect: () => {}, onRetry: () => {} }));
  assert.ok(list.includes("Approved, in use") && list.includes("2 checks, 1 holdout"));
  const confirm = render(createElement(checks.ProfileConfirm, { asking: { kind: "revoke", id: PROFILE }, busy: false, onConfirm: () => {}, onCancel: () => {} }));
  assert.ok(confirm.includes("Revoke this profile?") && confirm.includes("No check runs until you approve another profile, and earlier runs stop counting for Accept."), "revoke asks first and says what follows");
  const approve = render(createElement(checks.ProfileConfirm, { asking: { kind: "approve", id: PROFILE }, busy: false, onConfirm: () => {}, onCancel: () => {} }));
  assert.ok(approve.includes("Approve this profile?"));
});

test("every checks and tests code a refusal can carry has a sentence", () => {
  for (const [code, sentence] of Object.entries(api.CHECKS_REFUSALS)) {
    assert.ok(sentence.length > 10 && sentence.endsWith("."), `${code} reads as a sentence`);
    assert.equal(api.checksRefusal({ code }), sentence);
  }
  assert.ok(api.checksRefusal({ code: "owner_busy" }).length > 0, "other codes take the shared sentence");
});

test("the Tests tab and both Checks settings are reachable", () => {
  assert.ok(router.PROJECT_TABS.includes("tests"));
  assert.ok(router.SETTINGS_TABS.includes("checks"));
  assert.deepEqual(router.parseRoute("#/projects/p1/tests"), { name: "project", id: "p1", tab: "tests" });
  assert.deepEqual(router.parseRoute("#/settings/checks"), { name: "settings", tab: "checks" });
  assert.ok(projectPage.TABS.some((tab) => tab.id === "tests" && tab.label === "Tests"));
  assert.deepEqual([...tests.SECTIONS.map((section) => section.id)], ["cases", "plans", "runs", "coverage"]);
  assert.equal(tests.runFromHash("#/projects/p1/tests?run=CA-R7"), "CA-R7");
  assert.equal(tests.runFromHash("#/projects/p1/tests?run=../x"), null, "only a run id opens a run");
  const settingsTabs = source("apps/desktop/src/screens/settings/index.tsx");
  assert.ok(settingsTabs.includes('{ id: "checks", label: "Checks"'));
  const projectSettings = source("apps/desktop/src/screens/project/settings.tsx");
  assert.ok(projectSettings.includes('{ id: "checks", label: "Checks" }'));
  const changes = source("apps/desktop/src/screens/changes/index.tsx");
  assert.ok(changes.includes("<ChangeSetChecks "), "the change set view shows its checks");
});
