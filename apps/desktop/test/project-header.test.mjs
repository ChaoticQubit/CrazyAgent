import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const head = await import("../src/screens/project/head.tsx");
const css = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
const index = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
const source = readFileSync(new URL("../src/screens/project/head.tsx", import.meta.url), "utf8");

const ABOUT = `${"A booking web app for small studios: a live calendar, deposits at checkout and reminders by email. ".repeat(6)}\nV2 adds recurring classes and package passes.`;
const TABS = [
  { id: "overview", label: "Overview", href: "#/projects/larkspur/overview" },
  { id: "settings", label: "Settings", href: "#/projects/larkspur/settings" },
];

/** Renders the project head the way the project page draws it. Inputs: whether the page has
 * scrolled and the description. Output: the static markup. */
function markup(scrolled, about = ABOUT) {
  return renderToStaticMarkup(createElement(head.ProjectHead, {
    title: "Larkspur Booking", about, actions: null, tabs: TABS, current: "overview", scrolled,
  }));
}

test("the head shrinks once the page scrolls past 8 px and not before", () => {
  assert.ok(ABOUT.length >= 600, "the description is a long one");
  assert.deepEqual([0, 8, 9, 2000].map((top) => head.headScrolled(top)), [false, false, true, true]);
});

test("the title, the whole description with its more toggle and the tab bar sit in one head", () => {
  const drawn = markup(false);
  assert.ok(drawn.startsWith("<div class=\"pj-head\">") && drawn.endsWith("</div>"), drawn.slice(0, 200));
  assert.ok(drawn.includes("Larkspur Booking"), "the title");
  assert.ok(drawn.indexOf("role=\"tablist\"") > drawn.indexOf("Larkspur Booking"), "the tab bar sits in the head under the title");
  assert.match(drawn, /<span id="pj-desc" class="pj-desc">A booking web app/u);
  assert.ok(drawn.includes("V2 adds recurring classes and package passes."), "the description keeps every line, not only the first");
  assert.ok(!drawn.includes("pj-truncate"), "the description wraps instead of being cut to one line");
  assert.match(drawn, /<button type="button" class="pj-desc-more" aria-expanded="false" aria-controls="pj-desc"[^>]*>more<\/button>/u);
});

test("a scrolled head carries the scrolled class", () => {
  assert.ok(markup(true).startsWith("<div class=\"pj-head scrolled\">"), markup(true).slice(0, 80));
});

test("a project with no description shows neither a description nor a more toggle", () => {
  const drawn = markup(false, "");
  assert.ok(drawn.startsWith("<div class=\"pj-head\">"), drawn.slice(0, 80));
  assert.ok(!drawn.includes("pj-desc"), drawn);
});

test("the head stays on screen and the description takes two lines, one once the page scrolls", () => {
  assert.match(css, /\.pj-head \{[^}]*position: sticky;[^}]*top: 0;/u);
  assert.match(css, /\.pj-desc \{[^}]*-webkit-line-clamp: 2;/u);
  assert.match(css, /\.pj-head\.scrolled \.pj-desc \{[^}]*-webkit-line-clamp: 1;/u);
  assert.match(css, /\.pj-desc\.open \{[^}]*-webkit-line-clamp: unset;/u);
});

test("the project page draws its head through ProjectHead and follows the page scroll", () => {
  assert.match(index, /<ProjectHead /u);
  assert.match(index, /useProjectHead\(/u);
  assert.match(source, /headScrolled\(page\.scrollTop, was\)/u);
});

test("a scrolled head keeps its scrolled form until the page is nearly back at the top", () => {
  assert.deepEqual([[5, false], [9, false], [5, true], [3, true], [2, true], [0, true]].map(([top, was]) => head.headScrolled(top, was)),
    [false, true, true, true, false, false]);
});

test("the room the scrolled head gives up stays under the page, so the scroll range never shrinks with the head", () => {
  assert.equal(typeof head.headRoom, "function", "head.tsx exports headRoom");
  assert.deepEqual([[120, 96, true], [120, 96, false], [96, 120, true], [120, 95.5, true]].map(([full, now, scrolled]) => head.headRoom(full, now, scrolled)),
    [24, 0, 0, 25]);
  assert.match(css, /\.pj-project::after \{[^}]*height: var\(--pj-head-room, 0px\);/u);
  assert.match(index, /useProjectHead\(root, projectId\)/u);
});

test("the scroll container keeps a focused control clear of the head by the head's own height", () => {
  assert.match(css, /\.main \{[^}]*scroll-padding-top: var\(--pj-head-h, 0px\);/u);
  assert.match(source, /setProperty\("--pj-head-h", `\$\{height\}px`\)/u);
  assert.match(source, /removeProperty\("--pj-head-h"\)/u);
});
