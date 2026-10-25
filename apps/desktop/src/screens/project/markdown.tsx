import type { ReactNode } from "react";
import { displayText } from "../conversation";

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "em"; children: Inline[] }
  | { kind: "link"; href: string; children: Inline[] };

export type Block =
  | { kind: "heading"; level: number; text: string; children: Inline[] }
  | { kind: "paragraph"; children: Inline[] }
  | { kind: "list"; ordered: boolean; items: Inline[][] }
  | { kind: "code"; lang: string; text: string }
  | { kind: "table"; header: Inline[][]; rows: Inline[][][] }
  | { kind: "quote"; children: Inline[] }
  | { kind: "rule" };

export const MARKDOWN_CHARS_MAX = 1024 * 1024;
const INLINE_DEPTH_MAX = 4;
const TABLE_COLUMNS_MAX = 32;
const URL_CHARS_MAX = 2048;
const PUNCTUATION = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/** Accepts only absolute http and https links without spaces or control characters.
 * Input: a link target from untrusted Markdown. Output: the same target, or null. */
export function safeHref(url: string): string | null {
  if (!url || url.length > URL_CHARS_MAX || /[\u0000- \u007f-\u009f]/u.test(url)) return null;
  if (!/^https?:\/\/[^/]/iu.test(url)) return null;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? url : null;
  } catch {
    return null;
  }
}

function pushText(out: Inline[], text: string) {
  if (!text) return;
  const last = out[out.length - 1];
  if (last?.kind === "text") last.text += text;
  else out.push({ kind: "text", text });
}

const isSpace = (value: string | undefined) => value === undefined || /\s/u.test(value);
const isWord = (value: string | undefined) => value !== undefined && /[\p{L}\p{N}]/u.test(value);

