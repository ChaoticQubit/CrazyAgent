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

/** A Rust char literal at the start of a text, such as `'{'`, `'\\''` or `'\\u{7b}'`. */
const CHAR = /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'/;

/**
 * The text of a Rust file with every string, char literal and comment blanked to spaces, so
 * each offset keeps its place and line breaks stay. Input: the text. Output: the masked text.
 */
function masked(text) {
  const out = text.split("");
  const blank = (from, to) => {
    for (let at = from; at < to; at += 1) {
      out[at] = out[at] === "\n" ? "\n" : " ";
    }
  };
  let at = 0;
  while (at < text.length) {
    const rest = text.slice(at, at + 3);
    const raw = /^b?r(#*)"/.exec(text.slice(at, at + 260));
    if (rest.startsWith("//")) {
      const stop = text.indexOf("\n", at);
      const end = stop === -1 ? text.length : stop;
      blank(at, end);
      at = end;
    } else if (rest.startsWith("/*")) {
      const stop = text.indexOf("*/", at + 2);
      const end = stop === -1 ? text.length : stop + 2;
      blank(at, end);
      at = end;
    } else if (raw !== null && !/\w/.test(text[at - 1] ?? "")) {
      const close = `"${raw[1]}`;
      const stop = text.indexOf(close, at + raw[0].length);
      const end = stop === -1 ? text.length : stop + close.length;
      blank(at, end);
      at = end;
    } else if (text[at] === '"') {
      let end = at + 1;
      while (end < text.length && text[end] !== '"') {
        end += text[end] === "\\" ? 2 : 1;
      }
      blank(at, end + 1);
      at = end + 1;
    } else if (text[at] === "'" && CHAR.test(text.slice(at, at + 12))) {
      const end = text.indexOf("'", at + 2) + 1;
      blank(at, end);
      at = end;
    } else {
      at += 1;
    }
  }
  assert.equal(out.length, text.length, "masking keeps every offset");
  return out.join("");
}

/**
 * The body of one function in masked text: the offset just inside its first `{` and its
 * closing one, the brace depth at each offset (1 directly in the body), its blocks as
 * `{ open, close, header }` (header: where the statement opening it starts) and its statements
 * at every depth as `{ start, end, block }`. Output: the body, or null for a declaration.
 */
function body(text, starts, fn) {
  const from = starts[fn.start];
  const to = fn.end + 1 < starts.length ? starts[fn.end + 1] : text.length;
  const open = text.indexOf("{", from);
  if (open === -1 || open >= to || text.slice(from, open).includes(";")) {
    return null;
  }
  const depth = new Map();
  const statements = [];
  const blocks = [{ open, close: to, header: from }];
  const stack = [0];
  const begun = [null, open + 1];
  const push = (start, end) => {
    if (text.slice(start, end).trim() !== "") {
      statements.push({ start, end, block: stack.at(-1) });
    }
  };
  let level = 1;
  let at = open + 1;
  for (; at < to && level > 0; at += 1) {
    const char = text[at];
    depth.set(at, level);
    if (char === "{") {
      blocks.push({ open: at, close: to, header: begun[level] });
      stack.push(blocks.length - 1);
      level += 1;
      begun[level] = at + 1;
    } else if (char === "}") {
      push(begun[level], at);
      blocks[stack.pop()].close = at;
      level -= 1;
      const next = /\S/.exec(text.slice(at + 1, at + 80));
      const goes = next !== null && /^[.?),]|^else\b/.test(text.slice(at + 1 + next.index));
      if (level >= 1 && !goes) {
        push(begun[level], at + 1);
        begun[level] = at + 1;
      }
    } else if (char === ";") {
      push(begun[level], at + 1);
      begun[level] = at + 1;
    }
  }
  assert.ok(stack.length === 0 || level > 0, "the body's blocks close in order");
  return { open, close: at - 1, depth, blocks, statements };
}

/**
 * The guard calls of a function body that sit directly in it, at brace depth 1: never in an
 * `if`, `match`, loop or closure block. Inputs: the masked text and the body. Output: the
 * offsets of those calls.
 */
function topGuards(text, fnBody) {
  const found = [];
  const all = new RegExp(GUARD.source, "g");
  all.lastIndex = fnBody.open;
  for (let call = all.exec(text); call?.index < fnBody.close; call = all.exec(text)) {
    if (fnBody.depth.get(call.index) === 1) {
      found.push(call.index);
    }
  }
  return found;
}

/** A statement's binding: `let [mut] name`, `let (a, b)`, a bare `name` or `(a, b)`, then `=`. */
const BINDING = /^\s*(let\s+)?(?:mut\s+)?(\(?[\w\s,]*?\)?)\s*(?::[^=]*)?=(?![=>])/;

