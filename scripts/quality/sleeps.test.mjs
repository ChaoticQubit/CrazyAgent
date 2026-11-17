import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { check, run, scan } from "./sleeps.mjs";

const script = fileURLToPath(new URL("./sleeps.mjs", import.meta.url));
const THREAD = ["thread", "sleep"].join("::");
const TOKIO = ["tokio", "time", "sleep"].join("::");
const TIMER = ["set", "Timeout"].join("");
const NONE = { test: false, lines: [], helpers: [] };

/**
 * One awaited call of a named sleep, built from parts so this file holds no sleep itself.
 * Input: the callee. Output: the statement.
 */
function awaited(callee) {
  return ["await", `${callee}(5);`].join(" ");
}

/**
 * A scan result for a test file with sleeps on lines 1 to `count`.
 * Input: the count. Output: the result `check` reads.
 */
function sleepsOn(count) {
  const lines = Array.from({ length: count }, (_, index) => index + 1);
  return { test: true, lines, helpers: [] };
}

/**
 * A throwaway git repository holding the given files, all tracked.
 * Input: a map of relative path to text. Output: the repository folder.
 */
function repository(files) {
  const root = mkdtempSync(path.join(tmpdir(), "sleeps-"));
  execFileSync("git", ["init", "-q"], { cwd: root });
  for (const [name, text] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  execFileSync("git", ["add", "-A"], { cwd: root });
  return root;
}

test("a planted thread or tokio sleep in a Rust test file fails; without it, it passes", () => {
  const file = "crates/a/src/tests.rs";
  const text = `async fn waits() {\n    std::${THREAD}(D);\n    ${TOKIO}(D).await;\n}\n`;
  const found = scan(file, text);
  assert.deepEqual(found.lines, [2, 3]);
  assert.deepEqual(check(new Map([[file, found]]), {}), [
    `${file}: 2 new sleeps in test code (lines 2, 3); ` +
      "wait on an event or the one shared wait_until helper",
  ]);
  assert.deepEqual(check(new Map([[file, scan(file, "fn waits() {}\n")]]), {}), []);
});

test("Rust test code is a test file by name or folder, or a cfg(test) item", () => {
  const body = `    std::${THREAD}(D);`;
  const fn = ["fn f() {", body, "}"].join("\n");
  const files = [
    "crates/a/tests/flow.rs",
    "crates/a/src/x/tests.rs",
    "crates/a/src/x_tests.rs",
    "crates/a/src/engine_tests/job.rs",
  ];
  for (const file of files) {
    assert.deepEqual(scan(file, fn).lines, [2], file);
  }
  assert.deepEqual(scan("crates/a/src/lib.rs", fn), NONE);
  const inline = [
    "fn production() {",
    body,
    "}",
    "#[cfg(test)]",
    "mod tests {",
    "    fn f() {",
    `    ${body}`,
    "    }",
    "}",
    "#[cfg(all(unix, not(test)))]",
    "fn live() {",
    body,
    "}",
    '#[cfg(any(test, feature = "x"))]',
    "fn helper() {",
    body,
    "}",
    "#[cfg(not(any(test, debug_assertions)))]",
    "fn release() {",
    body,
    "}",
  ].join("\n");
  const found = scan("crates/a/src/lib.rs", inline);
  assert.deepEqual(found, { test: true, lines: [7, 16], helpers: [] });
});

test("a Rust sleep imported bare or renamed counts at each call; a local one does not", () => {
  const renamed = [
    `use std::${THREAD} as nap;`,
    "fn f() {",
    "    nap(D);",
    "    sleep(D);",
    "    self.nap(D);",
    "}",
  ].join("\n");
  assert.deepEqual(scan("crates/a/tests/t.rs", renamed).lines, [3]);
  const braced = [
    "use tokio::time::{Duration, " + "sleep};",
    "async fn f() {",
    "    sleep(D).await;",
    "    clock.sleep(D);",
    "}",
  ].join("\n");
  assert.deepEqual(scan("crates/a/tests/t.rs", braced).lines, [3]);
  const local = "fn sleep(d: u64) {}\nfn f() {\n    sleep(D);\n}\n";
  assert.deepEqual(scan("crates/a/tests/t.rs", local).lines, []);
});

test("each listed JS sleep counts and a timer that does not wait does not", () => {
  const text = [
    awaited("sleep"),
    awaited("pause"),
    awaited("delay"),
    awaited(TIMER),
    awaited(`timers.${TIMER}`),
    `await new Promise((done) => ${TIMER}(done, 5))`,
    "await new Promise((done) => {",
    `  ${TIMER}(done, 5)`,
    "})",
    `${TIMER}(() => controller.abort(), 5)`,
    "await new Promise((done) => server.close(done))",
  ].join("\n");
  assert.deepEqual(scan("apps/desktop/test/a.test.mjs", text).lines, [1, 2, 3, 4, 5, 6, 8]);
});

test("a named wrapper or a timers alias counts at each call; another object's does not", () => {
  const wrapped = [
    `import { ${TIMER} as nap } from "node:timers/promises";`,
    "await nap(5);",
    "nap(5).then(done);",
    "snap(5);",
    "other.nap(5);",
    `const pause = (ms) => new Promise((done) => ${TIMER}(done, ms));`,
    awaited("pause"),
    awaited("pause"),
  ].join("\n");
  assert.deepEqual(scan("scripts/a.test.mjs", wrapped).lines, [2, 3, 6, 7, 8]);
  const destructured = [
    `const { ${TIMER}: wait } = await import("node:timers/promises");`,
    "await wait(5);",
  ].join("\n");
  assert.deepEqual(scan("apps/x/test/b.mjs", destructured).lines, [2]);
});

test("JS test code is a script under a test folder or named test, spec or e2e", () => {
  const text = awaited("sleep");
  const files = [
    "apps/desktop/test/a.mjs",
    "apps/x/tests/a.ts",
    "apps/x/src/__tests__/a.tsx",
    "scripts/a.test.mjs",
    "apps/x/src/a.spec.ts",
    "apps/desktop/test/native/a.e2e.mjs",
    "skills/x/a.test.cjs",
  ];
  for (const file of files) {
    assert.deepEqual(scan(file, text).lines, [1], file);
  }
  assert.deepEqual(scan("apps/desktop/src/screens/a.ts", text), NONE);
  assert.deepEqual(scan("apps/desktop/test/fixture.json", text), NONE);
});

test("comment lines never count", () => {
  const js = [
    `// ${awaited("sleep")}`,
    ` * ${awaited("sleep")}`,
    `/* ${awaited("sleep")}`,
    awaited("sleep"),
  ].join("\n");
  assert.deepEqual(scan("apps/desktop/test/a.mjs", js).lines, [4]);
  const rust = [
    `/// std::${THREAD}(D)`,
    "fn f() {",
    `    // std::${THREAD}(D)`,
    `    std::${THREAD}(D);`,
    "}",
  ].join("\n");
  assert.deepEqual(scan("crates/a/tests/t.rs", rust).lines, [4]);
});

test("a sleep inside the one wait_until helper does not count", () => {
  const rust = [
    "pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {",
    "    for _ in 0..100 {",
    "        if done() {",
    "            return;",
    "        }",
    `        std::${THREAD}(STEP);`,
    "    }",
    '    panic!("{what}");',
    "}",
    "fn other() {",
    `    std::${THREAD}(STEP);`,
    "}",
  ].join("\n");
  const support = scan("crates/a/tests/support/mod.rs", rust);
  assert.deepEqual(support, { test: true, lines: [11], helpers: [1] });
  const js = [
    ["export async function", "waitUntil(check, limitMs = 5000) {"].join(" "),
    "  const end = Date.now() + limitMs;",
    "  while (!(await check())) {",
    '    assert.ok(Date.now() < end, "waited too long");',
    `    await new Promise((done) => ${TIMER}(done, 20));`,
    "  }",
    "}",
    awaited("pause"),
  ].join("\n");
  const shared = scan("apps/desktop/test/wait.mjs", js);
  assert.deepEqual(shared, { test: true, lines: [8], helpers: [1] });
});

test("a second wait_until helper in one language fails", () => {
  const helper = (line) => ({ test: true, lines: [], helpers: [line] });
  const found = new Map([
    ["crates/a/tests/one.rs", helper(3)],
    ["crates/b/tests/two.rs", helper(9)],
    ["apps/desktop/test/wait.mjs", helper(1)],
  ]);
  assert.deepEqual(check(found, {}), [
    "wait_until is defined more than once in Rust test code " +
      "(crates/a/tests/one.rs:3, crates/b/tests/two.rs:9); keep one shared helper",
  ]);
  const both = { test: true, lines: [], helpers: [1, 9] };
  const twice = new Map([["apps/desktop/test/wait.mjs", both]]);
  assert.deepEqual(check(twice, {}), [
    "waitUntil is defined more than once in JS test code " +
      "(apps/desktop/test/wait.mjs:1, apps/desktop/test/wait.mjs:9); keep one shared helper",
  ]);
});

test("an allowlisted file may keep its sleeps but never add one", () => {
  const allow = { "crates/a/tests/t.rs": 3 };
  assert.deepEqual(check(new Map([["crates/a/tests/t.rs", sleepsOn(3)]]), allow), []);
  assert.deepEqual(check(new Map([["crates/a/tests/t.rs", sleepsOn(4)]]), allow), [
    "crates/a/tests/t.rs: 4 sleeps in test code (lines 1, 2, 3, 4), allowed at most 3; " +
      "the allowlist only shrinks",
  ]);
});

test("the allowlist only shrinks: lower, empty, untracked and non-test entries must change", () => {
  const file = "crates/a/tests/t.rs";
  const allow = { [file]: 3 };
  assert.deepEqual(check(new Map([[file, sleepsOn(2)]]), allow), [
    `${file}: 2 sleeps in test code; lower its entry from 3 to 2`,
  ]);
  assert.deepEqual(check(new Map([[file, sleepsOn(0)]]), allow), [
    `${file}: no sleeps left in test code; remove its entry`,
  ]);
  assert.deepEqual(check(new Map(), allow), [`${file}: not a tracked test file; remove its entry`]);
  const lib = "crates/a/src/lib.rs";
  assert.deepEqual(check(new Map([[lib, NONE]]), { [lib]: 1 }), [
    `${lib}: not a tracked test file; remove its entry`,
  ]);
});

test("an allowlist entry must be a whole count of at least one", () => {
  const file = "crates/a/tests/t.rs";
  for (const count of [0, -1, 1.5, "3", null]) {
    assert.deepEqual(
      check(new Map([[file, sleepsOn(3)]]), { [file]: count }),
      [`${file}: entry ${JSON.stringify(count)} is not a sleep count`],
      String(count),
    );
  }
});

test("the check reads every tracked file of a repository and fails on a planted sleep", () => {
  const sleeping = `fn f() {\n    std::${THREAD}(D);\n}\n`;
  const root = repository({
    "crates/a/tests/flow.rs": sleeping,
    "crates/a/src/lib.rs": sleeping,
    "apps/desktop/test/ok.test.mjs": "export const ok = 1;\n",
    "scripts/quality/sleeps-allow.json": "{}\n",
  });
  try {
    writeFileSync(path.join(root, "crates/a/tests/untracked.rs"), sleeping);
    assert.deepEqual(run(root), [
      "crates/a/tests/flow.rs: 1 new sleep in test code (line 2); " +
        "wait on an event or the one shared wait_until helper",
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the command exits 1 with the problems on a planted sleep and 0 on a clean tree", () => {
  const allow = { "scripts/quality/sleeps-allow.json": "{}\n" };
  const planted = repository({ "apps/desktop/test/a.mjs": `${awaited("sleep")}\n`, ...allow });
  const clean = repository({ "apps/desktop/test/a.mjs": "export const ok = 1;\n", ...allow });
  const pipes = { stdio: ["ignore", "pipe", "pipe"] };
  try {
    assert.throws(
      () => execFileSync(process.execPath, [script, planted], pipes),
      (error) =>
        error.status === 1 &&
        String(error.stderr).includes("apps/desktop/test/a.mjs: 1 new sleep in test code (line 1)"),
    );
    const output = execFileSync(process.execPath, [script, clean], { ...pipes, encoding: "utf8" });
    assert.equal(output, "no new sleeps in test code\n");
  } finally {
    rmSync(planted, { recursive: true, force: true });
    rmSync(clean, { recursive: true, force: true });
  }
});
