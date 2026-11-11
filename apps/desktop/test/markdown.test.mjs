import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const md = await import("../src/screens/project/markdown.tsx");
const linksFixture = await bundle(new URL("./fixtures/markdown-links.mjs", import.meta.url).pathname);

const render = (source) => renderToStaticMarkup(createElement(md.MarkdownView, { source, onOpenLink: () => {} }));

const cpuMs = (run) => {
  const started = process.cpuUsage();
  const result = run();
  const used = process.cpuUsage(started);
  return { result, ms: (used.user + used.system) / 1000 };
};

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
  assert.match(markup, /<a href="https:\/\/acme.dev" title="https:\/\/acme.dev" target="_blank" rel="noopener noreferrer">Acme<\/a>/);
  assert.deepEqual(md.markdownLinks(md.parseMarkdown(source)), [
    { href: "https://acme.dev", text: "Acme" },
    { href: "https://beta.dev/x", text: "https://beta.dev/x" },
  ]);
});

test("control and bidi characters are shown as visible escapes", () => {
  const markup = render("name\u202etxt.exe and \u0007bell");
  assert.match(markup, /\\u202e/);
  assert.match(markup, /\\u0007/);
  assert.doesNotMatch(markup, /[\u202e\u0007]/u);
});

test("heading sections list every heading in order for saving one section", () => {
  assert.deepEqual(md.headings(md.parseMarkdown("# Top\ntext\n## Who pays\n### Notes\n```\n# fake\n```")), ["Top", "Who pays", "Notes"]);
});

test("huge or deeply nested input stays bounded", () => {
  const deep = "*".repeat(5000) + "x" + "*".repeat(5000);
  const { ms } = cpuMs(() => {
    md.parseMarkdown(deep);
    md.parseMarkdown("[".repeat(20000));
    md.parseMarkdown("| a ".repeat(2000) + "\n" + "|---".repeat(2000));
  });
  assert.ok(ms < 2000);
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
  for (const source of ["#" + " ".repeat(16000) + "\u2028", "-" + " ".repeat(200000) + "\u2028", "1." + "\t".repeat(200000) + "\u2029"]) {
    const { result: blocks, ms } = cpuMs(() => md.parseMarkdown(source));
    assert.ok(ms < 1000, `${JSON.stringify(source.slice(0, 3))} took ${ms} ms`);
    assert.equal(blocks.length, 1);
  }
});

test("a full-size line of openers that never close parses inline in linear time", () => {
  const size = md.MARKDOWN_CHARS_MAX;
  const fill = (unit) => unit.repeat(Math.ceil(size / unit.length)).slice(0, size);
  let ticks = "";
  for (let run = 1500; ticks.length < size - run - 1 && run > 0; run -= 1) ticks += "`".repeat(run) + " ";
  const sources = [fill("*a "), fill("**a "), fill("_a "), fill("__a "), fill("***a "), "[".repeat(size - 1) + "]", "<".repeat(size - 1) + ">", ticks,
    "[".repeat(size - 3000) + "](" + "x".repeat(2990), fill("http://a" + ")".repeat(2040) + " ")];
  for (const source of sources) {
    assert.ok(source.length <= size);
    const { result: blocks, ms } = cpuMs(() => md.parseMarkdown(source));
    assert.ok(ms < 1500, `${JSON.stringify(source.slice(0, 5))} took ${Math.round(ms)} ms`);
    assert.equal(blocks.length, 1);
  }
});