function closingParen(text: string, open: number): number {
  let depth = 0;
  for (let index = open; index < text.length && index - open <= URL_CHARS_MAX + 256; index += 1) {
    const char = text[index];
    if (char === "\n") return -1;
    if (char === "(") depth += 1;
    else if (char === ")") {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

function bareUrl(text: string, start: number): string {
  let end = start;
  while (end < text.length && end - start < URL_CHARS_MAX && !/[\s<>"`]/u.test(text[end])) end += 1;
  let url = text.slice(start, end);
  for (;;) {
    const last = url[url.length - 1];
    if (last && ".,;:!?'*_".includes(last)) url = url.slice(0, -1);
    else if (last === ")" && (url.match(/\)/gu)?.length ?? 0) > (url.match(/\(/gu)?.length ?? 0)) url = url.slice(0, -1);
    else return url;
  }
}

/** Parses inline Markdown: code, strong, emphasis, links and bare http(s) links.
 * Inputs: the text and the nesting depth. Output: inline nodes; anything else stays text. */
export function parseInline(text: string, depth = 0): Inline[] {
  const out: Inline[] = [];
  const missing = new Map<string, number>();
  const find = (needle: string, from: number) => {
    const known = missing.get(needle);
    if (known !== undefined && from >= known) return -1;
    const found = text.indexOf(needle, from);
    if (found < 0) missing.set(needle, from);
    return found;
  };
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    const next = text[index + 1];
    if (char === "\\" && next !== undefined && PUNCTUATION.includes(next)) {
      pushText(out, next);
      index += 2;
      continue;
    }
    if (char === "`") {
      let run = 1;
      while (text[index + run] === "`") run += 1;
      const fence = "`".repeat(run);
      const close = find(fence, index + run);
      if (close >= 0) {
        out.push({ kind: "code", text: text.slice(index + run, close).replace(/^ (.*) $/su, "$1") });
        index = close + run;
        continue;
      }
      pushText(out, fence);
      index += run;
      continue;
    }
    if (depth < INLINE_DEPTH_MAX && (char === "*" || char === "_") && next === char && !isSpace(text[index + 2])) {
      const delimiter = char + char;
      let close = find(delimiter, index + 2);
      while (close >= 0 && (isSpace(text[close - 1]) || (char === "_" && isWord(text[close + 2])))) close = find(delimiter, close + 1);
      if (close > index + 2 && !(char === "_" && isWord(text[index - 1]))) {
        out.push({ kind: "strong", children: parseInline(text.slice(index + 2, close), depth + 1) });
        index = close + 2;
        continue;
      }
    }
    if (depth < INLINE_DEPTH_MAX && (char === "*" || char === "_") && next !== char && !isSpace(next) && !(char === "_" && isWord(text[index - 1]))) {
      let close = find(char, index + 1);
      while (close >= 0 && (isSpace(text[close - 1]) || text[close + 1] === char || (char === "_" && isWord(text[close + 1])))) {
        close = text[close + 1] === char ? find(char, close + 2) : find(char, close + 1);
      }
      if (close > index + 1) {
        out.push({ kind: "em", children: parseInline(text.slice(index + 1, close), depth + 1) });
        index = close + 1;
        continue;
      }
    }
    if (char === "[" && depth < INLINE_DEPTH_MAX) {
      const close = find("]", index + 1);
      if (close > index && text[close + 1] === "(") {
        const end = closingParen(text, close + 1);
        if (end > close) {
          const target = text.slice(close + 2, end).trim().split(/\s+/u)[0].replace(/^<(.*)>$/u, "$1");
          const href = safeHref(target);
          const label = parseInline(text.slice(index + 1, close), depth + 1);
          if (href) out.push({ kind: "link", href, children: label });
          else for (const node of label) node.kind === "text" ? pushText(out, node.text) : out.push(node);
          index = end + 1;
          continue;
        }
      }
    }
    if (char === "<") {
      const close = find(">", index + 1);
      const href = close > index ? safeHref(text.slice(index + 1, close)) : null;
      if (href) {
        out.push({ kind: "link", href, children: [{ kind: "text", text: href }] });
        index = close + 1;
        continue;
      }
    }
    if ((char === "h" || char === "H") && !isWord(text[index - 1]) && /^https?:\/\//iu.test(text.slice(index, index + 8))) {
      const url = bareUrl(text, index);
      const href = safeHref(url);
      if (href) {
        out.push({ kind: "link", href, children: [{ kind: "text", text: url }] });
        index += url.length;
        continue;
      }
    }
    pushText(out, char);
    index += 1;
  }
  return out;
}

const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([^`\s]*)/u;
const HEADING_OPEN = /^ {0,3}(#{1,6})(?=[ \t]|$)/u;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/u;
const QUOTE = /^ {0,3}>[ \t]?(.*)$/u;
const ITEM_OPEN = /^ {0,6}([-*+]|\d{1,9}[.)])[ \t]/u;

function blank(character: string | undefined): boolean {
  return character === " " || character === "\t";
}

/** Reads an ATX heading line in one pass, without a backtracking regex over its text.
 * Input: one line. Output: the level and the text between the opening hashes and an
 * optional closing hash run, or null when the line is not a heading. */
function headingLine(line: string): { level: number; text: string } | null {
  const open = HEADING_OPEN.exec(line);
  if (!open) return null;
  let start = open[0].length;
  while (blank(line[start])) start += 1;
  let end = line.length;
  while (end > start && blank(line[end - 1])) end -= 1;
  let hashes = end;
  while (hashes > start && line[hashes - 1] === "#") hashes -= 1;
  if (hashes < end && hashes > start && blank(line[hashes - 1])) {
    end = hashes;
    while (end > start && blank(line[end - 1])) end -= 1;
  }
  return { level: open[1].length, text: line.slice(start, end) };
}

/** Reads a list item line in one pass. Input: one line. Output: the marker and the item
 * text after the marker's spacing, or null when the line is not a list item. */
function itemLine(line: string): { marker: string; text: string } | null {
  const open = ITEM_OPEN.exec(line);
  if (!open) return null;
  let start = open[0].length;
  while (blank(line[start])) start += 1;
  return { marker: open[1], text: line.slice(start) };
}

function cells(line: string): string[] {
  let row = line.trim();
  if (row.startsWith("|")) row = row.slice(1);
  if (row.endsWith("|") && !row.endsWith("\\|")) row = row.slice(0, -1);
  const out: string[] = [];
  let current = "";
  for (let index = 0; index < row.length; index += 1) {
    if (row[index] === "\\" && row[index + 1] === "|") {
      current += "\\|";
      index += 1;
    } else if (row[index] === "|") {
      out.push(current.trim());
      current = "";
    } else current += row[index];
  }
  out.push(current.trim());
  return out;
}

function separator(line: string | undefined): boolean {
  if (line === undefined || !line.includes("-") || !line.includes("|") && !/^\s*:?-+:?\s*$/u.test(line)) return false;
  const parts = cells(line);
  return parts.length <= TABLE_COLUMNS_MAX && parts.every((part) => /^:?-+:?$/u.test(part));
}

function startsBlock(line: string, next: string | undefined): boolean {
  return FENCE.test(line) || headingLine(line) !== null || RULE.test(line) || QUOTE.test(line) || itemLine(line) !== null || (line.includes("|") && separator(next));
}

/** Parses the Markdown subset research reports use into blocks.
 * Input: untrusted Markdown, truncated to MARKDOWN_CHARS_MAX characters. Output: blocks. */
export function parseMarkdown(source: string): Block[] {
  const lines = source.slice(0, MARKDOWN_CHARS_MAX).replace(/\r\n?/gu, "\n").split("\n");
  const blocks: Block[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1];
      const body: string[] = [];
      index += 1;
      while (index < lines.length && !new RegExp(`^ {0,3}${marker[0] === "`" ? "`" : "~"}{${marker.length},}[ \\t]*$`, "u").test(lines[index])) {
        body.push(lines[index]);
        index += 1;
      }
      index += 1;
      blocks.push({ kind: "code", lang: fence[2], text: body.join("\n") });
      continue;
    }
    const heading = headingLine(line);
    if (heading) {
      const hashes = heading.level;
      blocks.push({ kind: "heading", level: hashes, text: line.trimStart().slice(hashes).trim(), children: parseInline(heading.text) });
      index += 1;
      continue;
    }
    if (RULE.test(line)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }
    if (line.includes("|") && separator(lines[index + 1])) {
      const header = cells(line).slice(0, TABLE_COLUMNS_MAX);
      const rows: Inline[][][] = [];
      index += 2;
      while (index < lines.length && lines[index].trim() && lines[index].includes("|")) {
        const row = cells(lines[index]).slice(0, header.length);
        while (row.length < header.length) row.push("");
        rows.push(row.map((cell) => parseInline(cell)));
        index += 1;
      }
      blocks.push({ kind: "table", header: header.map((cell) => parseInline(cell)), rows });
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (index < lines.length && QUOTE.test(lines[index])) {
        body.push(QUOTE.exec(lines[index])![1]);
        index += 1;
      }
      blocks.push({ kind: "quote", children: parseInline(body.join("\n")) });
      continue;
    }
    const item = itemLine(line);
    if (item) {
      const ordered = /\d/u.test(item.marker);
      const items: string[] = [];
      while (index < lines.length) {
        const current = itemLine(lines[index]);
        if (current && /\d/u.test(current.marker) === ordered) {
          items.push(current.text);
          index += 1;
        } else if (!current && /^\s{2,}\S/u.test(lines[index]) && items.length) {
          items[items.length - 1] += "\n" + lines[index].trim();
          index += 1;
        } else break;
      }
      blocks.push({ kind: "list", ordered, items: items.map((text) => parseInline(text)) });
      continue;
    }
    const body = [line];
    index += 1;
    while (index < lines.length && lines[index].trim() && !startsBlock(lines[index], lines[index + 1])) {
      body.push(lines[index]);
      index += 1;
    }
    blocks.push({ kind: "paragraph", children: parseInline(body.join("\n")) });
  }
  return blocks;
}

function inlineText(nodes: Inline[]): string {
  return nodes.map((node) => node.kind === "text" || node.kind === "code" ? node.text : inlineText(node.children)).join("");
}

function inlineLinks(nodes: Inline[], out: { href: string; text: string }[]) {
  for (const node of nodes) {
    if (node.kind === "link") out.push({ href: node.href, text: inlineText(node.children) });
    else if (node.kind === "strong" || node.kind === "em") inlineLinks(node.children, out);
  }
}

/** Collects every link in a parsed report once, in reading order, for the Sources list.
 * Input: parsed blocks. Output: unique links with the text of their first use. */
export function markdownLinks(blocks: Block[]): { href: string; text: string }[] {
  const all: { href: string; text: string }[] = [];
  for (const block of blocks) {
    if (block.kind === "heading" || block.kind === "paragraph" || block.kind === "quote") inlineLinks(block.children, all);
    else if (block.kind === "list") block.items.forEach((item) => inlineLinks(item, all));
    else if (block.kind === "table") [block.header, ...block.rows].forEach((row) => row.forEach((cell) => inlineLinks(cell, all)));
  }
  const seen = new Set<string>();
  return all.filter((link) => !seen.has(link.href) && Boolean(seen.add(link.href)));
}

/** Lists heading texts in order, as the engine matches them when saving one section.
 * Input: parsed blocks. Output: heading texts. */
export function headings(blocks: Block[]): string[] {
  return blocks.flatMap((block) => block.kind === "heading" && block.text ? [block.text] : []);
}

function renderInline(nodes: Inline[], open: (href: string) => void): ReactNode[] {
  return nodes.map((node, key) => {
    switch (node.kind) {
      case "text": return displayText(node.text);
      case "code": return <code key={key}>{displayText(node.text)}</code>;
      case "strong": return <strong key={key}>{renderInline(node.children, open)}</strong>;
      case "em": return <em key={key}>{renderInline(node.children, open)}</em>;
      case "link": return <a key={key} href={node.href} target="_blank" rel="noopener noreferrer"
        onClick={(event) => { event.preventDefault(); open(node.href); }}>{renderInline(node.children, open)}</a>;
    }
  });
}

/** Renders untrusted Markdown as React elements; raw HTML stays visible text.
 * Inputs: the source, a link handler and an optional per-heading action.
 * Output: the rendered report. */
export function MarkdownView({ source, onOpenLink, headingAction }: {
  source: string;
  onOpenLink: (href: string) => void;
  headingAction?: (heading: string) => ReactNode;
}) {
  const blocks = parseMarkdown(source);
  return <div className="markdown">{blocks.map((block, key) => {
    switch (block.kind) {
      case "heading": {
        const Tag = `h${Math.min(6, block.level + 1)}` as "h2";
        return <div className="markdown-heading" key={key}><Tag>{renderInline(block.children, onOpenLink)}</Tag>{headingAction?.(block.text)}</div>;
      }
      case "paragraph": return <p key={key}>{renderInline(block.children, onOpenLink)}</p>;
      case "quote": return <blockquote key={key}>{renderInline(block.children, onOpenLink)}</blockquote>;
      case "rule": return <hr key={key} />;
      case "code": return <pre key={key}><code>{displayText(block.text)}</code></pre>;
      case "list": {
        const items = block.items.map((item, index) => <li key={index}>{renderInline(item, onOpenLink)}</li>);
        return block.ordered ? <ol key={key}>{items}</ol> : <ul key={key}>{items}</ul>;
      }
      case "table": return <div className="markdown-table" key={key}><table>
        <thead><tr>{block.header.map((cell, index) => <th key={index}>{renderInline(cell, onOpenLink)}</th>)}</tr></thead>
        <tbody>{block.rows.map((row, index) => <tr key={index}>{row.map((cell, column) => <td key={column}>{renderInline(cell, onOpenLink)}</td>)}</tr>)}</tbody>
      </table></div>;
    }
  })}</div>;
}
