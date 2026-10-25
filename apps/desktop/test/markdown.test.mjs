import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const md = await import("../src/screens/project/markdown.tsx");

const render = (source) => renderToStaticMarkup(createElement(md.MarkdownView, { source, onOpenLink: () => {} }));

test("headings, paragraphs, lists, rules and quotes parse into blocks", () => {
  const blocks = md.parseMarkdown("# Market\n\nFirst line\nsecond line\n\n- one\n* two\n\n1. first\n2) second\n\n---\n\n> quoted *text*\n\n###### Deep");
  assert.deepEqual(blocks.map((block) => block.kind), ["heading", "paragraph", "list", "list", "rule", "quote", "heading"]);
  assert.equal(blocks[0].level, 1);
  assert.equal(blocks[0].text, "Market");
  assert.deepEqual(blocks[1].children, [{ kind: "text", text: "First line\nsecond line" }]);
  assert.equal(blocks[2].ordered, false);
  assert.equal(blocks[2].items.length, 2);
  assert.equal(blocks[3].ordered, true);
  assert.equal(blocks[6].level, 6);
});

test("inline code, strong, emphasis and links parse without nesting surprises", () => {
  assert.deepEqual(md.parseInline("a `b *c*` **d _e_** [f](https://x.dev/a_(b)) g"), [
    { kind: "text", text: "a " },
    { kind: "code", text: "b *c*" },
    { kind: "text", text: " " },
    { kind: "strong", children: [{ kind: "text", text: "d " }, { kind: "em", children: [{ kind: "text", text: "e" }] }] },
    { kind: "text", text: " " },
    { kind: "link", href: "https://x.dev/a_(b)", children: [{ kind: "text", text: "f" }] },
    { kind: "text", text: " g" },
  ]);
  assert.deepEqual(md.parseInline("snake_case_name and 2 * 3 * 4"), [{ kind: "text", text: "snake_case_name and 2 * 3 * 4" }]);
  assert.deepEqual(md.parseInline("see https://example.com/page."), [
    { kind: "text", text: "see " },
    { kind: "link", href: "https://example.com/page", children: [{ kind: "text", text: "https://example.com/page" }] },
    { kind: "text", text: "." },
  ]);
  assert.deepEqual(md.parseInline("\\*not em\\*"), [{ kind: "text", text: "*not em*" }]);
});

test("fenced code keeps its text exactly and ignores markdown inside", () => {
  const blocks = md.parseMarkdown("```ts\nconst a = '<b>**x**</b>';\n\n# not heading\n```\nafter");
  assert.equal(blocks[0].kind, "code");
  assert.equal(blocks[0].lang, "ts");
  assert.equal(blocks[0].text, "const a = '<b>**x**</b>';\n\n# not heading");
  assert.equal(blocks[1].kind, "paragraph");
  const unclosed = md.parseMarkdown("```\nopen");
  assert.deepEqual(unclosed, [{ kind: "code", lang: "", text: "open" }]);
});

test("pipe tables parse header and rows with inline content", () => {
  const blocks = md.parseMarkdown("| Name | Price |\n|:---|---:|\n| **Acme** | $5 \\| month |\n| Beta |\n\nafter");
  assert.equal(blocks[0].kind, "table");
  assert.deepEqual(blocks[0].header, [[{ kind: "text", text: "Name" }], [{ kind: "text", text: "Price" }]]);
  assert.deepEqual(blocks[0].rows[0][0], [{ kind: "strong", children: [{ kind: "text", text: "Acme" }] }]);
  assert.deepEqual(blocks[0].rows[0][1], [{ kind: "text", text: "$5 | month" }]);
  assert.deepEqual(blocks[0].rows[1], [[{ kind: "text", text: "Beta" }], []]);
  assert.equal(blocks[1].kind, "paragraph");
  const markup = render("| A | B |\n|---|---|\n| 1 | 2 |");
  assert.match(markup, /<table>.*<th>A<\/th>.*<td>1<\/td>.*<\/table>/s);
});

