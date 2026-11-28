import assert from "node:assert/strict";

/** A call that guards a sandbox against the engine root's secrets. */
export const GUARD = /\bguard_engine(?:_around)?\s*\(/;

/** A Rust function's first line; groups: indent, name. */
const FN = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)/;

/** A comment line, by its trimmed start. */
export const COMMENT = /^\s*(?:\/\/|\/\*|\*)/;

/**
 * The functions of a Rust file: each `fn` line to the first later line that is only `}` at
 * its indent, or to itself for a declaration ending in `;`. Input: the lines.
 * Output: `{ name, start, end }` per function, 0-based and inclusive.
 */
export function functions(lines) {
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
export function masked(text) {
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
export function body(text, starts, fn) {
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
export function topGuards(text, fnBody) {
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
