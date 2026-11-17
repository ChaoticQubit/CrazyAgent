import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const css = read("../src/ui/tokens.css");
const DARK_RULE = ':root[data-theme="dark"]';

/**
 * The custom properties one rule of tokens.css sets. Input: the rule's selector as written.
 * Output: an object of token name, without the dashes, to its raw value.
 */
function block(selector) {
  const start = css.indexOf(selector + " {");
  assert.ok(start >= 0, `tokens.css has no ${selector} block`);
  const body = css.slice(start + selector.length + 2, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
}

const LIGHT = block(":root");
const DARK = { ...LIGHT, ...block(DARK_RULE) };

/**
 * One token's value in a theme, following `var(--name)` aliases as the browser does on
 * :root, where every theme rule applies. Inputs: the theme's tokens and the name.
 * Output: the final value; fails, naming the token, when it or an alias target is missing.
 */
function resolve(theme, name) {
  let value = theme[name];
  for (let hops = 0; hops < 8; hops += 1) {
    assert.ok(value !== undefined, `tokens.css sets no --${name}`);
    const alias = /^var\(--([a-z0-9-]+)\)$/.exec(value);
    if (!alias) return value;
    name = alias[1];
    value = theme[name];
  }
  assert.fail(`--${name} aliases more than 8 deep`);
}

/**
 * The sRGB channels a token paints, with an rgba color composited on its backdrop first.
 * Inputs: the theme, the token name and, for an rgba color, the backdrop token.
 * Output: [r, g, b] from 0 to 255.
 */
function paint(theme, name, under) {
  const value = resolve(theme, name);
  const hex = /^#([0-9a-f]{6})$/.exec(value);
  if (hex) return [0, 2, 4].map((at) => parseInt(hex[1].slice(at, at + 2), 16));
  const rgba = /^rgba\((\d+), (\d+), (\d+), ([\d.]+)\)$/.exec(value);
  assert.ok(rgba, `--${name} is ${value}, neither #rrggbb nor rgba(r, g, b, a)`);
  assert.ok(under, `--${name} is translucent and has no backdrop`);
  const alpha = Number(rgba[4]);
  assert.ok(alpha >= 0 && alpha <= 1, `--${name} alpha ${alpha}`);
  const base = paint(theme, under);
  return [1, 2, 3].map((at, index) => Math.round(Number(rgba[at]) * alpha + base[index] * (1 - alpha)));
}

/**
 * WCAG 2 contrast of two painted colors. Inputs: two [r, g, b] arrays.
 * Output: the ratio, from 1 to 21.
 */
function ratio(one, two) {
  const luminance = (rgb) => {
    const [r, g, b] = rgb.map((value) => value / 255).map((value) => value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const [high, low] = [luminance(one), luminance(two)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
}

/** Table 1, light page values, each resolved through its aliases. */
const QUIET_LIGHT = {
  bg: "#fafafa", surface: "#ffffff", "surface-2": "#f4f4f5", "surface-3": "#e9e9ec", selection: "#d9d9de",
  border: "#e4e4e7", "border-input": "#8a8a93", text: "#18181b", "text-2": "#52525b", "text-3": "#65656d",
  primary: "#18181b", "primary-hover": "#3f3f46", "on-primary": "#ffffff", focus: "#18181b", brand: "#e5482d",
  attention: "#b8341c", running: "#1b6a8a", "running-soft": "#e0eef4",
  "chart-in": "#e5482d", "chart-cached": "#2a78d6", "chart-out": "#149a6a",
  scrim: "rgba(24, 24, 27, 0.32)", "elev-1": "0 1px 2px rgba(24, 24, 27, 0.06)", "elev-2": "0 12px 32px rgba(24, 24, 27, 0.16)",
};

/** Table 1, dark page values, each resolved through its aliases. */
const QUIET_DARK = {
  bg: "#0c0c0d", surface: "#161618", "surface-2": "#1f1f22", "surface-3": "#2a2a2e", selection: "#3a3a40",
  border: "#2a2a2d", "border-input": "#71717a", text: "#f4f4f5", "text-2": "#d4d4d8", "text-3": "#a1a1aa",
  primary: "#f4f4f5", "primary-hover": "#d4d4d8", "on-primary": "#18181b", focus: "#f4f4f5", brand: "#ff6a45",
  attention: "#ff8a6b", running: "#6fb6d6", "running-soft": "#12262f",
  "chart-in": "#ff6a45", "chart-cached": "#2a78d6", "chart-out": "#199e70",
  scrim: "rgba(0, 0, 0, 0.55)", "elev-1": "0 1px 2px rgba(0, 0, 0, 0.4)", "elev-2": "0 12px 32px rgba(0, 0, 0, 0.55)",
};

/** Page tokens set once on :root: the same in both themes, or an alias that follows the theme. */
const ROOT_ONLY = ["chart-in", "chart-cached"];

/** The state tokens and the brand, which Table 1 keeps at their 2.4.2 values. */
const STATE = ["ok", "ok-soft", "warn", "warn-soft", "danger", "danger-soft", "danger-solid", "on-danger", "brand"];

/** Table 1, the terminal palette by name after `--terminal-`; dark in every theme. */
const QUIET_TERMINAL = {
  bg: "#111113", surface: "#1a1a1d", fg: "#e4e4e7", muted: "#a1a1aa", border: "#2e2e33", cursor: "#f4f4f5", focus: "#f4f4f5",
  selection: "rgba(228, 228, 231, 0.16)", scrollbar: "rgba(228, 228, 231, 0.1)", "scrollbar-hover": "rgba(228, 228, 231, 0.22)",
  black: "#27272a", white: "#d4d4d8", "bright-black": "#8b8b94", "bright-white": "#fafafa",
};

/** The retired accent tokens and the neutral token each one aliases until slice 15 removes them. */
const ACCENT_ALIASES = {
  accent: "primary", "accent-solid": "primary", "accent-hover": "primary-hover",
  "accent-text": "text", "accent-soft": "surface-3", "on-accent": "on-primary",
};

/** The type scale: one size token and one line-height token per step (FEATURE-250 section 1). */
const TYPE_SCALE = {
  "font-meta": "11px", "leading-meta": "16px", "font-secondary": "12px", "leading-secondary": "16px",
  "font-body": "13px", "leading-body": "20px", "font-section": "14px", "leading-section": "20px",
  "font-subtitle": "16px", "leading-subtitle": "24px", "font-title": "20px", "leading-title": "28px",
};

/** The weights, per QUESTION-15:55 Q1 A: 500 for UI text, 400 for prose, 600 for h1 and h2. */
const WEIGHTS = { "weight-ui": "500", "weight-prose": "400", "weight-title": "600" };

const TEXT = 4.5;
const CONTROL = 3;
const GROUNDS = ["bg", "surface", "surface-2", "surface-3"];

/**
 * Every page pair of Table 2 (FEATURE-250 section 1, the pair list of contrast-250.py):
 * text on each ground and on its fills at 4.5, controls and graphics at 3.0. Output:
 * [foreground, background, bar] rows.
 */
function pagePairs() {
  const rows = [];
  for (const fg of ["text", "text-2", "text-3", "attention", "ok", "warn", "danger", "running"]) {
    for (const bg of GROUNDS) rows.push([fg, bg, TEXT]);
  }
  for (const [fg, bg] of [
    ["text", "selection"], ["ok", "ok-soft"], ["warn", "warn-soft"], ["danger", "danger-soft"], ["running", "running-soft"],
    ["text", "ok-soft"], ["text", "warn-soft"], ["text", "danger-soft"], ["text", "running-soft"],
    ["on-primary", "primary"], ["on-primary", "primary-hover"], ["on-danger", "danger-solid"],
  ]) rows.push([fg, bg, TEXT]);
  for (const fg of ["border-input", "focus", "primary", "brand", "chart-in", "chart-cached", "chart-out"]) {
    for (const bg of GROUNDS.slice(0, 3)) rows.push([fg, bg, CONTROL]);
  }
  return rows;
}

const TERMINAL_TEXT = ["fg", "muted", "white", "bright-white", "bright-black", "black", "red", "green", "yellow", "blue", "magenta", "cyan",
  "bright-red", "bright-green", "bright-yellow", "bright-blue", "bright-magenta", "bright-cyan"];

/**
 * The ANSI program colors Table 2 lists as known limits: programs pick them, and each is
 * better than 2.4.2. The theme refusal of slice 17 reuses this list (QUESTION-15:55 Q3).
 */
const ANSI_LIMITS = ["black on bg", "black on surface", "black on selection", "bright-black on selection"];

for (const [name, theme] of [["light", LIGHT], ["dark", DARK]]) {
  test(`${name} theme: every Table 2 text pair is at least 4.5:1 and every control and graphic pair at least 3:1`, () => {
    const failures = [];
    for (const [fg, bg, bar] of pagePairs()) {
      const value = ratio(paint(theme, fg), paint(theme, bg));
      if (value < bar) failures.push(`--${fg} on --${bg} is ${value.toFixed(2)}, needs ${bar}`);
    }
    assert.deepEqual(failures, []);
  });
}

test("the agent terminal: every Table 2 pair meets its bar except the 4 ANSI limits, and each limit is still below it", () => {
  const failures = [];
  const limits = [];
  for (const fg of TERMINAL_TEXT) {
    for (const bg of ["bg", "surface", "selection"]) {
      const value = ratio(paint(LIGHT, `terminal-${fg}`), paint(LIGHT, `terminal-${bg}`, "terminal-bg"));
      if (ANSI_LIMITS.includes(`${fg} on ${bg}`)) limits.push(`${fg} on ${bg} ${value < TEXT ? "below" : "meets"}`);
      else if (value < TEXT) failures.push(`--terminal-${fg} on --terminal-${bg} is ${value.toFixed(2)}, needs ${TEXT}`);
    }
  }
  for (const fg of ["cursor", "focus"]) {
    const value = ratio(paint(LIGHT, `terminal-${fg}`), paint(LIGHT, "terminal-bg"));
    if (value < CONTROL) failures.push(`--terminal-${fg} on --terminal-bg is ${value.toFixed(2)}, needs ${CONTROL}`);
  }
  assert.deepEqual(failures, []);
  assert.deepEqual(limits.sort(), ANSI_LIMITS.map((limit) => `${limit} below`).sort(), "a limit that now meets the bar leaves the exemption list");
});

test("tokens.css holds Table 1's quiet values for the page in light and dark", () => {
  const page = (theme) => Object.fromEntries(Object.keys(QUIET_LIGHT).map((name) => [name, resolve(theme, name)]));
  assert.deepEqual(page(LIGHT), QUIET_LIGHT);
  assert.deepEqual(page(DARK), QUIET_DARK);
  assert.equal(LIGHT["chart-in"], "var(--brand)", "the input series is the brand color in both themes");
});

test("tokens.css holds Table 1's quiet terminal palette, one for every theme", () => {
  const terminal = Object.fromEntries(Object.keys(QUIET_TERMINAL).map((name) => [name, LIGHT[`terminal-${name}`]]));
  assert.deepEqual(terminal, QUIET_TERMINAL);
  assert.equal(paint(LIGHT, "terminal-selection", "terminal-bg").map((value) => value.toString(16)).join(""), "333335");
});

test("the values Table 1 keeps are today's: state colors, brand, on-danger, radii and the untouched ANSI colors", () => {
  const kept = (theme, names) => Object.fromEntries(names.map((name) => [name, theme[name]]));
  assert.deepEqual(kept(LIGHT, STATE), {
    ok: "#286b43", "ok-soft": "#e3efe5", warn: "#7d4b00", "warn-soft": "#f6e9cf", danger: "#a3221b",
    "danger-soft": "#f8e1de", "danger-solid": "#b3261e", "on-danger": "#ffffff", brand: "#e5482d",
  });
  assert.deepEqual(kept(DARK, STATE), {
    ok: "#6cc08e", "ok-soft": "#15281c", warn: "#e3b45e", "warn-soft": "#2c2313", danger: "#f2867e",
    "danger-soft": "#2e1715", "danger-solid": "#c2362c", "on-danger": "#ffffff", brand: "#ff6a45",
  });
  assert.deepEqual(kept(LIGHT, ["radius-segment", "radius-control", "radius-panel", "radius-dialog", "radius-pill"]), {
    "radius-segment": "4px", "radius-control": "6px", "radius-panel": "8px", "radius-dialog": "12px", "radius-pill": "999px",
  });
  assert.deepEqual(kept(LIGHT, ["terminal-red", "terminal-green", "terminal-yellow", "terminal-blue", "terminal-magenta", "terminal-cyan",
    "terminal-bright-red", "terminal-bright-green", "terminal-bright-yellow", "terminal-bright-blue", "terminal-bright-magenta", "terminal-bright-cyan"]), {
    "terminal-red": "#f2867e", "terminal-green": "#6cc08e", "terminal-yellow": "#e3b45e", "terminal-blue": "#8fb3d9",
    "terminal-magenta": "#d4a0c8", "terminal-cyan": "#7fc4bd", "terminal-bright-red": "#ff9d94", "terminal-bright-green": "#8fd6aa",
    "terminal-bright-yellow": "#f0c77a", "terminal-bright-blue": "#a9c6e6", "terminal-bright-magenta": "#e2b8d8", "terminal-bright-cyan": "#9dd6cf",
  });
});

test("the six retired accent tokens alias their neutral replacements in :root and no theme rule sets them", () => {
  assert.deepEqual(Object.fromEntries(Object.keys(ACCENT_ALIASES).map((name) => [name, LIGHT[name]])),
    Object.fromEntries(Object.entries(ACCENT_ALIASES).map(([name, target]) => [name, `var(--${target})`])));
  for (const [name, target] of Object.entries(ACCENT_ALIASES)) {
    assert.equal(resolve(DARK, name), resolve(DARK, target), `--${name} reads dark --${target} in the dark theme`);
  }
  assert.deepEqual(Object.keys(block(DARK_RULE)).filter((name) => name in ACCENT_ALIASES), []);
});

test("the system dark theme and the chosen dark theme set the same tokens, and only the ones that differ by theme", () => {
  const media = css.slice(css.indexOf("@media (prefers-color-scheme: dark)"));
  const system = media.slice(media.indexOf(':root:not([data-theme="light"]) {'));
  const body = system.slice(system.indexOf("{") + 1, system.indexOf("}"));
  const tokens = Object.fromEntries([...body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
  assert.deepEqual(tokens, block(DARK_RULE));
  const themed = new Set([...Object.keys(QUIET_DARK).filter((name) => !ROOT_ONLY.includes(name)), ...STATE]);
  assert.deepEqual(Object.keys(block(DARK_RULE)).sort(), [...themed].sort());
});

test("the agent terminal keeps one dark palette in every theme, with its own focus color", () => {
  const names = Object.keys(LIGHT).filter((name) => name.startsWith("terminal-")).sort();
  assert.deepEqual(names, [
    "terminal-bg", "terminal-black", "terminal-blue", "terminal-border",
    "terminal-bright-black", "terminal-bright-blue", "terminal-bright-cyan", "terminal-bright-green",
    "terminal-bright-magenta", "terminal-bright-red", "terminal-bright-white", "terminal-bright-yellow",
    "terminal-cursor", "terminal-cyan", "terminal-fg", "terminal-focus", "terminal-green",
    "terminal-magenta", "terminal-muted", "terminal-red", "terminal-scrollbar",
    "terminal-scrollbar-hover", "terminal-selection", "terminal-surface", "terminal-white",
    "terminal-yellow",
  ]);
  assert.deepEqual(Object.keys(block(DARK_RULE)).filter((name) => name.startsWith("terminal-")), []);
});

test("the console's and embedded terminals' focus border and rings use --terminal-focus, since the light --focus is 1.06 on the terminal", () => {
  const consoleCss = read("../src/screens/project/console.css");
  assert.match(consoleCss, /\.cs:focus-within \{\s*border-color: var\(--terminal-focus\);\s*\}/);
  assert.match(consoleCss, /section\.cs :focus-visible \{\s*outline-color: var\(--terminal-focus\);\s*\}/);
  const graphCss = read("../src/screens/team/team-graph.css");
  assert.match(graphCss, /\.tg-terminal\.embedded :focus-visible \{\s*outline-color: var\(--terminal-focus\);\s*\}/,
    "the terminal a console or a Terminals wall tile embeds rings its composer on --terminal-focus");
  assert.ok(ratio(paint(LIGHT, "focus"), paint(LIGHT, "terminal-bg")) < CONTROL, "the light page focus would vanish on the terminal");
});

test("every terminal-panel.tsx token() fallback equals the token's value in tokens.css", () => {
  const panel = read("../src/screens/team/terminal-panel.tsx");
  const fallbacks = [...panel.matchAll(/token\("--(terminal-[a-z0-9-]+)", "([^"]+)"\)/g)].map((match) => [match[1], match[2]]);
  assert.equal(fallbacks.length, 24);
  assert.deepEqual(fallbacks.filter(([name, value]) => LIGHT[name] !== value), []);
});

test("brand, fonts, the type scale and the weights live in tokens, and style.css sets no literal color", () => {
  assert.match(LIGHT.font, /^"Instrument Sans"/);
  assert.match(LIGHT.mono, /^"JetBrains Mono"/);
  assert.deepEqual(Object.fromEntries(Object.keys(TYPE_SCALE).map((name) => [name, LIGHT[name]])), TYPE_SCALE);
  assert.deepEqual(Object.fromEntries(Object.keys(WEIGHTS).map((name) => [name, LIGHT[name]])), WEIGHTS);
  const style = read("../src/style.css");
  assert.doesNotMatch(style, /#[0-9a-fA-F]{3,8}\b/, "style.css must use tokens, not literal colors");
  assert.doesNotMatch(style, /font-family:(?!\s*var\(--(?:font|mono)\))/, "style.css must use font tokens");
  assert.doesNotMatch(style, /rgba?\(/, "style.css must take shadows and scrims from tokens");
});
