import "./load-source.mjs";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";
import { inflateSync } from "node:zlib";
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

/** Reads one pixel of an 8-bit RGBA PNG. Inputs: the PNG bytes and the pixel's column and row.
 * Output: its red, green, blue and alpha. */
function pixel(png, x, y) {
  assert.equal(png.toString("latin1", 1, 4), "PNG");
  const [width, depth, kind] = [png.readUInt32BE(16), png[24], png[25]];
  assert.deepEqual([depth, kind, png[28]], [8, 6, 0], "8-bit RGBA, not interlaced");
  const chunks = [];
  for (let at = 8; at < png.length; at += 12 + png.readUInt32BE(at)) {
    if (png.toString("latin1", at + 4, at + 8) === "IDAT") chunks.push(png.subarray(at + 8, at + 8 + png.readUInt32BE(at)));
  }
  const raw = inflateSync(Buffer.concat(chunks));
  const stride = width * 4;
  let previous = new Uint8Array(stride);
  for (let row = 0; row <= y; row += 1) {
    const filter = raw[row * (stride + 1)];
    const line = Uint8Array.from(raw.subarray(row * (stride + 1) + 1, (row + 1) * (stride + 1)));
    for (let i = 0; i < stride; i += 1) {
      const [a, b, c] = [i >= 4 ? line[i - 4] : 0, previous[i], i >= 4 ? previous[i - 4] : 0];
      const guess = a + b - c;
      const [pa, pb, pc] = [Math.abs(guess - a), Math.abs(guess - b), Math.abs(guess - c)];
      const add = [0, a, b, (a + b) >> 1, pa <= pb && pa <= pc ? a : pb <= pc ? b : c][filter];
      assert.notEqual(add, undefined, `filter ${filter}`);
      line[i] = (line[i] + add) & 255;
    }
    previous = line;
  }
  return [...previous.subarray(x * 4, x * 4 + 4)];
}

/** Reads the tile's top band and a corner of a square app icon PNG, where the brand tile and
 * not the mark is drawn (150 and 2 of 1024). Input: the PNG bytes. Output: both pixels. */
function tile(png) {
  const size = png.readUInt32BE(16);
  return { top: pixel(png, size >> 1, Math.round((150 * size) / 1024)), corner: pixel(png, 0, 0) };
}

/** Returns the embedded image of one icon in an .ico or .icns file. Inputs: the file bytes and
 * the .ico width or the .icns type. Output: the PNG bytes. */
function embedded(file, key) {
  if (file.toString("latin1", 0, 4) === "icns") {
    for (let at = 8; at < file.length; at += file.readUInt32BE(at + 4)) {
      if (file.toString("latin1", at, at + 4) === key) return file.subarray(at + 8, at + file.readUInt32BE(at + 4));
    }
    assert.fail(`no ${key} in the icns`);
  }
  for (let entry = 0; entry < file.readUInt16LE(4); entry += 1) {
    const head = 6 + entry * 16;
    if ((file[head] || 256) === key) return file.subarray(file.readUInt32LE(head + 12), file.readUInt32LE(head + 12) + file.readUInt32LE(head + 8));
  }
  return assert.fail(`no ${key} px entry in the ico`);
}

const NIGHT = (rgba, what) => assert.ok(rgba.slice(0, 3).every((value) => value < 64) && rgba[3] === 255, `${what} is the night tile: ${rgba}`);
const BONE = (rgba, what) => assert.ok(rgba.slice(0, 3).every((value) => value > 200) && rgba[3] === 255, `${what} is the bone tile: ${rgba}`);

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

