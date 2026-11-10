import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (name) => readFileSync(new URL(`../src/screens/project/${name}`, import.meta.url), "utf8");
const index = read("index.tsx");
const projects = read("projects.css");
const activity = read("team-activity.css");

test("only the Conversation tab makes the project page a fixed screen, with every banner in one box between the head and the tab panel (DEC-125)", () => {
  assert.match(index, /<div className=\{`pj pj-project\$\{active === "conversation" \? " pj-fixed" : ""\}`\} ref=\{root\}>/u);
  assert.equal(index.match(/pj-fixed/gu)?.length, 1, "no other tab sets the fixed screen");
  assert.match(index, /\}\)\)\} \/>\s*<div className="pj-banners">\s*<RecoveryBanner /u, "the box opens right after the head");
  assert.match(index, /<InstructionImportPrompt [^\n]*\/>\s*<\/div>\s*<div className="pj-tab-panel"/u);
});

test("the fixed screen never scrolls: the main region hides its overflow and the page is the window's height", () => {
  assert.match(projects, /\.main:has\(> \.page > \.pj-fixed\) \{ overflow: hidden; \}/u);
  assert.match(projects, /\.main:has\(> \.page > \.pj-fixed\) > \.page \{[^}]*flex-direction: column;[^}]*height: 100%;/u);
  assert.match(projects, /\.pj-fixed \{[^}]*flex-direction: column;[^}]*flex: 1;[^}]*min-height: 0;/u);
  assert.match(projects, /\.pj-fixed::after \{ display: none; \}/u);
});

test("the tab panel takes the rest of the window down to a 240px floor, and the banners scroll on their own only when the window is too short", () => {
  assert.match(projects, /\.pj-fixed > \.pj-tab-panel \{[^}]*flex: 1 1 0;[^}]*min-height: 240px;/u);
  assert.match(projects, /\.pj-fixed > \.pj-banners \{[^}]*flex: 0 1 auto;[^}]*min-height: 0;[^}]*overflow: auto;/u);
});

test("the master and the Team bar fill the panel's height instead of a guess at the window's", () => {
  assert.match(projects, /\.pj-fixed \.ta-split \{[^}]*flex: 1;[^}]*min-height: 0;[^}]*grid-template-rows: minmax\(0, 1fr\);/u);
  assert.match(projects, /\.pj-fixed \.ta-main \{[^}]*flex-direction: column;[^}]*min-height: 0;/u);
  assert.match(projects, /\.pj-fixed \.ta-main > :is\(\.pj-master-live, \.pj-convo\) \{[^}]*flex: 1;[^}]*height: auto;[^}]*min-height: 0;/u);
});

test("below 1000px the Team bar narrows to 240px beside the master and never stacks under it, and an open agent terminal keeps its wider split", () => {
  const narrow = activity.slice(activity.indexOf("@media (max-width: 1000px)"));
  assert.match(narrow, /^@media \(max-width: 1000px\) \{\s*\.ta-split \{\s*grid-template-columns: minmax\(0, 1fr\) 240px;\s*\}\s*\}/u);
  assert.doesNotMatch(activity, /height: 360px/u);
  assert.match(activity, /\.ta-split\.with-terminal \{\s*grid-template-columns: minmax\(0, 1fr\) minmax\(420px, 1fr\);\s*\}/u);
});
