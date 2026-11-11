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
function markup(scrolled, about = ABOUT, extra = {}) {
  return renderToStaticMarkup(createElement(head.ProjectHead, {
    title: "Larkspur Booking", about, actions: null, tabs: TABS, current: "overview", scrolled, ...extra,
  }));
}

const META = { folder: "~/CrazyAgent/larkspur-booking", branch: "main", access: "Full access", pool: "Pool A, 4 accounts" };
const words = (drawn) => drawn.replace(/<[^>]*>/gu, " ").replace(/\s+/gu, " ").trim();

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

test("a head still taking its scrolled form holds the room at its full height, and trims it once settled", () => {
  assert.deepEqual([[120, 120, true, false], [120, 104, true, false], [120, 104, true, true], [120, 104, false, false], [119.5, 119.5, true, false]]
    .map(([full, now, scrolled, settled]) => head.headRoom(full, now, scrolled, settled)), [120, 120, 16, 0, 120]);
});

test("the head settles on its own transitions ending, never on a timer", () => {
  assert.match(source, /addEventListener\("transitionend", sync\)/u);
  assert.match(source, /addEventListener\("transitioncancel", sync\)/u);
  assert.match(source, /removeEventListener\("transitionend", sync\)/u);
  assert.match(source, /removeEventListener\("transitioncancel", sync\)/u);
  assert.match(source, /getAnimations\(\{ subtree: true \}\)/u);
  assert.doesNotMatch(source, /setTimeout|setInterval|requestAnimationFrame/u);
});

test("only the project page turns scroll anchoring off, so the head's shrink never pulls the page back to the top", () => {
  assert.match(css, /\.main:has\(\.pj-project\) \{ overflow-anchor: none; \}/u);
  assert.doesNotMatch(css, /(^|\n)\.main \{[^}]*overflow-anchor/u);
});

test("the scroll container keeps a focused control clear of the head by the head's own height", () => {
  assert.match(css, /\.main \{[^}]*scroll-padding-top: var\(--pj-head-h, 0px\);/u);
  assert.match(source, /setProperty\("--pj-head-h", `\$\{height\}px`\)/u);
  assert.match(source, /removeProperty\("--pj-head-h"\)/u);
});

test("the head names the project folder, its branch, the agents' access and the pool in one meta line under the description (OS-1)", () => {
  const drawn = markup(false, ABOUT, { meta: META, working: 5, questions: 1 });
  const line = drawn.match(/<p class="pj-meta"[^>]*>(.*?)<\/p>/u);
  assert.ok(line, `a meta line: ${drawn.slice(0, 600)}`);
  assert.equal(words(line[1]), "~/CrazyAgent/larkspur-booking main Full access Pool A, 4 accounts");
  assert.ok(drawn.indexOf("pj-meta") > drawn.indexOf("pj-desc") && drawn.indexOf("pj-meta") < drawn.indexOf("role=\"tablist\""), "under the description, above the tabs");
  const partial = markup(false, ABOUT, { meta: { ...META, branch: null, pool: null } });
  assert.equal(words(partial.match(/<p class="pj-meta"[^>]*>(.*?)<\/p>/u)[1]), "~/CrazyAgent/larkspur-booking Full access", "an unknown part is left out");
  assert.ok(!markup(false).includes("pj-meta"), "no meta line before the project's facts are read");
});

test("next to the title the head shows how many agents work and how many questions wait, each only when there are any", () => {
  assert.deepEqual(head.headChips(5, 1), [{ word: "5 agents working", tone: "ok" }, { word: "1 question", tone: "warn" }]);
  assert.deepEqual(head.headChips(1, 2), [{ word: "1 agent working", tone: "ok" }, { word: "2 questions", tone: "warn" }]);
  assert.deepEqual(head.headChips(0, 0), []);
  assert.throws(() => head.headChips(-1, 0), /head counts are whole numbers/u);
  assert.throws(() => head.headChips(1.5, 0), /head counts are whole numbers/u);
  const drawn = markup(false, ABOUT, { meta: META, working: 5, questions: 1 });
  const title = drawn.indexOf("Larkspur Booking");
  assert.ok(drawn.indexOf("5 agents working") > title && drawn.indexOf("1 question") > drawn.indexOf("5 agents working"), "the chips follow the title");
  assert.ok(drawn.indexOf("1 question") < drawn.indexOf("pj-desc"), "the chips sit on the title's line, above the description");
  assert.match(drawn, /<h1>Larkspur Booking<\/h1>/u, "the chips are not part of the heading");
  assert.ok(!markup(false, ABOUT, { meta: META, working: 0, questions: 0 }).includes("working"), "no chip when nothing works or waits");
});

test("the meta line hides once the page scrolls, as the description drops to one line", () => {
  assert.match(css, /\.pj-head\.scrolled \.page-header p\.pj-meta \{[^}]*display: none;/u);
});

test("the project page gives its head the meta line and the working and question counts", () => {
  assert.match(index, /<ProjectHead [\s\S]*?\bmeta=\{meta\}/u);
  assert.match(index, /working=\{/u);
  assert.match(index, /questions=\{/u);
});