test("emphasis keeps its opener and closer rules", () => {
  const inline = (source) => md.parseMarkdown(source)[0].children;
  assert.deepEqual(inline("*a* b"), [{ kind: "em", children: [{ kind: "text", text: "a" }] }, { kind: "text", text: " b" }]);
  assert.deepEqual(inline("*a *b*"), [{ kind: "em", children: [{ kind: "text", text: "a *b" }] }]);
  assert.deepEqual(inline("*a **b* c"), [{ kind: "em", children: [{ kind: "text", text: "a **b" }] }, { kind: "text", text: " c" }]);
  assert.deepEqual(inline("**a ** b**"), [{ kind: "strong", children: [{ kind: "text", text: "a ** b" }] }]);
  assert.deepEqual(inline("__a__b__ c__"), [{ kind: "strong", children: [{ kind: "text", text: "a__b" }] }, { kind: "text", text: " c__" }]);
  assert.deepEqual(inline("_a_b_ c"), [{ kind: "em", children: [{ kind: "text", text: "a_b" }] }, { kind: "text", text: " c" }]);
  assert.deepEqual(inline("*a b"), [{ kind: "text", text: "*a b" }]);
  assert.deepEqual(inline("[[x](https://a.example)"), [{ kind: "link", href: "https://a.example", children: [{ kind: "text", text: "[x" }] }]);
  assert.deepEqual(inline("``a` b``"), [{ kind: "code", text: "a` b" }]);
  assert.deepEqual(inline("see https://a.example/x_(y)). next"), [{ kind: "text", text: "see " }, { kind: "link", href: "https://a.example/x_(y)", children: [{ kind: "text", text: "https://a.example/x_(y)" }] }, { kind: "text", text: "). next" }]);
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

test("a link whose text looks like a URL on another host names the target host; other links do not", () => {
  assert.equal(md.linkTargetHost("https://docs.example", "https://other.example/page"), "other.example");
  assert.equal(md.linkTargetHost("docs.example.com/guide", "https://evil.example/login"), "evil.example");
  assert.equal(md.linkTargetHost(" acme.dev ", "https://acme.dev.evil.example/"), "acme.dev.evil.example");
  assert.equal(md.linkTargetHost("acme.dev", "https://acme.dev@evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("\u0430pple.com", "https://apple.com/"), "apple.com");
  assert.equal(md.linkTargetHost("https://acme.dev/a", "https://acme.dev/b?c=1"), null);
  assert.equal(md.linkTargetHost("ACME.dev", "https://acme.dev/"), null);
  assert.equal(md.linkTargetHost("acme.dev", "https://www.acme.dev/"), null);
  assert.equal(md.linkTargetHost("https://www.acme.dev", "https://acme.dev/"), null);
  assert.equal(md.linkTargetHost("the docs", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("version 1.2", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("", "https://other.example/"), null);
});

test("URL text in brackets, quotes or before punctuation still names another host, and URL text that cannot be read asks", () => {
  assert.equal(md.linkTargetHost("github.com.", "https://evil.example/login"), "evil.example");
  assert.equal(md.linkTargetHost("(https://github.com/login)", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("<github.com>,", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("“github.com”", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("( github.com )", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("https://github.com:99999", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("https://github.com:99999", "https://github.com/"), "github.com", "text that cannot be read as a URL never passes as the target");
  assert.equal(md.linkTargetHost(`github.com/${"a".repeat(3000)}`, "https://evil.example/"), "evil.example", "long URL text is checked by its start");
  assert.equal(md.linkTargetHost(`github.com/${"a".repeat(3000)}`, "https://github.com/a"), null);
  assert.equal(md.linkTargetHost("github.com.", "https://github.com/"), null);
  assert.equal(md.linkTargetHost("[acme.dev]", "https://www.acme.dev/"), null);
  assert.equal(md.linkTargetHost("the docs.", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("e.g.", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("(see below)", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("...", "https://other.example/"), null);
  assert.equal(md.linkTargetHost("github.com", "not a url"), null, "a target that is not a URL has no host to name");
});

test("link text that names a host among other words asks when any named host differs from the target", () => {
  assert.equal(md.linkTargetHost("see example.com for details", "https://other.example/"), "other.example");
  assert.equal(md.linkTargetHost("the https://github.com/login page", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("docs at acme.dev, mirror at evil.example", "https://acme.dev/"), "acme.dev", "every named host must match");
  assert.equal(md.linkTargetHost("github.com,evil.example", "https://evil.example/"), "evil.example");
  assert.equal(md.linkTargetHost("github.com@evil.example", "https://github.com/"), "github.com");
  assert.equal(md.linkTargetHost("open https://github.com:99999 now", "https://github.com/"), "github.com");
  assert.equal(md.linkTargetHost("ｇｉｔｈｕｂ．ｃｏｍ", "https://evil.example/"), "evil.example", "full-width text names a host too");
  assert.equal(md.linkTargetHost("github。com", "https://evil.example/"), "evil.example", "an ideographic full stop is a dot in a host");
  assert.equal(md.linkTargetHost("github。com", "https://github.com/"), null);
  assert.equal(md.linkTargetHost("see github.com for details", "https://github.com/x"), null);
  assert.equal(md.linkTargetHost("read https://acme.dev/docs/index.html first", "https://www.acme.dev/docs"), null, "a file name in a URL path is not a host");
  assert.equal(md.linkTargetHost("read the guide, then e.g. section 1.2", "https://other.example/"), null);
  const many = (count) => Array.from({ length: count }, () => "acme.dev").join(" ");
  assert.equal(md.linkTargetHost(many(md.SHOWN_HOSTS_MAX), "https://acme.dev/"), null);
  assert.equal(md.linkTargetHost(many(md.SHOWN_HOSTS_MAX + 1), "https://acme.dev/"), "acme.dev", "text naming more hosts than are checked asks");
});

test("link text naming a local host or an IP address asks like any other host", () => {
  const evil = "https://evil.example/login";
  for (const text of ["localhost", "localhost:3000", "Open the app at http://localhost:3000", "http://intranet/wiki", "1.2.3.4", "http://127.0.0.1:8080",
    "[::1]", "http://[::1]:8080", "see [::1] here", "[docs.example]", "256.1.1.1"]) {
    assert.equal(md.linkTargetHost(text, evil), "evil.example", text);
  }
  assert.equal(md.linkTargetHost("http://localhost:3000", "http://localhost:5173/"), null);
  assert.equal(md.linkTargetHost("[0:0::1]", "http://[::1]:8080/"), null, "hosts compare as the URL parser reads them");
  assert.equal(md.linkTargetHost("1.2.3.4", "https://1.2.3.4/"), null);
  for (const text of ["meet at 10:30", "ratio 3:1", "step 2", "see below:", "note: done", "intranet", "2130706433"]) {
    assert.equal(md.linkTargetHost(text, evil), null, text);
  }
});

test("every link shows its target on hover", () => {
  const markup = render("[https://docs.example](https://other.example/x) and [docs](https://acme.dev/a)");
  assert.match(markup, /<a href="https:\/\/other.example\/x" title="https:\/\/other.example\/x" [^>]*>https:\/\/docs.example<\/a>/);
  assert.match(markup, /<a href="https:\/\/acme.dev\/a" title="https:\/\/acme.dev\/a" [^>]*>docs<\/a>/);
  assert.deepEqual(md.parseInline("[see https://a.example or <https://c.example> **now**](https://b.example)"), [{ kind: "link", href: "https://b.example", children: [
    { kind: "text", text: "see https://a.example or https://c.example " }, { kind: "strong", children: [{ kind: "text", text: "now" }] }] }]);
});

test("a Sources list shows each report link once with its target beside differing text, only when asked for", () => {
  const source = "[https://docs.example](https://other.example/x), [docs](https://acme.dev/a) and [docs again](https://acme.dev/a)";
  const listed = renderToStaticMarkup(createElement(md.MarkdownView, { source, sources: true, onOpenLink: () => {} }));
  const list = listed.slice(listed.indexOf('<section class="pj-sources"'));
  assert.match(list, /^<section class="pj-sources" aria-labelledby="pj-sources"><h3 id="pj-sources">Sources<\/h3><ol>/);
  assert.equal((list.match(/<li>/g) ?? []).length, 2);
  assert.match(list, /<a href="https:\/\/other.example\/x" [^>]*>https:\/\/docs.example<\/a><span class="pj-small pj-muted pj-mono"> https:\/\/other.example\/x<\/span>/);
  assert.match(list, /<a href="https:\/\/acme.dev\/a" [^>]*>docs<\/a><span class="pj-small pj-muted pj-mono"> https:\/\/acme.dev\/a<\/span>/);
  assert.doesNotMatch(render(source), /pj-sources/);
  const context = readFileSync(new URL("../src/screens/project/context.tsx", import.meta.url), "utf8");
  assert.match(context, /<MarkdownView source=\{text\} onOpenLink=\{open\} sources /, "the Context tab lists Sources through the reader");
  assert.doesNotMatch(context, /pj-sources|markdownLinks/, "the Context tab keeps no Sources list of its own that skips the ask");
});

test("a click on a Sources link whose URL text names another host asks first like the report link; other Sources open at once", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(linksFixture);
    return evaluate(`(async () => {
      const tick = () => new Promise((done) => setTimeout(done, 0));
      const source = (text) => [...document.querySelectorAll(".pj-sources a")].find((anchor) => anchor.textContent === text);
      const button = (label) => [...document.querySelectorAll("dialog[open] button")].find((item) => item.textContent === label);
      Fixture.mount("[https://docs.example](https://other.example/x) and [the docs](https://third.example/)", true);
      const steps = {};
      source("https://docs.example").click(); await tick();
      steps.asked = Fixture.state();
      button("Cancel").click(); await tick();
      steps.cancelled = Fixture.state();
      source("https://docs.example").click(); await tick();
      button("Open").click(); await tick();
      steps.confirmed = Fixture.state();
      source("the docs").click(); await tick();
      steps.direct = Fixture.state();
      return steps;
    })()`);
  });
  assert.deepEqual(seen.asked, { opened: [], question: "Open other.example?", body: "The link text shows https://docs.example, but it opens https://other.example/x." });
  assert.deepEqual(seen.cancelled, { opened: [], question: null, body: null });
  assert.deepEqual(seen.confirmed, { opened: ["https://other.example/x"], question: null, body: null });
  assert.deepEqual(seen.direct, { opened: ["https://other.example/x", "https://third.example/"], question: null, body: null });
});

test("a click on a link whose URL text names another host asks first; Cancel opens nothing, Open opens the target, other links open at once", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(linksFixture);
    return evaluate(`(async () => {
      const tick = () => new Promise((done) => setTimeout(done, 0));
      const link = (text) => [...document.querySelectorAll(".markdown a")].find((anchor) => anchor.textContent === text);
      const button = (label) => [...document.querySelectorAll("dialog[open] button")].find((item) => item.textContent === label);
      Fixture.mount("[https://docs.example](https://other.example/x), [acme.dev](https://www.acme.dev/a) and [the docs](https://third.example/)");
      const steps = {};
      link("https://docs.example").click(); await tick();
      steps.asked = Fixture.state();
      button("Cancel").click(); await tick();
      steps.cancelled = Fixture.state();
      link("https://docs.example").click(); await tick();
      button("Open").click(); await tick();
      steps.confirmed = Fixture.state();
      link("acme.dev").click(); await tick();
      link("the docs").click(); await tick();
      steps.direct = Fixture.state();
      return steps;
    })()`);
  });
  assert.deepEqual(seen.asked, { opened: [], question: "Open other.example?", body: "The link text shows https://docs.example, but it opens https://other.example/x." });
  assert.deepEqual(seen.cancelled, { opened: [], question: null, body: null });
  assert.deepEqual(seen.confirmed, { opened: ["https://other.example/x"], question: null, body: null });
  assert.deepEqual(seen.direct, { opened: ["https://other.example/x", "https://www.acme.dev/a", "https://third.example/"], question: null, body: null });
});
