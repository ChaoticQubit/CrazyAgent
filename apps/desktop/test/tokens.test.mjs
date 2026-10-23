import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8");

function block(selector) {
  const start = css.indexOf(selector + " {");
  assert.ok(start >= 0, `tokens.css has no ${selector} block`);
  const body = css.slice(start + selector.length + 2, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
}

function luminance(hex) {
  assert.match(hex, /^#[0-9a-f]{6}$/, `token value ${hex} is not a six digit hex color`);
  const [r, g, b] = [1, 3, 5].map((index) => parseInt(hex.slice(index, index + 2), 16) / 255)
    .map((value) => value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function ratio(foreground, background) {
  const [high, low] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
}

const text = [
  ["text", ["bg", "surface", "surface-2"]],
  ["text-2", ["bg", "surface", "surface-2"]],
  ["text-3", ["bg", "surface", "surface-2"]],
  ["accent-text", ["bg", "surface", "surface-2", "accent-soft"]],
  ["on-accent", ["accent-solid", "accent-hover"]],
  ["ok", ["bg", "surface", "ok-soft"]],
  ["warn", ["bg", "surface", "warn-soft"]],
  ["danger", ["bg", "surface", "danger-soft"]],
  ["on-danger", ["danger-solid"]],
];

const graphics = [
  ["border-input", ["bg", "surface", "surface-2"]],
  ["focus", ["bg", "surface", "surface-2"]],
  ["accent", ["bg", "surface", "surface-2"]],
];

for (const [theme, selector] of [["light", ":root"], ["dark", ':root[data-theme="dark"]']]) {
  test(`${theme} theme text pairs meet WCAG AA 4.5:1 and graphics meet 3:1`, () => {
    const tokens = block(selector);
    const failures = [];
    for (const [rule, minimum] of [[text, 4.5], [graphics, 3]]) {
      for (const [foreground, backgrounds] of rule) {
        for (const background of backgrounds) {
          const value = ratio(tokens[foreground], tokens[background]);
          if (value < minimum) failures.push(`${foreground} on ${background} is ${value.toFixed(2)}, needs ${minimum}`);
        }
      }
    }
    assert.deepEqual(failures, []);
  });
}

test("the system dark theme and the chosen dark theme use identical tokens", () => {
  const media = css.slice(css.indexOf("@media (prefers-color-scheme: dark)"));
  const system = media.slice(media.indexOf(':root:not([data-theme="light"]) {'));
  const body = system.slice(system.indexOf("{") + 1, system.indexOf("}"));
  const tokens = Object.fromEntries([...body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
  assert.deepEqual(tokens, block(':root[data-theme="dark"]'));
  assert.deepEqual(Object.keys(block(':root[data-theme="dark"]')).sort(),
    Object.keys(block(":root")).filter((name) => !/^(font|mono|space|radius|dur|ease|z)-?/.test(name) || name === "focus").sort());
});

test("brand colors and fonts live in tokens: the brand vermilion, bone and charcoal are present", () => {
  assert.equal(block(":root").bg, "#f2eee6");
  assert.equal(block(":root").accent, "#e5482d");
  assert.equal(block(':root[data-theme="dark"]').bg, "#161513");
  assert.equal(block(':root[data-theme="dark"]').accent, "#ff6a45");
  assert.match(block(":root").font, /^"Instrument Sans"/);
  assert.match(block(":root").mono, /^"JetBrains Mono"/);
  const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
  assert.doesNotMatch(style, /#[0-9a-fA-F]{3,8}\b/, "style.css must use tokens, not literal colors");
  assert.doesNotMatch(style, /font-family:(?!\s*var\(--(?:font|mono)\))/, "style.css must use font tokens");
  assert.doesNotMatch(style, /rgba?\(/, "style.css must take shadows and scrims from tokens");
});
