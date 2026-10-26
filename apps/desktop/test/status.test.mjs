import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { StatusBadge, statusTone, statusIcon } = await import("../src/ui/feedback.tsx");
const { ICON_NAMES } = await import("../src/ui/icon.tsx");

const TABLE = [
  [["Signed in", "Connected", "Ready", "Done", "Approved", "Saved", "Research done", "Replied"], "circle-check", "ok"],
  [["Key saved"], "key-round", "ok"],
  [["Not signed in", "Needs a pool", "Needs approval", "Needs you", "Rate limited until 12:32 AM"], "circle-alert", "warn"],
  [["Limited control"], "triangle-alert", "warn"],
  [["Can't reach this URL", "Failed", "Can't start"], "circle-x", "danger"],
  [["Running", "Signing in...", "Starting", "Thinking...", "Writing"], "loader-circle", "info"],
  [["Queued", "Starts after Harbor Notes"], "clock", "neutral"],
  [["Waiting for a free account"], "hourglass", "neutral"],
  [["Stopped"], "circle-stop", "neutral"],
  [["Turned off", "Not installed"], "power-off", "neutral"],
  [["Suggested"], "lightbulb", "neutral"],
];

test("every status word from the design icon table gets its icon and tone", () => {
  for (const [words, icon, tone] of TABLE) {
    assert.ok(ICON_NAMES.includes(icon), `${icon} is vendored`);
    for (const word of words) {
      assert.equal(statusIcon(word), icon, word);
      assert.equal(statusTone(word), tone, word);
    }
  }
});

test("a status word without an entry renders as text only, and the word is always shown", () => {
  assert.equal(statusIcon("Mystery state"), undefined);
  assert.equal(statusTone("Mystery state"), "neutral");
  const plain = renderToStaticMarkup(createElement(StatusBadge, { word: "Mystery state" }));
  assert.equal(plain, '<span class="badge">Mystery state</span>');
  const signedIn = renderToStaticMarkup(createElement(StatusBadge, { word: "Key saved" }));
  assert.match(signedIn, /^<span class="badge tone-ok"><svg[^>]*class="icon"[\s\S]*<\/svg>Key saved<\/span>$/);
  const live = renderToStaticMarkup(createElement(StatusBadge, { word: "Signing in..." }));
  assert.match(live, /class="icon spin"/);
  assert.doesNotMatch(renderToStaticMarkup(createElement(StatusBadge, { word: "Queued" })), /spin/);
});

test("the spinner only turns when the system allows motion", () => {
  const css = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
  const rule = css.indexOf(".icon.spin {");
  assert.ok(rule > 0);
  const media = css.lastIndexOf("@media (prefers-reduced-motion: no-preference)", rule);
  assert.ok(media >= 0 && css.slice(media, rule).split("}").length <= 2, ".icon.spin sits inside the no-preference media block");
});

test("projects folder, onboarding, setup, pack and team words carry their icons", () => {
  for (const [words, icon, tone] of [
    [["Found"], "search", "info"],
    [["Setting up"], "list-checks", "warn"],
    [["Folder missing"], "circle-x", "danger"],
    [["Working", "Installing"], "loader-circle", "info"],
    [["Waiting"], "circle-dashed", "neutral"],
    [["Idle"], "circle-pause", "neutral"],
    [["Sent"], "send", "neutral"],
    [["Installed", "On"], "circle-check", "ok"],
    [["Off"], "power-off", "neutral"],
  ]) {
    assert.ok(ICON_NAMES.includes(icon), `${icon} is vendored`);
    for (const word of words) {
      assert.equal(statusIcon(word), icon, word);
      assert.equal(statusTone(word), tone, word);
    }
  }
  assert.equal(statusIcon("Waiting for a free account"), "hourglass");
  assert.equal(statusIcon("Found 3.4"), undefined);
  assert.equal(statusIcon("Online"), undefined);
  for (const name of ["folder-input", "git-branch", "wrench", "blocks"]) assert.ok(ICON_NAMES.includes(name), `${name} is vendored`);
  assert.match(renderToStaticMarkup(createElement(StatusBadge, { word: "Installing" })), /class="icon spin"/);
});