/** A block header opened by `if` or `while`, whose `let` parts bind names for the block. */
const CONDITION = /^\s*(?:if|while)\b/;

/** One `let` part of a block header: its pattern and the expression it reads, to a next `&&`. */
const LET_PART = /\blet\s+([^=]*?)=(?![=>])((?:(?!&&)[^{])*)/g;

/** Names a value is read from: identifiers not after `.` or `::` nor before `(`, `::` or `!`. */
const NAMES = /(?<![\w.:])([A-Za-z_]\w*)\b(?!\s*(?:\(|::|!))/g;

/** Words that name no sandbox a value could be read from. */
const KEYWORDS = new Set(["self", "Self", "Some", "Ok", "Err", "None", "mut", "move", "ref"]);

/**
 * Whether a name holds a sandbox guarded against the engine root at a point: walking, in
 * order, the statements before it that bind it in a block holding the point, the `if let`
 * headers of those blocks, and its plain assignments in other blocks, which only clear it.
 * Each read is at an earlier point, so it ends. Inputs: the masked text, the body, the depth-1
 * guards, the name, the point and the answers so far by `name@point`. Output: the answer.
 */
function holdsGuard(text, fnBody, guardAt, name, point, memo = new Map()) {
  const key = `${name}@${point}`;
  if (memo.has(key)) {
    return memo.get(key);
  }
  memo.set(key, false);
  const word = new RegExp(`\\b${name}\\b`);
  const holds = (block) => block.open < point && point <= block.close;
  const events = [];
  for (const statement of fnBody.statements.filter((s) => s.end <= point)) {
    const source = text.slice(statement.start, statement.end);
    const bound = BINDING.exec(source);
    if (bound === null || !word.test(bound[2]) || /^\s*(?:if|while|for|match)\b/.test(source)) {
      continue;
    }
    const visible = holds(fnBody.blocks[statement.block]);
    if (visible || bound[1] === undefined) {
      events.push({ ...statement, value: source.slice(bound[0].length), visible });
    }
  }
  for (const block of fnBody.blocks.filter((b) => holds(b) && b.header < b.open)) {
    const header = text.slice(block.header, block.open);
    for (const part of CONDITION.test(header) ? header.matchAll(LET_PART) : []) {
      if (word.test(part[1])) {
        const start = block.header + part.index;
        events.push({ start, end: block.open, value: part[2], header: true });
      }
    }
  }
  let guarded = false;
  for (const event of events.sort((a, b) => a.start - b.start)) {
    const fresh = /\bProjectSandbox::new\s*\(/.test(event.value);
    const read = () => !fresh && fromGuarded(text, fnBody, guardAt, event.value, event.start, memo);
    if (event.header) {
      guarded = read();
    } else if (!event.visible) {
      guarded = guarded && (word.test(event.value) || read());
    } else if (guardAt.some((at) => at >= event.start && at < event.end)) {
      guarded = true;
    } else if (/^\s*None\s*;?\s*$/.test(event.value)) {
      guarded = true;
    } else if (!word.test(event.value) || fresh) {
      guarded = read();
    }
  }
  memo.set(key, guarded);
  return guarded;
}

/**
 * Whether a value is read from a sandbox guarded at a point: some name it reads holds one
 * ([`holdsGuard`]). Inputs: the masked text, the body, the depth-1 guards, the value's text,
 * the point and the answers so far. Output: the answer.
 */
function fromGuarded(text, fnBody, guardAt, value, point, memo) {
  const names = Array.from(value.matchAll(NAMES), (found) => found[1]);
  return names
    .filter((name) => !KEYWORDS.has(name))
    .some((name) => holdsGuard(text, fnBody, guardAt, name, point, memo));
}

/**
 * Whether an install call is guarded: its statement guards before it at depth 1, or what it
 * installs (a type's install's first argument, the name it is called on, else its chain) is
 * read from a guarded sandbox ([`fromGuarded`]). Inputs: the masked text, the body, the call
 * offset and its text. Output: the answer.
 */
function guardedCall(text, fnBody, at, call) {
  const guardAt = topGuards(text, fnBody);
  const statement = fnBody.statements
    .filter((s) => s.start <= at && at < s.end)
    .reduce((inner, next) => (inner === null || next.start > inner.start ? next : inner), null);
  if (statement === null) {
    return false;
  }
  if (guardAt.some((g) => g >= statement.start && g < at)) {
    return true;
  }
  const lead = text.slice(statement.start, at);
  const bound = BINDING.exec(lead);
  const chain = bound === null ? lead : lead.slice(bound[0].length);
  let value = chain;
  if (!call.startsWith(".")) {
    const rest = text.slice(at + call.length, statement.end);
    let level = 0;
    let end = 0;
    for (; end < rest.length && !(level === 0 && /[,)]/.test(rest[end])); end += 1) {
      level += rest[end] === "(" ? 1 : rest[end] === ")" ? -1 : 0;
    }
    value = rest.slice(0, end);
  } else {
    const before = /(?:^|[^\w.:])(\w+)\s*$/.exec(lead);
    const closure = before !== null && new RegExp(`\\|[\\w\\s,:]*\\b${before[1]}\\b`).test(lead);
    value = before === null || closure ? chain : before[1];
  }
  return fromGuarded(text, fnBody, guardAt, value, statement.start, new Map());
}

/** A return type that hands back a sandbox by value, not a reference to one. */
const RETURNS_SANDBOX = /->[^{]*?(?<![&\w]\s*|&'\w+\s+)\bProjectSandbox\b/;

/** A parameter that takes a sandbox by value. */
const TAKES_SANDBOX = /:\s*ProjectSandbox\b/;

/**
 * Whether a function is a sandbox builder the contract checks: it returns a `ProjectSandbox`
 * by value and either makes one (`ProjectSandbox::new`) or takes none by value, so the
 * sandbox it returns is its own. A function that takes a sandbox and returns it, making none,
 * passes its caller's on. Inputs: the masked text, the line starts, the function and its
 * body. Output: the answer.
 */
function builds(text, starts, fn, fnBody) {
  const signature = text.slice(starts[fn.start], fnBody.open);
  if (!RETURNS_SANDBOX.test(signature)) {
    return false;
  }
  const params = signature.slice(0, signature.lastIndexOf("->"));
  const makes = /\bProjectSandbox::new\s*\(/.test(text.slice(fnBody.open, fnBody.close));
  return makes || !TAKES_SANDBOX.test(params);
}

/**
 * Scans one Rust file's non-test functions: which guard at their top, each builder returning a
 * sandbox it did not guard, and, in a file naming a sandbox type, each install call with its
 * innermost function and whether its receiver is guarded ([`guardedCall`]). Inputs: the path,
 * the text, whether a test-only `mod` declares it. Output: `{ sites, guarded }`, sites as
 * `{ line, fn, guarded }` (1-based; a builder's line with `returns: true`), guarded by name.
 */
export function scan(file, text, whole = false) {
  assert.equal(typeof file, "string");
  assert.equal(typeof text, "string");
  if (kindOf(file) !== "rust") {
    return { sites: [], guarded: new Map() };
  }
  const lines = text.split("\n");
  const mask = testLines("rust", file, lines, whole) ?? lines.map(() => false);
  const plain = masked(text);
  const plainLines = plain.split("\n");
  const starts = [];
  lines.reduce((offset, line) => {
    starts.push(offset);
    return offset + line.length + 1;
  }, 0);
  const fns = functions(lines)
    .filter((fn) => !mask[fn.start])
    .map((fn) => ({ ...fn, body: body(plain, starts, fn) }));
  const guarded = new Map();
  const sites = [];
  for (const fn of fns) {
    const top = fn.body !== null && topGuards(plain, fn.body).length > 0;
    guarded.set(fn.name, (guarded.get(fn.name) ?? true) && top);
    if (fn.body !== null && builds(plain, starts, fn, fn.body) && !top) {
      sites.push({ line: fn.start + 1, fn: fn.name, guarded: false, returns: true });
    }
  }
  const named = SANDBOX_FILE.test(text);
  lines.forEach((line, index) => {
    if (!named || mask[index] || COMMENT.test(line)) {
      return;
    }
    for (const call of plainLines[index].matchAll(SITE)) {
      const holders = fns.filter((fn) => fn.start <= index && index <= fn.end);
      const fn = holders.reduce((inner, next) => (next.start > inner.start ? next : inner), {
        name: "?",
        start: -1,
        body: null,
      });
      const at = starts[index] + call.index;
      const covered = fn.body !== null && guardedCall(plain, fn.body, at, call[0]);
      sites.push({ line: index + 1, fn: fn.name, guarded: covered });
    }
  });
  sites.sort((a, b) => a.line - b.line);
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
  const returning = new Set();
  for (const [file, result] of found) {
    for (const site of result.sites.filter((site) => !site.guarded)) {
      const key = `${file}#${site.fn}`;
      unguarded.set(key, [...(unguarded.get(key) ?? []), site.line]);
      if (site.returns) {
        returning.add(key);
      }
    }
  }
  for (const [key, lines] of unguarded) {
    if (!Object.hasOwn(allow, key)) {
      const [file, fn] = key.split("#");
      const does = returning.has(key) ? "returns a sandbox it built" : "installs a sandbox";
      problems.push(
        `${file}:${lines.join(",")}: fn ${fn} ${does} with no guard_engine; ` +
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
