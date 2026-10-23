import "./load-source.mjs";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { parseSvg, Icon, ICON_NAMES, ProviderMark, providerLogo } = await import("../src/ui/icon.tsx");
const iconDir = new URL("../src/assets/icons/", import.meta.url);
const logoDir = new URL("../src/assets/logos/", import.meta.url);

test("every vendored Lucide and Simple Icons file parses into allowlisted shapes only", () => {
  for (const [directory, files] of [[iconDir, readdirSync(iconDir)], [logoDir, readdirSync(logoDir)]]) {
    for (const file of files.filter((name) => name.endsWith(".svg"))) {
      const shape = parseSvg(readFileSync(new URL(file, directory), "utf8"));
      assert.equal(shape.viewBox, "0 0 24 24", file);
      assert.ok(shape.children.length > 0, `${file} has no drawable shapes`);
      for (const child of shape.children) {
        assert.match(child.tag, /^(path|circle|rect|line|polyline|polygon|ellipse)$/, file);
        for (const [name, value] of Object.entries(child.attrs)) {
          if (name === "fill") assert.match(value, /^(currentColor|none)$/, file);
          else assert.match(name, /^(d|cx|cy|r|rx|ry|x|y|x1|x2|y1|y2|width|height|points)$/, file);
        }
      }
    }
  }
});

test("the parser refuses scripts, event handlers, links and foreign elements", () => {
  const svg = (inner) => `<svg viewBox="0 0 24 24">${inner}</svg>`;
  for (const hostile of [
    '<script>alert(1)</script>', '<path d="M0 0" onload="alert(1)" />', '<a href="https://x.test"><path d="M0 0" /></a>',
    '<image href="data:x" />', '<path d="M0 0" fill="url(#x)" />', '<path d="M0 0" style="fill:url(javascript:x)" />', '<foreignObject></foreignObject>',
  ]) {
    assert.throws(() => parseSvg(svg(hostile)), /icon_svg_rejected/, hostile);
  }
  assert.throws(() => parseSvg('<svg viewBox="0 0 48 48"><path d="M0 0" /></svg>'), /icon_svg_rejected/);
});

test("Icon renders inline decorative SVG, or a labelled image when given a label", () => {
  assert.ok(ICON_NAMES.length >= 60);
  for (const name of ICON_NAMES) {
    const markup = renderToStaticMarkup(createElement(Icon, { name }));
    assert.match(markup, /^<svg\b[^>]*aria-hidden="true"[^>]*>/, name);
    assert.match(markup, /stroke="currentColor"/);
    assert.doesNotMatch(markup, /<script|on[a-z]+=|href=/i);
  }
  const labelled = renderToStaticMarkup(createElement(Icon, { name: "circle-check", label: "Done", size: 20 }));
  assert.match(labelled, /role="img"/);
  assert.match(labelled, /aria-label="Done"/);
  assert.match(labelled, /width="20"/);
  assert.doesNotMatch(labelled, /aria-hidden/);
});

test("provider marks use the CC0 logo where one exists and a monogram otherwise, never brand colors", () => {
  assert.ok(providerLogo("claude"));
  assert.ok(providerLogo("openrouter"));
  assert.equal(providerLogo("chatgpt"), undefined);
  const logo = renderToStaticMarkup(createElement(ProviderMark, { provider: "claude", name: "Claude" }));
  assert.match(logo, /<svg\b[^>]*fill="currentColor"/);
  assert.match(logo, /title="Claude"/);
  assert.doesNotMatch(logo, /#[0-9a-f]{6}/i);
  const monogram = renderToStaticMarkup(createElement(ProviderMark, { provider: "chatgpt", name: "ChatGPT / Codex" }));
  assert.match(monogram, />CG</);
  assert.equal(renderToStaticMarkup(createElement(ProviderMark, { provider: "unknown-thing", name: "pi" })).includes(">PI<"), true);
});