test("tauri bundles the dark night tile as the fixed icon and the Icon Composer file for macOS 26 (DEC-181)", () => {
  const conf = JSON.parse(read("../src-tauri/tauri.conf.json").toString("utf8"));
  assert.deepEqual(conf.bundle.icon, [
    "icons/icon.png", "icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.ico", "icons/AppIcon.icon",
  ]);
  for (const [file, size] of [["icon.png", 1024], ["32x32.png", 32], ["128x128.png", 128], ["128x128@2x.png", 256]]) {
    assert.deepEqual(pngSize(`../src-tauri/icons/${file}`), [size, size], file);
    const { top, corner } = tile(read(`../src-tauri/icons/${file}`));
    NIGHT(top, file);
    assert.equal(corner[3], 0, `${file} corner is clear`);
  }
  const icns = read("../src-tauri/icons/icon.icns");
  assert.equal(icns.toString("latin1", 0, 4), "icns");
  assert.equal(icns.readUInt32BE(4), icns.length);
  for (const key of ["ic10", "ic08", "ic07"]) NIGHT(tile(embedded(icns, key)).top, `icns ${key}`);
  const ico = read("../src-tauri/icons/icon.ico");
  assert.deepEqual([ico.readUInt16LE(0), ico.readUInt16LE(2)], [0, 1]);
  const sizes = Array.from({ length: ico.readUInt16LE(4) }, (_, index) => ico[6 + index * 16] || 256);
  assert.deepEqual(sizes, [16, 24, 32, 48, 64, 256]);
  for (const size of sizes) NIGHT(tile(embedded(ico, size)).top, `ico ${size}`);
});

test("the light tile stays beside it for the running Dock and window icon of the Light theme", () => {
  for (const [file, size] of [["icon.png", 1024], ["128x128@2x.png", 256]]) {
    assert.ok(existsSync(new URL(`../src-tauri/icons/light/${file}`, import.meta.url)), `light/${file}`);
    assert.deepEqual(pngSize(`../src-tauri/icons/light/${file}`), [size, size], file);
    const { top, corner } = tile(read(`../src-tauri/icons/light/${file}`));
    BONE(top, `light/${file}`);
    assert.equal(corner[3], 0, `light/${file} corner is clear`);
  }
});

test("the Icon Composer file gives the fill and the mark a dark look the Mac picks with its icon style", () => {
  const folder = "../src-tauri/icons/AppIcon.icon";
  assert.ok(existsSync(new URL(`${folder}/icon.json`, import.meta.url)), "AppIcon.icon/icon.json");
  const icon = JSON.parse(read(`${folder}/icon.json`).toString("utf8"));
  assert.deepEqual(icon["supported-platforms"], { squares: ["macOS"] });
  const look = (list, appearance) => list?.find((entry) => entry.appearance === appearance)?.value;
  const hex = (value) => `#${value.slice("srgb:".length).split(",").slice(0, 3).map((part) => Math.round(Number(part) * 255).toString(16).padStart(2, "0")).join("").toUpperCase()}`;
  const fill = (appearance) => look(icon["fill-specializations"], appearance)?.["linear-gradient"]?.map(hex);
  assert.deepEqual(fill(undefined), ["#F7F4EE", "#E8E3D9"], "the default fill is the bone gradient");
  assert.deepEqual(fill("dark"), ["#2A2824", "#161513"], "the dark look fills with the night gradient");
  const layers = icon.groups.flatMap((group) => group.layers);
  const shown = (appearance) => layers.filter((layer) => look(layer["hidden-specializations"], appearance) === false).map((layer) => layer["image-name"]);
  assert.deepEqual(shown(undefined), ["mark-light.svg"], "the default look shows only the ink mark");
  assert.deepEqual(shown("dark"), ["mark-dark.svg"], "the dark look shows only the bone mark");
  const mark = shapes(asset("mark.svg"));
  for (const [name, colors] of [["mark-light.svg", ["#1B1A17", "#E5482D"]], ["mark-dark.svg", ["#F2EEE6", "#FF6A45"]]]) {
    assert.ok(existsSync(new URL(`${folder}/Assets/${name}`, import.meta.url)), name);
    const svg = read(`${folder}/Assets/${name}`).toString("utf8");
    assert.equal(shapes(svg).coil, mark.coil, `${name} draws the brand coil`);
    assert.deepEqual(shapes(svg).squares, mark.squares, `${name} draws the four squares`);
    assert.match(svg, /<g transform="translate\(194 194\) scale\(9\.9375\)">/, `${name} keeps the tile's mark-to-tile ratio, 512 of 824`);
    for (const color of colors) assert.ok(svg.includes(color), `${name} uses ${color}`);
    assert.doesNotMatch(svg, /<script|href=|on[a-z]+=/i);
  }
});