test("only http and https links become anchors", () => {
  assert.equal(md.safeHref("https://example.com/a?b=1#c"), "https://example.com/a?b=1#c");
  assert.equal(md.safeHref("http://127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
  for (const url of ["javascript:alert(1)", "JaVaScRiPt:alert(1)", " javascript:alert(1)", "java\tscript:alert(1)", "data:text/html,<script>alert(1)</script>",
    "vbscript:msgbox", "file:///etc/passwd", "/relative", "#frag", "mailto:a@b.c", "https://exa mple.com", "https://a.com/\u0000x", ""]) {
    assert.equal(md.safeHref(url), null, url);
  }
});

test("an XSS attempt renders as inert text", () => {
  const attack = [
    "<script>alert('x')</script>",
    "<img src=x onerror=alert(1)>",
    "[click](javascript:alert(1))",
    "[data](data:text/html;base64,PHNjcmlwdD4=)",
    "[quote](https://ok.dev/\" onmouseover=\"alert(1))",
    "<a href=\"javascript:alert(1)\">a</a>",
    "**<iframe src=https://evil.dev>**",
    "| <svg onload=alert(1)> | x |\n|---|---|\n| y | z |",
    "```\n</code><script>alert(2)</script>\n```",
  ].join("\n\n");
  const markup = render(attack);
  assert.doesNotMatch(markup, /<(?:script|img|iframe|svg|object|embed|style)\b/i);
  assert.doesNotMatch(markup, /<[^>]*\son[a-z]+\s*=/i);
  assert.doesNotMatch(markup, /<[^>]*(?:href|src)="(?:javascript|data|vbscript):/i);
  const tags = new Set([...markup.matchAll(/<([a-z0-9]+)\b/gi)].map((match) => match[1].toLowerCase()));
  for (const tag of tags) assert.ok(["div", "p", "a", "strong", "em", "code", "pre", "table", "thead", "tbody", "tr", "th", "td"].includes(tag), tag);
  assert.match(markup, /&lt;script&gt;alert/);
  assert.match(markup, /&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.match(markup, /click/);
  assert.doesNotMatch(markup, /href="[^"]*javascript/i);
  const anchors = [...markup.matchAll(/<a\b[^>]*href="([^"]*)"/g)].map((match) => match[1]);
  for (const href of anchors) assert.match(href, /^https?:\/\//);
});

test("links render as safe anchors and the sources list holds each link once", () => {
  const source = "See [Acme](https://acme.dev) and https://beta.dev/x.\n\n- [Acme again](https://acme.dev)\n- [bad](javascript:x)";
  const markup = render(source);
  assert.match(markup, /<a href="https:\/\/acme.dev" target="_blank" rel="noopener noreferrer">Acme<\/a>/);
  assert.deepEqual(md.markdownLinks(md.parseMarkdown(source)), [
    { href: "https://acme.dev", text: "Acme" },
    { href: "https://beta.dev/x", text: "https://beta.dev/x" },
  ]);
});

test("control and bidi characters are shown as visible escapes", () => {
  const markup = render("name‮txt.exe and \u0007bell");
  assert.match(markup, /\\u202e/);
  assert.match(markup, /\\u0007/);
  assert.doesNotMatch(markup, /[‮\u0007]/u);
});

test("heading sections list every heading in order for saving one section", () => {
  assert.deepEqual(md.headings(md.parseMarkdown("# Top\ntext\n## Who pays\n### Notes\n```\n# fake\n```")), ["Top", "Who pays", "Notes"]);
});

test("huge or deeply nested input stays bounded", () => {
  const deep = "*".repeat(5000) + "x" + "*".repeat(5000);
  const started = performance.now();
  md.parseMarkdown(deep);
  md.parseMarkdown("[".repeat(20000));
  md.parseMarkdown("| a ".repeat(2000) + "\n" + "|---".repeat(2000));
  assert.ok(performance.now() - started < 2000);
  const long = "line\n".repeat(300000);
  const blocks = md.parseMarkdown(long);
  const text = blocks.map((block) => block.kind === "paragraph" ? block.children.map((c) => c.text ?? "").join("") : "").join("");
  assert.ok(text.length <= md.MARKDOWN_CHARS_MAX + 100);
});

test("the renderer never injects raw HTML", () => {
  const source = readFileSync(new URL("../src/screens/project/markdown.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /dangerouslySetInnerHTML|innerHTML/);
});

test("heading and list lines that never match parse in linear time", () => {
  const timed = (source) => {
    const started = performance.now();
    const blocks = md.parseMarkdown(source);
    return { blocks, ms: performance.now() - started };
  };
  for (const source of ["#" + " ".repeat(16000) + " ", "-" + " ".repeat(200000) + " ", "1." + "\t".repeat(200000) + " "]) {
    const { blocks, ms } = timed(source);
    assert.ok(ms < 1000, `${JSON.stringify(source.slice(0, 3))} took ${ms} ms`);
    assert.equal(blocks.length, 1);
  }
});

test("headings keep their closing-hash and empty-text rules", () => {
  assert.deepEqual(md.parseMarkdown("# Title ##")[0].children, [{ kind: "text", text: "Title" }]);
  assert.deepEqual(md.parseMarkdown("## a ## ##  ")[0].children, [{ kind: "text", text: "a ##" }]);
  assert.deepEqual(md.parseMarkdown("# #")[0].children, [{ kind: "text", text: "#" }]);
  assert.deepEqual(md.parseMarkdown("#")[0].children, []);
  assert.deepEqual(md.parseMarkdown("###   ")[0].children, []);
  assert.deepEqual(md.parseMarkdown("   #\tTabbed\t#\t")[0].children, [{ kind: "text", text: "Tabbed" }]);
  assert.equal(md.parseMarkdown("####### seven")[0].kind, "paragraph");
  assert.equal(md.parseMarkdown("#nospace")[0].kind, "paragraph");
  assert.equal(md.parseMarkdown("    # four spaces")[0].kind, "paragraph");
  assert.deepEqual(md.parseMarkdown("-   spaced item\n+ next")[0].items, [[{ kind: "text", text: "spaced item" }], [{ kind: "text", text: "next" }]]);
  assert.equal(md.parseMarkdown("-nospace")[0].kind, "paragraph");
});
