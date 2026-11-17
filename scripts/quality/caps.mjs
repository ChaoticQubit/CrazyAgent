import { execFileSync } from "node:child_process";
import { lstatSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/**
 * The line cap of each capped file kind, by extension: Rust 1,500 lines; TypeScript and
 * JavaScript, `.mjs` included, 600 lines (2.5 plan section 9 item 2).
 */
export const CAPS = new Map([
  [".rs", 1500],
  [".ts", 600],
  [".tsx", 600],
  [".js", 600],
  [".mjs", 600],
]);

/** The ratchet allowlist, relative to the repository root. */
export const ALLOWLIST = "scripts/quality/caps-allow.json";

/**
 * The line cap of one file. Input: its repository-relative path.
 * Output: the cap, or null when no cap applies to its kind.
 */
export function capOf(file) {
  return CAPS.get(path.extname(file)) ?? null;
}

/**
 * The lines of a text as an editor shows them: one per line break, plus a last line
 * without one. Input: the text. Output: the count.
 */
export function lineCount(text) {
  let count = 0;
  for (const char of text) {
    if (char === "\n") {
      count += 1;
    }
  }
  return text.length > 0 && !text.endsWith("\n") ? count + 1 : count;
}

/**
 * Judges line counts against the caps and the ratchet allowlist, which may only shrink: a
 * file over its cap needs an entry, never grows past it, and an entry that is larger than
 * its file, names a file within its cap, an uncapped kind or no tracked file must go down.
 * Inputs: a map of tracked path to line count, and the allowlist object of path to count.
 * Output: one line per problem, sorted by path; empty when the tree holds the caps.
 */
export function check(counts, allow) {
  const problems = [];
  for (const [file, count] of counts) {
    const cap = capOf(file);
    if (cap !== null && count > cap && !Object.hasOwn(allow, file)) {
      problems.push(`${file}: ${count} lines, over the ${cap}-line cap; split it`);
    }
  }
  for (const [file, allowed] of Object.entries(allow)) {
    const cap = capOf(file);
    const count = counts.get(file);
    if (cap === null) {
      problems.push(`${file}: no line cap applies; remove its entry`);
    } else if (!Number.isInteger(allowed) || allowed <= cap) {
      problems.push(`${file}: entry ${JSON.stringify(allowed)} is not a line count over the cap`);
    } else if (count === undefined) {
      problems.push(`${file}: not a tracked file; remove its entry`);
    } else if (count > allowed) {
      problems.push(`${file}: ${count} lines, allowed at most ${allowed}; the allowlist only shrinks`);
    } else if (count <= cap) {
      problems.push(`${file}: ${count} lines is within the ${cap}-line cap; remove its entry`);
    } else if (count < allowed) {
      problems.push(`${file}: ${count} lines; lower its entry from ${allowed} to ${count}`);
    }
  }
  return problems.sort();
}

/**
 * Reads the allowlist of a repository. Input: the repository root.
 * Output: the object of path to count; throws when it is not a JSON object.
 */
function readAllowlist(root) {
  const allow = JSON.parse(readFileSync(path.join(root, ALLOWLIST), "utf8"));
  if (allow === null || typeof allow !== "object" || Array.isArray(allow)) {
    throw new Error(`${ALLOWLIST} must be a JSON object of path to line count`);
  }
  return allow;
}

/**
 * Checks every tracked, capped, regular file of a repository against the caps and its
 * allowlist; untracked files are never read. Input: the repository root.
 * Output: the problems, as `check` gives them.
 */
export function run(root) {
  const listed = execFileSync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8" });
  const counts = new Map();
  for (const file of listed.split("\0")) {
    if (file === "" || capOf(file) === null) {
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
      counts.set(file, lineCount(readFileSync(full, "utf8")));
    }
  }
  return check(counts, readAllowlist(root));
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
    process.stderr.write(
      `${problems.length} file line cap problem(s); caps: Rust 1500, TS/JS/MJS 600 (${ALLOWLIST})\n`,
    );
    process.exit(1);
  }
  process.stdout.write("file line caps hold\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
