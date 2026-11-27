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

/** An install call: a method `install` or `install_with_grants`, or a sandbox type's own. */
const SITE =
  /\.install(?:_with_grants)?\s*\(|\b(?:InstalledSandbox|WindowsSandbox)::install\w*\s*\(/g;

/** A call that guards a sandbox against the engine root's secrets. */
const GUARD = /\bguard_engine(?:_around)?\s*\(/;

/** A file that names a sandbox type, the only files whose install calls are read. */
const SANDBOX_FILE = /\b(?:ProjectSandbox|InstalledSandbox|WindowsSandbox)\b/;

/** The most entries the allowlist may hold; it only shrinks, so this only comes down. */
export const ALLOW_MAX = 8;

/** A Rust function's first line; groups: indent, name. */
const FN = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)/;

/** A comment line, by its trimmed start. */
const COMMENT = /^\s*(?:\/\/|\/\*|\*)/;

/** An allowlist guard that names a function: `path.rs#name`; any other text is a reason. */
const NAMED = /^([\w./-]+\.rs)#(\w+)$/;

/** A function a reason names, `path.rs#name`, which must still exist for the reason to hold. */
const CITED = /([\w./-]+\.rs)#(\w+)/g;

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
 * Whether a function calls a guard before a point in it, outside comment lines: on an earlier
 * line, or earlier on the point's own line. Inputs: the lines, the function, and the point's
 * line index and column, by default the function's end. Output: the answer.
 */
function guards(lines, fn, index = fn.end, column = Infinity) {
  assert.ok(fn.start <= index && index <= fn.end, "the point sits in the function");
  return lines.slice(fn.start, index + 1).some((line, at) => {
    const found = COMMENT.test(line) ? null : GUARD.exec(line);
    return found !== null && (fn.start + at < index || found.index < column);
  });
}

/**
 * Finds which non-test functions of one Rust file guard and, in a file that names a sandbox
 * type, its install calls, each with the innermost function holding it and whether that
 * function guards before the call. Inputs: the path, the text and whether a test-only `mod`
 * declares it. Output: `{ sites, guarded }`, sites as `{ line, fn, guarded }` with 1-based
 * lines, and `guarded` a map of function name to whether every function of that name guards.
 */
export function scan(file, text, whole = false) {
  assert.equal(typeof file, "string");
  assert.equal(typeof text, "string");
  if (kindOf(file) !== "rust") {
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
    if (mask[index] || COMMENT.test(line) || !SANDBOX_FILE.test(text)) {
      return;
    }
    for (const call of line.matchAll(SITE)) {
      const holders = fns.filter((fn) => fn.start <= index && index <= fn.end);
      const fn = holders.reduce((inner, next) => (next.start > inner.start ? next : inner), {
        name: "?",
        start: -1,
        end: -1,
      });
      const guarded = fn.start >= 0 && guards(lines, fn, index, call.index);
      sites.push({ line: index + 1, fn: fn.name, guarded });
    }
  });
  assert.ok(sites.every((site) => site.line >= 1 && site.line <= lines.length));
  return { sites, guarded };
}

/**
 * Judges scan results against the allowlist of `path#fn` to `{ sites, guards }`, at most
 * `ALLOW_MAX` entries: an unguarded install function needs an entry pinning its count of
 * unguarded installs; each guard is a `path.rs#fn` whose functions all guard, or a reason
 * naming a `path.rs#fn` that still exists. Inputs: a map of path to its `scan` result, and
 * the allowlist. Output: one line per problem, sorted; empty when the tree holds it.
 */
export function check(found, allow) {
  assert.ok(found instanceof Map, "results come as a map of path to scan result");
  assert.ok(allow !== null && typeof allow === "object" && !Array.isArray(allow));
  const problems = [];
  const entries = Object.keys(allow).length;
  if (entries > ALLOW_MAX) {
    const over = `${entries} entries, at most ${ALLOW_MAX}`;
    problems.push(`${ALLOWLIST}: ${over}; the allowlist only shrinks`);
  }
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
  for (const [key, entry] of Object.entries(allow)) {
    const covers = entry?.guards;
    const pinned = entry?.sites;
    const shaped = Number.isInteger(pinned) && pinned >= 1 && Array.isArray(covers);
    if (!shaped || covers.length === 0 || Object.keys(entry).length !== 2) {
      problems.push(`${key}: entry ${JSON.stringify(entry)} is not { sites, guards }`);
      continue;
    }
    const count = unguarded.get(key)?.length ?? 0;
    if (count === 0) {
      problems.push(`${key}: holds no unguarded install; remove its entry`);
    } else if (count !== pinned) {
      const lines = unguarded.get(key).join(",");
      problems.push(`${key}: ${count} unguarded installs (lines ${lines}), entry pins ${pinned}`);
    }
    for (const cover of covers) {
      const named = typeof cover === "string" ? NAMED.exec(cover) : null;
      if (typeof cover !== "string" || cover.trim() === "") {
        problems.push(`${key}: guard ${JSON.stringify(cover)} is not a reason or a function`);
      } else if (named !== null && found.get(named[1])?.guarded.get(named[2]) === undefined) {
        problems.push(`${key}: guard ${cover} names no function`);
      } else if (named !== null && !found.get(named[1]).guarded.get(named[2])) {
        problems.push(`${key}: guard ${cover} holds no guard_engine`);
      } else if (named === null) {
        const cited = Array.from(cover.matchAll(CITED));
        const gone = cited.filter(([, file, fn]) => !found.get(file)?.guarded.has(fn));
        if (cited.length === 0 || gone.length > 0) {
          const names = gone.map(([name]) => name).join(", ") || "none";
          problems.push(`${key}: reason names no function that still exists (${names})`);
        }
      }
    }
  }
  return problems.sort();
}

/**
 * Reads the allowlist of a repository. Input: the repository root.
 * Output: the object of `path#fn` to entries; throws when it is not a JSON object.
 */
function readAllowlist(root) {
  const allow = JSON.parse(readFileSync(path.join(root, ALLOWLIST), "utf8"));
  if (allow === null || typeof allow !== "object" || Array.isArray(allow)) {
    throw new Error(`${ALLOWLIST} must be a JSON object of path#fn to { sites, guards }`);
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
    process.stderr.write(`${problems.length} engine guard problem(s) (${ALLOWLIST})\n`);
    process.exit(1);
  }
  process.stdout.write("every sandbox install is guarded against the engine root\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
