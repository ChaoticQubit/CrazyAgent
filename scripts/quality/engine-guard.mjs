import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { lstatSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { kindOf, testLines, testModules } from "./sleeps.mjs";
import { body, COMMENT, functions, GUARD, masked, topGuards } from "./engine-guard-text.mjs";

/** The allowlist of unguarded install functions, relative to the repository root. */
export const ALLOWLIST = "scripts/quality/engine-guard-allow.json";

/** The largest `git ls-files` listing read, far above the repository's size. */
const LISTING_BYTES_MAX = 64 * 1024 * 1024;

/** An install call: a method `install` or `install_with_grants`, or a sandbox type's own. */
const SITE =
  /\.install(?:_with_grants)?\s*\(|\b(?:InstalledSandbox|WindowsSandbox)::install\w*\s*\(/g;

/** A file that names a sandbox type, the only files whose install calls are read. */
const SANDBOX_FILE = /\b(?:ProjectSandbox|InstalledSandbox|WindowsSandbox)\b/;

/** The most entries the allowlist may hold; it only shrinks, so this only comes down. */
export const ALLOW_MAX = 8;

/** An allowlist guard that names a function: `path.rs#name`; any other text is a reason. */
const NAMED = /^([\w./-]+\.rs)#(\w+)$/;

/** A function a reason names, `path.rs#name`, which must still exist for the reason to hold. */
const CITED = /([\w./-]+\.rs)#(\w+)/g;

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
      const own = element(text, bound[2], statement.start + bound[0].length, statement.end, word);
      events.push({ ...statement, ...own, visible });
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
    } else if (chainGuarded(text, event.from, event.from + event.value.length, guardAt)) {
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

/** A closure step's text from its call's `(` to a guard: `(|s| s.` passes its input on. */
const STEP = /^\(\s*(?:move\s+)?\|\s*(\w+)\s*(?::[^|]*)?\|\s*\1\s*\.\s*$/;

/**
 * Whether a guard reaches the value a span of masked text ends in: it is a step of the span's
 * own method chain (outside every parenthesis of the span) or the whole of a closure step
 * (`.and_then(|s| s.guard_engine(e))`). A guard in another argument or a nested call does not.
 * Inputs: the masked text, the span's start and end, the depth-1 guards. Output: the answer.
 */
function chainGuarded(text, from, to, guardAt) {
  assert.ok(from <= to, "a span starts before it ends");
  return guardAt.some((guard) => {
    if (guard < from || guard >= to) {
      return false;
    }
    const opens = [];
    for (let at = from; at < guard; at += 1) {
      if (text[at] === "(" || text[at] === "[") {
        opens.push(at);
      } else if ((text[at] === ")" || text[at] === "]") && opens.length > 0) {
        opens.pop();
      }
    }
    return opens.length === 0 || (opens.length === 1 && STEP.test(text.slice(opens[0], guard)));
  });
}

/**
 * The parts of a tuple literal: `(a, b)` split at its top-level commas, as offsets. Inputs:
 * the masked text and the span holding the literal, with its `?` or `;` tail. Output: the
 * parts as `{ from, to }`, or null when the span is no single tuple literal.
 */
function tupleParts(text, from, to) {
  const span = text.slice(from, to);
  const start = span.search(/\S/);
  const end = span.replace(/[\s?;]*$/, "").length;
  if (start === -1 || span[start] !== "(" || span[end - 1] !== ")") {
    return null;
  }
  const parts = [];
  let level = 0;
  let part = from + start + 1;
  for (let at = from + start; at < from + end; at += 1) {
    const char = text[at];
    level += "([{".includes(char) ? 1 : ")]}".includes(char) ? -1 : 0;
    if (level === 0 && at < from + end - 1) {
      return null;
    }
    if (level === 1 && char === ",") {
      parts.push({ from: part, to: at });
      part = at + 1;
    }
  }
  parts.push({ from: part, to: from + end - 1 });
  return parts.filter((p) => text.slice(p.from, p.to).trim() !== "");
}

/**
 * The value one binding of a statement reads: for a tuple pattern over a tuple literal, the
 * element at the name's place; a pattern of another length reads nothing (fails closed);
 * any other right-hand side, one source, is read whole. Inputs: the masked text, the pattern,
 * the value's span and the name's word. Output: `{ value, from }`.
 */
function element(text, pattern, from, to, word) {
  const whole = { value: text.slice(from, to), from };
  if (!pattern.trim().startsWith("(")) {
    return whole;
  }
  const names = pattern.trim().slice(1, -1).split(",");
  const parts = tupleParts(text, from, to);
  if (parts === null) {
    return whole;
  }
  const index = names.findIndex((name) => word.test(name));
  if (parts.length !== names.length || index === -1) {
    return { value: "", from };
  }
  return { value: text.slice(parts[index].from, parts[index].to), from: parts[index].from };
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
  if (chainGuarded(text, receiverChain(text, statement.start, at, value), at, guardAt)) {
    return true;
  }
  return fromGuarded(text, fnBody, guardAt, value, statement.start, new Map());
}

/**
 * Where the method chain an install call is made on starts: walking back from the call over
 * names, `.`, `?`, `::` and balanced brackets, to the statement start or the first other
 * character. A closure step whose parameter is what the call installs (`.and_then(|s|
 * InstalledSandbox::install(s, ..))`) continues the chain it is a step of. Inputs: the masked
 * text, the statement start, the call offset and what it installs. Output: the chain's start.
 */
function receiverChain(text, start, at, value) {
  let level = 0;
  let from = at;
  while (from > start) {
    const char = text[from - 1];
    if (char === ")" || char === "]") {
      level += 1;
    } else if ((char === "(" || char === "[") && level > 0) {
      level -= 1;
    } else if (level === 0 && !/[\w.?:\s]/.test(char)) {
      const step = /\(\s*(?:move\s+)?\|\s*(\w+)\s*(?::[^|]*)?\|\s*$/.exec(text.slice(start, from));
      const head = /^\s*(\w+)/.exec(text.slice(from, at))?.[1];
      if (char !== "|" || step === null || ![value.trim(), head].includes(step[1])) {
        break;
      }
      from = start + step.index;
      continue;
    }
    from -= 1;
  }
  assert.ok(start <= from && from <= at, "the chain sits in the statement before the call");
  return from;
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
 * The places of `ProjectSandbox` in what a function returns: under a `Result` or `Option`,
 * each tuple element that is one, or null when the value is the sandbox itself. Input: the
 * signature's text. Output: the element indexes, or null.
 */
function sandboxPlaces(signature) {
  let type = signature.slice(signature.lastIndexOf("->") + 2).trim();
  const wrapped = /^(?:Result|Option)\s*<(.*)>$/s.exec(type);
  if (wrapped !== null) {
    let level = 0;
    let end = 0;
    for (; end < wrapped[1].length && !(level === 0 && wrapped[1][end] === ","); end += 1) {
      level += "(<".includes(wrapped[1][end]) ? 1 : ")>".includes(wrapped[1][end]) ? -1 : 0;
    }
    type = wrapped[1].slice(0, end).trim();
  }
  if (!type.startsWith("(")) {
    return null;
  }
  const kinds = [];
  let level = 0;
  let part = "";
  for (const char of type.slice(1, -1)) {
    level += "(<".includes(char) ? 1 : ")>".includes(char) ? -1 : 0;
    if (level === 0 && char === ",") {
      kinds.push(part.trim());
      part = "";
    } else {
      part += char;
    }
  }
  kinds.push(part.trim());
  return kinds.flatMap((kind, index) => (kind === "ProjectSandbox" ? [index] : []));
}

/**
 * Whether every value a builder returns is a sandbox guarded against the engine root: each
 * `return` and the tail, read through `Ok`/`Some` and, for a tuple, at each sandbox place,
 * is a guard chain ([`chainGuarded`]) or a method chain on a name that holds a guard
 * ([`holdsGuard`]). An `Err` returns none. Anything else fails closed, as does a body with no
 * tail. Inputs: the masked text, the line starts, the function and its body. Output: the answer.
 */
function returnsGuarded(text, starts, fn, fnBody) {
  const guardAt = topGuards(text, fnBody);
  const places = sandboxPlaces(text.slice(starts[fn.start], fnBody.open));
  const tail = fnBody.statements.find((s) => s.block === 0 && s.end === fnBody.close);
  const exits = fnBody.statements.filter((s) => /^\s*return\b/.test(text.slice(s.start, s.end)));
  if (tail === undefined || /^\s*return\b/.test(text.slice(tail.start, tail.end))) {
    return false;
  }
  const proven = (from, to, point) => {
    if (chainGuarded(text, from, to, guardAt)) {
      return true;
    }
    const head = /^\s*([A-Za-z_]\w*)\s*(?=\.|$)/.exec(text.slice(from, to));
    const chained = receiverChain(text, from, to, "") === from;
    return head !== null && chained && holdsGuard(text, fnBody, guardAt, head[1], point);
  };
  return [...exits, tail].every((exit) => {
    const keyword = /^\s*return\b/.exec(text.slice(exit.start, exit.end));
    let from = exit.start + (keyword?.[0].length ?? 0);
    let to = exit.start + text.slice(exit.start, exit.end).replace(/[\s;]*$/, "").length;
    if (/^\s*Err\s*\(/.test(text.slice(from, to))) {
      return true;
    }
    const wrap = /^\s*(?:Ok|Some)\s*\(/.exec(text.slice(from, to));
    if (wrap !== null && text[to - 1] === ")") {
      from += wrap[0].length;
      to -= 1;
    }
    if (places === null) {
      return proven(from, to, exit.start);
    }
    const parts = tupleParts(text, from, to);
    assert.ok((parts ?? []).every((p) => from <= p.from && p.to <= to), "parts sit in the value");
    return (
      parts !== null &&
      places.length > 0 &&
      places.every((p) => p < parts.length && proven(parts[p].from, parts[p].to, exit.start))
    );
  });
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
    const builder = fn.body !== null && builds(plain, starts, fn, fn.body);
    if (builder && !returnsGuarded(plain, starts, fn, fn.body)) {
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
