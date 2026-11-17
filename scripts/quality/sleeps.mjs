import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { lstatSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** The ratchet allowlist of sleep sites per test file, relative to the repository root. */
export const ALLOWLIST = "scripts/quality/sleeps-allow.json";

/** The most site lines one problem names, so a file with many sleeps keeps a short line. */
const SHOWN_LINES_MAX = 20;

/** The largest `git ls-files` listing read, far above the repository's size. */
const LISTING_BYTES_MAX = 64 * 1024 * 1024;

/** What a new sleep should become instead, named in each new-sleep problem. */
const ADVICE = "wait on an event or the one shared wait_until helper";

/**
 * Raw sleep calls by kind; each pattern's first group is the callee, where the site sits.
 * Rust: `thread::sleep(`, `time::sleep(`, `time::sleep_until(`. JS: an awaited `sleep`,
 * `pause`, `delay` or `setTimeout` call, bare or on one object, and a `new Promise(` whose
 * statement reaches `setTimeout(` within 200 characters.
 */
const RAW = {
  rust: [/\bthread::(sleep)\s*\(/dg, /\btime::(sleep(?:_until)?)\s*\(/dg],
  js: [
    /\bawait\s+(?:[\w$]+\.)?(sleep|pause|delay|setTimeout)\s*\(/dg,
    /\bnew\s+Promise\s*\([^;]{0,200}?\b(setTimeout)\s*\(/dg,
  ],
};

/** A Rust `use` of `std::thread` or `tokio::time` sleep items, braced or renamed. */
const RUST_USE = /\buse\s+(?:std::thread|tokio::time)::(\{[^}]*\}|sleep[\w\s]*);/g;

/** One imported Rust sleep item; the group is its local name when renamed. */
const RUST_ITEM = /\bsleep(?:_until)?\b(?:\s+as\s+(\w+))?/g;

/** A JS local name for `setTimeout`: `setTimeout as X` or `{ setTimeout: X }`. */
const JS_ALIAS = /\bsetTimeout\s+as\s+([\w$]+)|\{[^}]*?\bsetTimeout\s*:\s*([\w$]+)/g;

/** The one shared helper's definition line by kind; the group is its indent. */
const HELPER = {
  rust: /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+wait_until\b/,
  js: /^(\s*)(?:export\s+)?(?:async\s+)?(?:function|const|let)\s+waitUntil\b/,
};

/** The helper's name and the language's name by kind, as problems print them. */
const LANGUAGE = {
  rust: { helper: "wait_until", name: "Rust" },
  js: { helper: "waitUntil", name: "JS" },
};

/** A Rust `#[cfg(...)]` attribute on its own line; groups: indent, condition. */
const CFG = /^(\s*)#\[cfg\((.*)\)\]\s*$/;

/** The deepest cfg nesting read, far past any condition in the tree (three levels). */
const CFG_DEPTH_MAX = 16;

/** A Rust module declared without a body; the group is its name. */
const MOD = /^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;/;

/** A top-level `#[path = "...rs"]` attribute; the group is the path from the file's folder. */
const PATH_ATTRIBUTE = /^#\[path\s*=\s*"([^"]+\.rs)"\]\s*$/;

/** A line at an item's indent that still belongs to it: a closer, a brace, where, ->, else. */
const CONTINUES = /^(?:[}\])]|\{|where\b|->|else\b)/;

/** A comment line, by its trimmed start. */
const COMMENT = /^\s*(?:\/\/|\/\*|\*)/;

/**
 * The kind of a tracked path. Input: its repository-relative path.
 * Output: "rust", "js" for JS and TS scripts, or null when the check never reads it.
 */
function kindOf(file) {
  if (file.endsWith(".rs")) {
    return "rust";
  }
  return /\.(?:[cm]?[jt]s|[jt]sx)$/.test(file) ? "js" : null;
}

/**
 * Whether a whole file is test code by its path: Rust under a `tests/` or `*_tests/` folder or
 * named `tests.rs` or `*_tests.rs`; JS under `test/`, `tests/`, `__tests__/` or named
 * `.test.`, `.spec.` or `.e2e.`. Inputs: the kind and path. Output: the answer.
 */
function testPath(kind, file) {
  assert.ok(kind === "rust" || kind === "js", `no kind for ${file}`);
  if (kind === "rust") {
    return /(?:^|\/)(?:tests|\w+_tests)(?:\/|\.rs$)/.test(file);
  }
  return /(?:^|\/)(?:test|tests|__tests__)\/|\.(?:test|spec|e2e)\.[^/]+$/.test(file);
}

/**
 * The value of one cfg predicate with `test` false and every other predicate unknown: false,
 * true, or null for unknown. Inputs: the condition's tokens, the predicate's first token and
 * its nesting depth, at most `CFG_DEPTH_MAX`.
 * Output: `[value, index of the token after the predicate]`.
 */
function cfgValue(tokens, at, depth) {
  assert.ok(depth <= CFG_DEPTH_MAX, `a cfg condition nests past ${CFG_DEPTH_MAX} levels`);
  const token = tokens[at];
  if (!["all", "any", "not"].includes(token) || tokens[at + 1] !== "(") {
    return [token === "test" ? false : null, at + 1];
  }
  const values = [];
  let index = at + 2;
  while (index < tokens.length && tokens[index] !== ")") {
    const [value, next] = cfgValue(tokens, index, depth + 1);
    assert.ok(next > index, "each predicate takes at least one token");
    values.push(value);
    index = tokens[next] === "," ? next + 1 : next;
  }
  if (token === "not") {
    return [values[0] === null || values[0] === undefined ? null : !values[0], index + 1];
  }
  const settles = token === "all" ? false : true;
  if (values.includes(settles)) {
    return [settles, index + 1];
  }
  return [values.every((value) => value === !settles) ? !settles : null, index + 1];
}

/**
 * Whether a cfg condition compiles its item only for tests: it is false whenever `test` is
 * false, whatever the other predicates are. `all(test, unix)` is; `any(unix, test)` and
 * `not(test)` are not. Input: the condition. Output: the answer.
 */
function testOnly(condition) {
  const tokens = condition.match(/\w+(?:\s*=\s*"[^"]*")?|[(),]/g) ?? [];
  return tokens.length > 0 && cfgValue(tokens, 0, 0)[0] === false;
}

/**
 * The last line of a block opened at a line: the first later line that is only `}` at the
 * opener's indent, closing punctuation allowed after it; the last line when none closes it.
 * Inputs: the lines, the opener's index and its indent. Output: the closing line's index.
 */
function blockEnd(lines, open, indent) {
  const closer = new RegExp(`^\\s{${indent}}\\}[;,)]*$`);
  for (let index = open + 1; index < lines.length; index += 1) {
    if (closer.test(lines[index].trimEnd())) {
      return index;
    }
  }
  return lines.length - 1;
}

/**
 * The last line of the item starting at a line: the line itself when it ends in `;` or `,`;
 * a block's closer once a line at the item's indent opens one; otherwise the last line before
 * the next line at that indent that does not continue the item, or at a smaller indent.
 * Inputs: the lines, the item's index and indent. Output: the last line's index.
 */
function itemEnd(lines, start, indent) {
  let end = start;
  for (let index = start; index < lines.length; index += 1) {
    const text = lines[index].trimEnd();
    const depth = text.length - text.trimStart().length;
    if (index > start && text !== "") {
      if (depth < indent || (depth === indent && !CONTINUES.test(text.trimStart()))) {
        break;
      }
    }
    end = text === "" ? end : index;
    if (depth === indent && text.endsWith("{")) {
      end = blockEnd(lines, index, indent);
      break;
    }
    if (index === start && /[;,]$/.test(text)) {
      break;
    }
  }
  assert.ok(end >= start && end < lines.length, `item at line ${start + 1} ends in the file`);
  return end;
}

/**
 * The line of the item a test-only `#[cfg(...)]` attribute sits on, past further attributes
 * and comments. Inputs: the lines and a line's index.
 * Output: the item's index, `lines.length` when the file ends first, or null when the line is
 * no test-only cfg attribute.
 */
function testItem(lines, index) {
  const attribute = CFG.exec(lines[index]);
  if (attribute === null || !testOnly(attribute[2])) {
    return null;
  }
  let item = index + 1;
  while (item < lines.length && /^\s*(?:#\[|\/\/)/.test(lines[item])) {
    item += 1;
  }
  assert.ok(item > index && item <= lines.length);
  return item;
}

/**
 * The module paths a Rust file declares only for tests, as `mod name;` under a test-only cfg:
 * beside the file by Rust's rules, or where a top-level `#[path]` names, from the file's
 * folder; an indented `#[path]` is not followed. Inputs: the file's path and lines.
 * Output: each module's path without extension; its `.rs` file and folder are test code.
 */
function testModules(file, lines) {
  assert.ok(file.endsWith(".rs"), `${file} is no Rust file`);
  const base = path.posix.basename(file);
  const roots = ["mod.rs", "lib.rs", "main.rs"];
  const folder = roots.includes(base) ? path.posix.dirname(file) : file.slice(0, -".rs".length);
  const modules = [];
  for (let index = 0; index < lines.length; index += 1) {
    const item = testItem(lines, index);
    const declared = item === null ? null : MOD.exec(lines[item] ?? "");
    if (declared === null) {
      continue;
    }
    const attributes = lines.slice(index, item);
    const named = attributes.map((line) => PATH_ATTRIBUTE.exec(line)).find(Boolean);
    if (named !== undefined) {
      modules.push(path.posix.join(path.posix.dirname(file), named[1]).slice(0, -".rs".length));
    } else if (!attributes.some((line) => line.includes("#[path"))) {
      modules.push(path.posix.join(folder, declared[1]));
    }
  }
  return modules;
}

/**
 * Marks the lines of each Rust item under a test-only `#[cfg(...)]` (`testOnly`), from the
 * attribute to the item's end (`itemEnd`). Input: the lines.
 * Output: one flag per line, true for test code.
 */
function inlineTestLines(lines) {
  const mask = new Array(lines.length).fill(false);
  for (let index = 0; index < lines.length; index += 1) {
    const item = testItem(lines, index);
    if (item === null) {
      continue;
    }
    const attribute = CFG.exec(lines[index]);
    const last = lines.length - 1;
    const end = item <= last ? itemEnd(lines, item, attribute[1].length) : last;
    assert.ok(end >= index, "a cfg(test) region never ends before its attribute");
    mask.fill(true, index, end + 1);
    index = end;
  }
  return mask;
}

/**
 * Names whose every call is a sleep site in one file: Rust items imported from `std::thread`
 * or `tokio::time`, braced or renamed; JS local names for `setTimeout`.
 * Inputs: the kind and text. Output: the set of names.
 */
function aliasesOf(kind, text) {
  const names = new Set();
  if (kind === "rust") {
    for (const use of text.matchAll(RUST_USE)) {
      for (const item of use[1].matchAll(RUST_ITEM)) {
        names.add(item[1] ?? item[0]);
      }
    }
  } else {
    for (const alias of text.matchAll(JS_ALIAS)) {
      names.add(alias[1] ?? alias[2]);
    }
  }
  names.delete("_");
  return names;
}

/**
 * The offsets of each call of a name that is not a method, a path segment or a definition.
 * Inputs: the text and the name. Output: the offsets of the name at each call.
 */
function callsOf(text, name) {
  assert.match(name, /^[\w$]+$/, "a name is one identifier");
  const escaped = name.replaceAll("$", "\\$");
  const call = new RegExp(`(?<![\\w$.:])(?<!\\b(?:fn|function)\\s+)${escaped}\\s*\\(`, "g");
  return Array.from(text.matchAll(call), (match) => match.index);
}

/**
 * The test-code flags of a file's lines: all of a test file or a test-only module, the
 * test-only cfg items of another Rust file, nothing of other JS. Inputs: the kind, path, lines
 * and whether a test-only `mod` declares the file. Output: one flag per line, or null when the
 * file holds no test code.
 */
function testLines(kind, file, lines, whole) {
  if (kind === null || (kind === "js" && !testPath(kind, file))) {
    return null;
  }
  const all = whole || testPath(kind, file);
  assert.ok(!whole || kind === "rust", "only a Rust module is declared for tests");
  const mask = all ? lines.map(() => true) : inlineTestLines(lines);
  return mask.some(Boolean) ? mask : null;
}

/**
 * The line index of an offset. Inputs: the ascending start offset of each line, and an
 * offset inside the text. Output: the index of the line holding it.
 */
function lineAt(starts, offset) {
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (starts[middle] <= offset) {
      low = middle;
    } else {
      high = middle - 1;
    }
  }
  assert.ok(starts[low] <= offset, "an offset sits on a line");
  return low;
}

/**
 * Finds the sleep sites in the test code of one file: raw calls and calls of local aliases,
 * once per callee, outside comment lines and outside the body of a `wait_until` helper.
 * Inputs: the repository-relative path, the text, and whether a test-only `mod` declares it.
 * Output: `{ test, lines, helpers }`, with one ascending 1-based line per site and the line of
 * each helper definition.
 */
export function scan(file, text, whole = false) {
  assert.equal(typeof file, "string");
  assert.equal(typeof text, "string");
  const kind = kindOf(file);
  const lines = text.split("\n");
  const mask = testLines(kind, file, lines, whole);
  if (mask === null) {
    return { test: false, lines: [], helpers: [] };
  }
  const bodies = [];
  lines.forEach((line, index) => {
    const definition = HELPER[kind].exec(line);
    if (definition !== null && mask[index]) {
      bodies.push([index, itemEnd(lines, index, definition[1].length)]);
    }
  });
  const offsets = new Set();
  for (const pattern of RAW[kind]) {
    for (const match of text.matchAll(pattern)) {
      offsets.add(match.indices[1][0]);
    }
  }
  for (const name of aliasesOf(kind, text)) {
    callsOf(text, name).forEach((offset) => offsets.add(offset));
  }
  const starts = [];
  let start = 0;
  for (const line of lines) {
    starts.push(start);
    start += line.length + 1;
  }
  const sites = [];
  for (const offset of offsets) {
    const index = lineAt(starts, offset);
    const helper = bodies.some(([from, to]) => index >= from && index <= to);
    if (mask[index] && !helper && !COMMENT.test(lines[index])) {
      sites.push(index + 1);
    }
  }
  sites.sort((left, right) => left - right);
  assert.ok(sites.every((line) => line >= 1 && line <= lines.length), "sites sit in the file");
  return { test: true, lines: sites, helpers: bodies.map(([from]) => from + 1) };
}

/**
 * A count of sleeps as words. Inputs: the count and an optional adjective with its trailing
 * space. Output: for example "1 new sleep" or "4 sleeps".
 */
function sleeps(count, adjective = "") {
  return `${count} ${adjective}sleep${count === 1 ? "" : "s"}`;
}

/**
 * The site lines a problem names, at most `SHOWN_LINES_MAX`. Input: the 1-based lines.
 * Output: for example "line 2" or "lines 2, 3".
 */
function shown(lines) {
  assert.ok(lines.length > 0, "a problem names at least one line");
  const named = lines.slice(0, SHOWN_LINES_MAX).join(", ");
  const more = lines.length > SHOWN_LINES_MAX ? `, and ${lines.length - SHOWN_LINES_MAX} more` : "";
  return `${lines.length === 1 ? "line" : "lines"} ${named}${more}`;
}

/**
 * Judges scan results against the ratchet allowlist, which may only shrink: a test file with
 * sleeps needs an entry and never grows past it; an entry above its file's count, at zero,
 * or naming no tracked test file must go down; a second `wait_until` in one language fails.
 * Inputs: a map of tracked path to its `scan` result, and the allowlist of path to count.
 * Output: one line per problem, sorted; empty when the tree holds the ratchet.
 */
export function check(found, allow) {
  assert.ok(found instanceof Map, "results come as a map of path to scan result");
  assert.ok(allow !== null && typeof allow === "object" && !Array.isArray(allow));
  const problems = [];
  const helpers = { rust: [], js: [] };
  for (const [file, result] of found) {
    const kind = kindOf(file);
    assert.ok(kind !== null, `${file} is not a checked kind`);
    helpers[kind].push(...result.helpers.map((line) => `${file}:${line}`));
    if (result.lines.length > 0 && !Object.hasOwn(allow, file)) {
      const count = sleeps(result.lines.length, "new ");
      problems.push(`${file}: ${count} in test code (${shown(result.lines)}); ${ADVICE}`);
    }
  }
  for (const [kind, places] of Object.entries(helpers)) {
    if (places.length > 1) {
      const { helper, name } = LANGUAGE[kind];
      problems.push(
        `${helper} is defined more than once in ${name} test code (${places.join(", ")}); ` +
          "keep one shared helper",
      );
    }
  }
  for (const [file, allowed] of Object.entries(allow)) {
    const result = found.get(file);
    const count = result?.lines.length ?? 0;
    if (!Number.isInteger(allowed) || allowed < 1) {
      problems.push(`${file}: entry ${JSON.stringify(allowed)} is not a sleep count`);
    } else if (result === undefined || !result.test) {
      problems.push(`${file}: not a tracked test file; remove its entry`);
    } else if (count > allowed) {
      problems.push(
        `${file}: ${sleeps(count)} in test code (${shown(result.lines)}), ` +
          `allowed at most ${allowed}; the allowlist only shrinks`,
      );
    } else if (count === 0) {
      problems.push(`${file}: no sleeps left in test code; remove its entry`);
    } else if (count < allowed) {
      problems.push(
        `${file}: ${sleeps(count)} in test code; lower its entry from ${allowed} to ${count}`,
      );
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
    throw new Error(`${ALLOWLIST} must be a JSON object of path to sleep count`);
  }
  return allow;
}

/**
 * Scans every tracked, regular Rust, JS or TS file of a repository and checks the test files
 * against its allowlist; untracked files are never read. A Rust file a test-only `mod`
 * declares, and every file in that module's folder, is test code whole.
 * Input: the repository root. Output: the problems, as `check` gives them.
 */
export function run(root) {
  const options = { cwd: root, encoding: "utf8", maxBuffer: LISTING_BYTES_MAX };
  const listed = execFileSync("git", ["ls-files", "-z"], options);
  const texts = new Map();
  for (const file of listed.split("\0")) {
    if (file === "" || kindOf(file) === null) {
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
    if (kindOf(file) === "rust") {
      modules.push(...testModules(file, text.split("\n")));
    }
  }
  const found = new Map();
  for (const [file, text] of texts) {
    const inside = (module) => file === `${module}.rs` || file.startsWith(`${module}/`);
    const result = scan(file, text, kindOf(file) === "rust" && modules.some(inside));
    if (result.test) {
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
    process.stderr.write(`${problems.length} sleep problem(s) in test code (${ALLOWLIST})\n`);
    process.exit(1);
  }
  process.stdout.write("no new sleeps in test code\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
