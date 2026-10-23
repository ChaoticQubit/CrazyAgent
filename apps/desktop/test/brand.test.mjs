import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { BrandMark } = await import("../src/ui/brand.tsx");
const read = (path) => readFileSync(new URL(path, import.meta.url));
const asset = (name) => read(`../src/assets/brand/${name}`).toString("utf8");

function shapes(svg) {
  return {
    coil: /<path d="([^"]+)"/.exec(svg)?.[1],
    squares: [...svg.matchAll(/<rect x="([^"]+)" y="([^"]+)" width="([^"]+)" height="([^"]+)" transform="([^"]+)"/g)].map((match) => match.slice(1)),
  };
}

function glyph(markup, kind) {
  const start = markup.indexOf(`class="brand-glyph ${kind}"`);
  assert.ok(start >= 0, `no ${kind} glyph`);
  return markup.slice(start, markup.indexOf("</svg>", start));
}

function pngSize(path) {
  const png = read(path);
  assert.equal(png.toString("latin1", 1, 4), "PNG", path);
  assert.equal(png.toString("latin1", 12, 16), "IHDR", path);
  return [png.readUInt32BE(16), png.readUInt32BE(20)];
}

test("BrandMark draws the Ember 4 mark from the brand assets and is named CrazyAgent", () => {
  const markup = renderToStaticMarkup(createElement(BrandMark));
  assert.match(markup, /^<span class="brand-mark" role="img" aria-label="CrazyAgent">/);
  assert.match(markup, /<span class="brand-name">CrazyAgent<\/span><\/span>$/);
  for (const [file, kind, count] of [["mark.svg", "full", 4], ["mark-reduced.svg", "reduced", 2]]) {
    const expected = shapes(asset(file));
    const drawn = shapes(glyph(markup, kind));
    assert.equal(expected.squares.length, count, file);
    assert.equal(drawn.coil, expected.coil, file);
    assert.deepEqual(drawn.squares, expected.squares, file);
  }
  assert.match(glyph(markup, "full"), /stroke="currentColor"/);
  assert.equal((markup.match(/class="brand-square"/g) ?? []).length, 6);
  assert.equal((markup.match(/aria-hidden="true"/g) ?? []).length, 2);
  assert.doesNotMatch(markup, /#[0-9a-fA-F]{6}/, "colors come from the theme tokens");
});

test("the large BrandMark for first run draws only the full mark at 48 px", () => {
  const markup = renderToStaticMarkup(createElement(BrandMark, { large: true }));
  assert.match(markup, /^<span class="brand-mark large" role="img" aria-label="CrazyAgent">/);
  assert.match(glyph(markup, "full"), /width="48" height="48"/);
  assert.doesNotMatch(markup, /brand-glyph reduced/);
  assert.equal((markup.match(/class="brand-square"/g) ?? []).length, 4);
});

test("the brand squares follow the light and dark brand token, and small screens use the reduced drawing", () => {
  const css = read("../src/style.css").toString("utf8");
  assert.match(css, /\.brand-glyph \.brand-square \{ fill: var\(--brand\); \}/);
  assert.match(css, /\.brand-mark \{[^}]*color: var\(--text\);/);
  assert.match(css, /@media \(max-resolution: 1\.5dppx\) \{\s*\.brand-mark:not\(\.large\) \.brand-glyph\.full \{ display: none; \}\s*\.brand-mark:not\(\.large\) \.brand-glyph\.reduced \{ display: block; \}\s*\}/);
});

test("index.html links the brand favicon as SVG with a 32 px PNG fallback", () => {
  const html = read("../index.html").toString("utf8");
  assert.match(html, /<link rel="icon" type="image\/png" sizes="32x32" href="\/src\/assets\/brand\/favicon-32\.png" \/>/);
  assert.match(html, /<link rel="icon" type="image\/svg\+xml" href="\/src\/assets\/brand\/favicon\.svg" \/>/);
  assert.deepEqual(pngSize("../src/assets/brand/favicon-32.png"), [32, 32]);
  const favicon = asset("favicon.svg");
  assert.equal(shapes(favicon).coil, shapes(asset("mark-reduced.svg")).coil);
  assert.match(favicon, /prefers-color-scheme:dark/);
  assert.doesNotMatch(favicon, /<script|href=|on[a-z]+=/i);
});

test("tauri bundles the brand icon set with the 1024 icon first, so the dock uses it", () => {
  const conf = JSON.parse(read("../src-tauri/tauri.conf.json").toString("utf8"));
  assert.deepEqual(conf.bundle.icon, [
    "icons/icon.png", "icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.ico",
  ]);
  for (const [file, size] of [["icon.png", 1024], ["32x32.png", 32], ["128x128.png", 128], ["128x128@2x.png", 256]]) {
    assert.deepEqual(pngSize(`../src-tauri/icons/${file}`), [size, size], file);
  }
  const icns = read("../src-tauri/icons/icon.icns");
  assert.equal(icns.toString("latin1", 0, 4), "icns");
  assert.equal(icns.readUInt32BE(4), icns.length);
  const ico = read("../src-tauri/icons/icon.ico");
  assert.deepEqual([ico.readUInt16LE(0), ico.readUInt16LE(2)], [0, 1]);
  const sizes = Array.from({ length: ico.readUInt16LE(4) }, (_, index) => ico[6 + index * 16] || 256);
  assert.deepEqual(sizes, [16, 24, 32, 48, 64, 256]);
});
