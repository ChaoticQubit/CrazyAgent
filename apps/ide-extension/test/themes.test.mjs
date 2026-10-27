import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const themes = {
  dark: JSON.parse(read("../themes/crazyagent-dark.json")),
  light: JSON.parse(read("../themes/crazyagent-light.json")),
};
const HEX = /^#[0-9a-f]{6}([0-9a-f]{2})?$/;
const AA = 4.5;

const PAIRS = [
  ["editor.foreground", "editor.background"],
  ["sideBar.foreground", "sideBar.background"],
  ["sideBarTitle.foreground", "sideBar.background"],
  ["sideBarSectionHeader.foreground", "sideBarSectionHeader.background"],
  ["statusBar.foreground", "statusBar.background"],
  ["statusBar.noFolderForeground", "statusBar.noFolderBackground"],
  ["activityBar.foreground", "activityBar.background"],
  ["activityBar.inactiveForeground", "activityBar.background"],
  ["tab.activeForeground", "tab.activeBackground"],
  ["tab.inactiveForeground", "tab.inactiveBackground"],
  ["list.activeSelectionForeground", "list.activeSelectionBackground"],
  ["list.inactiveSelectionForeground", "list.inactiveSelectionBackground"],
  ["input.foreground", "input.background"],
  ["dropdown.foreground", "dropdown.background"],
  ["button.foreground", "button.background"],
  ["button.secondaryForeground", "button.secondaryBackground"],
  ["badge.foreground", "badge.background"],
  ["activityBarBadge.foreground", "activityBarBadge.background"],
  ["titleBar.activeForeground", "titleBar.activeBackground"],
  ["panelTitle.activeForeground", "panel.background"],
  ["terminal.foreground", "terminal.background"],
  ["editorLineNumber.activeForeground", "editor.background"],
  ["descriptionForeground", "sideBar.background"],
];

const channels = (hex) => [1, 3, 5, 7].map((at) => (hex.length > at ? parseInt(hex.slice(at, at + 2), 16) : 255));

function luminance([r, g, b]) {
  const linear = (value) => {
    const c = value / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

/** The WCAG contrast ratio of a colour, blended by its alpha, on an opaque background.
 * Inputs: both as #rrggbb or #rrggbbaa. Output: the ratio. */
function contrast(foreground, background) {
  const [br, bg, bb, ba] = channels(background);
  assert.equal(ba, 255, `background ${background} must be opaque`);
  const [fr, fg, fb, fa] = channels(foreground);
  const mix = (front, back) => Math.round((front * fa + back * (255 - fa)) / 255);
  const [lighter, darker] = [luminance([mix(fr, br), mix(fg, bg), mix(fb, bb)]), luminance([br, bg, bb])].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

/** Reads one token block of the desktop palette. Input: the CSS selector line it opens with.
 * Output: the token names and their hex values. */
function tokens(css, opening) {
  const start = css.indexOf(opening);
  assert.notEqual(start, -1, opening);
  const block = css.slice(start, css.indexOf("}", start));
  return Object.fromEntries([...block.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-f]{6});/g)].map(([, name, value]) => [name, value]));
}

test("the CrazyAgent themes carry their fixed labels and types, with valid colours only", () => {
  assert.equal(themes.dark.name, "CrazyAgent Dark");
  assert.equal(themes.dark.type, "dark");
  assert.equal(themes.light.name, "CrazyAgent Light");
  assert.equal(themes.light.type, "light");
  for (const [kind, theme] of Object.entries(themes)) {
    assert.ok(Object.keys(theme.colors).length >= 40, kind);
    for (const [key, value] of Object.entries(theme.colors)) assert.match(value, HEX, `${kind} ${key}`);
    assert.ok(theme.tokenColors.length >= 10, kind);
    for (const rule of theme.tokenColors) {
      assert.ok(rule.scope, `${kind} ${rule.name}`);
      if (rule.settings.foreground) assert.match(rule.settings.foreground, HEX, `${kind} ${rule.name}`);
    }
    assert.deepEqual(Object.keys(theme.colors).sort(), Object.keys(themes.dark.colors).sort(), kind);
  }
});

test("every listed foreground meets WCAG AA 4.5:1 on its background in both themes", () => {
  for (const [kind, theme] of Object.entries(themes)) {
    for (const [front, back] of PAIRS) {
      assert.ok(theme.colors[front], `${kind} ${front}`);
      assert.ok(theme.colors[back], `${kind} ${back}`);
      const ratio = contrast(theme.colors[front], theme.colors[back]);
      assert.ok(ratio >= AA, `${kind} ${front} on ${back}: ${ratio.toFixed(2)}:1`);
    }
  }
});

test("every syntax colour meets WCAG AA 4.5:1 on the editor background", () => {
  for (const [kind, theme] of Object.entries(themes)) {
    for (const rule of theme.tokenColors.filter((item) => item.settings.foreground)) {
      const ratio = contrast(rule.settings.foreground, theme.colors["editor.background"]);
      assert.ok(ratio >= AA, `${kind} ${rule.name}: ${ratio.toFixed(2)}:1`);
    }
  }
});

test("the themes take their grounds, text and accent from the desktop palette", () => {
  const css = read("../../desktop/src/ui/tokens.css");
  const light = tokens(css, ":root {");
  const dark = tokens(css, ':root[data-theme="dark"] {');
  const expected = {
    dark: {
      "editor.background": dark.bg,
      "sideBar.background": dark.surface,
      "input.background": dark["surface-2"],
      "editor.foreground": dark.text,
      "sideBar.foreground": dark["text-2"],
      "focusBorder": dark.focus,
      "button.background": dark["accent-solid"],
      "button.foreground": dark["on-accent"],
      "list.activeSelectionBackground": dark["accent-soft"],
      "textLink.foreground": dark["accent-text"],
      "terminal.background": light["terminal-bg"],
      "terminal.foreground": light["terminal-fg"],
      "terminalCursor.foreground": light["terminal-cursor"],
      "errorForeground": dark.danger,
    },
    light: {
      "editor.background": light.surface,
      "sideBar.background": light.bg,
      "input.background": light.surface,
      "editor.foreground": light.text,
      "sideBar.foreground": light["text-2"],
      "focusBorder": light.focus,
      "button.background": light["accent-solid"],
      "button.foreground": light["on-accent"],
      "list.activeSelectionBackground": light["accent-soft"],
      "textLink.foreground": light["accent-text"],
      "errorForeground": light.danger,
    },
  };
  for (const [kind, keys] of Object.entries(expected)) {
    for (const [key, value] of Object.entries(keys)) {
      assert.ok(value, `${kind} token for ${key}`);
      assert.equal(themes[kind].colors[key], value, `${kind} ${key}`);
    }
  }
});

test("the extension contributes both themes under their fixed labels", () => {
  const manifest = JSON.parse(read("../package.json"));
  assert.deepEqual(manifest.contributes.themes, [
    { label: "CrazyAgent Dark", uiTheme: "vs-dark", path: "./themes/crazyagent-dark.json" },
    { label: "CrazyAgent Light", uiTheme: "vs", path: "./themes/crazyagent-light.json" },
  ]);
  for (const entry of manifest.contributes.themes) {
    const theme = JSON.parse(read(`.${entry.path}`));
    assert.equal(theme.name, entry.label);
    assert.equal(theme.type, entry.uiTheme === "vs-dark" ? "dark" : "light");
  }
});
