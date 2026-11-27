import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { lstatSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { kindOf, testLines, testModules } from "./sleeps.mjs";

/** The allowlist of unguarded install functions, relative to the repository root. */
export const ALLOWLIST = "scripts/quality/engine-guard-allow.json";

/** The largest `git ls-files` listing read, far above the repository's size. */
const LISTING_BYTES_MAX = 64 * 1024 * 1024;

/** A file that names a sandbox type, the only files whose install calls are read. */
const SANDBOX_FILE = /\b(?:ProjectSandbox|InstalledSandbox|WindowsSandbox)\b/;

/** An install call: a method `install` or `install_with_grants`, or a sandbox type's own. */
const SITE =
  /\.install(?:_with_grants)?\s*\(|\b(?:InstalledSandbox|WindowsSandbox)::install\w*\s*\(/g;

/** A call that guards a sandbox against the engine root's secrets. */
const GUARD = /\bguard_engine(?:_around)?\s*\(/;

/** A Rust function's first line; groups: indent, name. */
const FN = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)/;

/** A comment line, by its trimmed start. */
const COMMENT = /^\s*(?:\/\/|\/\*|\*)/;

/** An allowlist guard that names a function: `path.rs#name`; any other text is a reason. */
const NAMED = /^([\w./-]+\.rs)#(\w+)$/;

/**
 * The functions of a Rust file: each `fn` line to the first later line that is only `}` at
 * its indent, or to itself for a declaration ending in `;`. Input: the lines.
 * Output: `{ name, start, end }` per function, 0-based and inclusive.
 */
function functions(lines) {
  const found = [];
  lines.forEach((line, start) => {
    const head = FN.exec(line);
    if (head === null || COMMENT.test(line)) {
      return;
    }
    const closer = new RegExp(`^\\s{${head[1].length}}\\}[;,)]*\\s*$`);
    let end = start;
    if (!line.trimEnd().endsWith(";")) {
      end = lines.findIndex((text, index) => index > start && closer.test(text));
      end = end === -1 ? lines.length - 1 : end;
    }
    assert.ok(end >= start && end < lines.length, `fn ${head[2]} ends in the file`);
    found.push({ name: head[2], start, end });
  });
  return found;
}

/**
 * Whether a function's body calls a guard, outside comment lines. Inputs: the lines and the
 * function. Output: the answer.
 */
function guards(lines, fn) {
  return lines.slice(fn.start, fn.end + 1).some((line) => !COMMENT.test(line) && GUARD.test(line));
}

/**
 * Finds the install calls in the non-test code of one Rust file that names a sandbox type,
 * each with the innermost function holding it and whether that function guards, and which
 * of its non-test functions guard. Inputs: the path, the text and whether a test-only `mod`
 * declares it. Output: `{ sites, guarded }`, sites as `{ line, fn, guarded }` with 1-based
 * lines, and `guarded` a map of function name to whether every function of that name guards.
 */
export function scan(file, text, whole = false) {
  assert.equal(typeof file, "string");
  assert.equal(typeof text, "string");
  if (kindOf(file) !== "rust" || !SANDBOX_FILE.test(text)) {
    return { sites: [], guarded: new Map() };
  }
  const lines = text.split("\n");
  const mask = testLines("rust", file, lines, whole) ?? lines.map(() => false);
  const fns = functions(lines).filter((fn) => !mask[fn.start]);
  const guarded = new Map();
  for (const fn of fns) {
    guarded.set(fn.name, (guarded.get(fn.name) ?? true) && guards(lines, fn));
  }
  const sites = [];
  lines.forEach((line, index) => {
    if (mask[index] || COMMENT.test(line)) {
      return;
    }
    for (const _ of line.matchAll(SITE)) {
      const holders = fns.filter((fn) => fn.start <= index && index <= fn.end);
      const fn = holders.reduce((inner, next) => (next.start > inner.start ? next : inner), {
        name: "?",
        start: -1,
        end: -1,
      });
      sites.push({ line: index + 1, fn: fn.name, guarded: fn.start >= 0 && guards(lines, fn) });
    }
  });
  assert.ok(sites.every((site) => site.line >= 1 && site.line <= lines.length));
  return { sites, guarded };
}

/**
 * Judges scan results against the allowlist of `path#fn` to the guards that cover it, which
 * may only shrink: an unguarded install function needs an entry; an entry needs a non-empty
 * list, each a reason or a `path.rs#fn` whose functions all guard; an entry whose function
 * no longer holds an unguarded install must go. Inputs: a map of path to its `scan` result,
 * and the allowlist. Output: one line per problem, sorted; empty when the tree holds it.
 */
export function check(found, allow) {
  assert.ok(found instanceof Map, "results come as a map of path to scan result");
  assert.ok(allow !== null && typeof allow === "object" && !Array.isArray(allow));
  const problems = [];
  const unguarded = new Map();
  for (const [file, result] of found) {
    for (const site of result.sites.filter((site) => !site.guarded)) {
      const key = `${file}#${site.fn}`;
      unguarded.set(key, [...(unguarded.get(key) ?? []), site.line]);
    }
  }
  for (const [key, lines] of unguarded) {
    if (!Object.hasOwn(allow, key)) {
      const [file, fn] = key.split("#");
      problems.push(
        `${file}:${lines.join(",")}: fn ${fn} installs a sandbox with no guard_engine; ` +
          `guard it against the engine root or name its guard in ${ALLOWLIST}`,
      );
    }
  }
  for (const [key, covers] of Object.entries(allow)) {
    if (!Array.isArray(covers) || covers.length === 0) {
      problems.push(`${key}: entry ${JSON.stringify(covers)} is not a list of guards`);
      continue;
    }
    if (!unguarded.has(key)) {
      problems.push(`${key}: holds no unguarded install; remove its entry`);
    }
    for (const cover of covers) {
      const named = typeof cover === "string" ? NAMED.exec(cover) : null;
      if (typeof cover !== "string" || cover.trim() === "") {
        problems.push(`${key}: guard ${JSON.stringify(cover)} is not a reason or a function`);
      } else if (named !== null && found.get(named[1])?.guarded.get(named[2]) === undefined) {
        problems.push(`${key}: guard ${cover} names no function`);
      } else if (named !== null && !found.get(named[1]).guarded.get(named[2])) {
        problems.push(`${key}: guard ${cover} holds no guard_engine`);
      }
    }
  }
  return problems.sort();
}

/**
 * Reads the allowlist of a repository. Input: the repository root.
 * Output: the object of `path#fn` to guards; throws when it is not a JSON object.
 */
function readAllowlist(root) {
  const allow = JSON.parse(readFileSync(path.join(root, ALLOWLIST), "utf8"));
  if (allow === null || typeof allow !== "object" || Array.isArray(allow)) {
    throw new Error(`${ALLOWLIST} must be a JSON object of path#fn to guards`);
  }
  return allow;
}

/**
 * Scans every tracked, regular Rust file of a repository and checks its install calls against
 * the allowlist; a file a test-only `mod` declares, and every file in that module's folder, is
 * test code whole. Input: the repository root. Output: the problems, as `check` gives them.
 */
export function run(root) {
  const options = { cwd: root, encoding: "utf8", maxBuffer: LISTING_BYTES_MAX };
  const texts = new Map();
  for (const file of execFileSync("git", ["ls-files", "-z"], options).split("\0")) {
    if (kindOf(file) !== "rust") {
      continue;
    }
    const full = path.join(root, file);
    let kind;
    try {
      kind = lstatSync(full);
    } catch {
      continue;
    }
    if (kind.isFile()) {
      texts.set(file, readFileSync(full, "utf8"));
    }
  }
  const modules = [];
  for (const [file, text] of texts) {
    modules.push(...testModules(file, text.split("\n")));
  }
  const found = new Map();
  for (const [file, text] of texts) {
    const inside = (module) => file === `${module}.rs` || file.startsWith(`${module}/`);
    const result = scan(file, text, modules.some(inside));
    if (result.guarded.size > 0 || result.sites.length > 0) {
      found.set(file, result);
    }
  }
  return check(found, readAllowlist(root));
}

/**
 * The command: checks the repository named by the first argument, or the one holding this
 * script. Prints each problem to stderr and exits 1, or prints one line and exits 0.
 */
function main() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(process.argv[2] ?? path.join(here, "..", ".."));
  const problems = run(root);
  if (problems.length > 0) {
    for (const line of problems) {
      process.stderr.write(`${line}\n`);
    }
    process.stderr.write(`${problems.length} unguarded sandbox install(s) (${ALLOWLIST})\n`);
    process.exit(1);
  }
  process.stdout.write("every sandbox install is guarded against the engine root\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
