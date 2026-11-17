import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { capOf, check, lineCount, run } from "./caps.mjs";

const script = fileURLToPath(new URL("./caps.mjs", import.meta.url));

/**
 * Text of exactly `count` lines, each ending in a line break.
 * Input: the line count. Output: the text.
 */
function lines(count) {
  return "x\n".repeat(count);
}

/**
 * A throwaway git repository holding the given files, all tracked.
 * Input: a map of relative path to text. Output: the repository folder.
 */
function repository(files) {
  const root = mkdtempSync(path.join(tmpdir(), "caps-"));
  execFileSync("git", ["init", "-q"], { cwd: root });
  for (const [name, text] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  execFileSync("git", ["add", "-A"], { cwd: root });
  return root;
}

test("a planted 1,501-line Rust file fails and 1,500 lines pass", () => {
  assert.deepEqual(check(new Map([["src/a.rs", 1500]]), {}), []);
  const problems = check(new Map([["src/a.rs", 1501]]), {});
  assert.equal(problems.length, 1);
  assert.match(problems[0], /^src\/a\.rs: 1501 lines, over the 1500-line cap/);
});

test("a TS, TSX, JS or MJS file over 600 lines fails and a CJS file is not capped", () => {
  for (const name of ["a.ts", "a.tsx", "a.js", "a.mjs"]) {
    assert.deepEqual(check(new Map([[name, 600]]), {}), [], name);
    assert.equal(check(new Map([[name, 601]]), {}).length, 1, name);
  }
  assert.equal(capOf("a.cjs"), null);
  assert.deepEqual(check(new Map([["a.cjs", 5000]]), {}), []);
});

test("an allowlisted file may stay at its count but never grow", () => {
  const allow = { "src/big.rs": 2000 };
  assert.deepEqual(check(new Map([["src/big.rs", 2000]]), allow), []);
  const grown = check(new Map([["src/big.rs", 2001]]), allow);
  assert.equal(grown.length, 1);
  assert.match(grown[0], /allowed at most 2000/);
});

test("the allowlist only shrinks: a smaller, capped, missing or uncapped entry fails", () => {
  const allow = { "src/big.rs": 2000 };
  assert.match(check(new Map([["src/big.rs", 1800]]), allow)[0], /lower its entry from 2000 to 1800/);
  assert.match(check(new Map([["src/big.rs", 1500]]), allow)[0], /within the 1500-line cap; remove its entry/);
  assert.match(check(new Map(), allow)[0], /^src\/big\.rs: not a tracked file; remove its entry/);
  assert.match(check(new Map([["a.cjs", 900]]), { "a.cjs": 900 })[0], /no line cap applies/);
});

test("an allowlist entry must be a whole count over the cap", () => {
  for (const count of [0, -1, 1.5, "2000", 1500]) {
    const problems = check(new Map([["src/big.rs", 2000]]), { "src/big.rs": count });
    assert.ok(problems.some((line) => line.includes("not a line count over the cap")), String(count));
  }
});

test("lines are counted as an editor shows them", () => {
  assert.equal(lineCount(""), 0);
  assert.equal(lineCount("a"), 1);
  assert.equal(lineCount("a\n"), 1);
  assert.equal(lineCount("a\nb"), 2);
  assert.equal(lineCount("a\r\nb\r\n"), 2);
});

test("the check reads every tracked file of a repository and fails on a planted file", () => {
  const root = repository({
    "crates/a/src/lib.rs": lines(1501),
    "apps/desktop/src/ok.ts": lines(600),
    "scripts/quality/caps-allow.json": "{}\n",
  });
  try {
    writeFileSync(path.join(root, "untracked.rs"), lines(5000));
    const report = run(root);
    assert.deepEqual(report, ["crates/a/src/lib.rs: 1501 lines, over the 1500-line cap; split it"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the command exits 1 with the problems on a planted file and 0 on a clean tree", () => {
  const planted = repository({ "a.mjs": lines(601), "scripts/quality/caps-allow.json": "{}\n" });
  const clean = repository({ "a.mjs": lines(600), "scripts/quality/caps-allow.json": "{}\n" });
  try {
    assert.throws(
      () => execFileSync(process.execPath, [script, planted], { stdio: ["ignore", "pipe", "pipe"] }),
      (error) => error.status === 1 && String(error.stderr).includes("a.mjs: 601 lines"),
    );
    execFileSync(process.execPath, [script, clean], { stdio: ["ignore", "pipe", "pipe"] });
  } finally {
    rmSync(planted, { recursive: true, force: true });
    rmSync(clean, { recursive: true, force: true });
  }
});
