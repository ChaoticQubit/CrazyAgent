import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");
const overview = await import("../src/screens/project/overview.tsx");

const item = (id, at_ms, goal, extra = {}) => ({
  id, at_ms, kind: "accepted", actor: "Owner", goal, text: `Event ${id}`, change_set: null, undone: false, ...extra,
});

test("activity groups by goal, newest group first, and items without a goal go under Project", () => {
  const items = [
    item(6, 6_000, "Build the service"),
    item(5, 5_000, null),
    item(4, 4_000, "Write the docs"),
    item(3, 3_000, "Build the service"),
    item(2, 2_000, null),
    item(1, 1_000, "Write the docs"),
  ];
  const groups = research.groupActivity(items);
  assert.deepEqual(groups.map((group) => group.goal), ["Build the service", "Project", "Write the docs"]);
  assert.deepEqual(groups.map((group) => group.items.map((entry) => entry.id)), [[6, 3], [5, 2], [4, 1]]);
});

test("a group is ordered by its latest item even when the read comes out of order", () => {
  const groups = research.groupActivity([
    item(1, 1_000, "Old goal"),
    item(2, 9_000, "Old goal"),
    item(3, 5_000, "New goal"),
    item(4, 5_000, "New goal"),
    item(5, 2_000, ""),
  ]);
  assert.deepEqual(groups.map((group) => group.goal), ["Old goal", "New goal", "Project"]);
  assert.deepEqual(groups[0].items.map((entry) => entry.id), [2, 1]);
  assert.deepEqual(groups[1].items.map((entry) => entry.id), [4, 3], "same time: the later id is newer");
  assert.deepEqual(research.groupActivity([]), []);
});

test("each goal shows eight rows with its actor and Show all for the rest", () => {
  const now = 10 * 60_000;
  const items = [
    ...Array.from({ length: 10 }, (_, i) => item(100 + i, now - i * 60_000, "Build the service", { actor: "Wren" })),
    item(1, now - 30 * 60_000, null, { kind: "research_saved", actor: "Research", text: "Market research saved market_research.md" }),
  ];
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, now, expanded: [], onShowAll: () => {} }));
  assert.equal(html.match(/class="pj-activity-row/g)?.length, 9);
  assert.match(html, /aria-label="Build the service"/);
  assert.match(html, /aria-label="Project"/);
  assert.match(html, />Show all \(10\)</);
  assert.equal(html.match(/>Show all/g)?.length, 1, "a goal with eight rows or fewer has no Show all");
  assert.match(html, /Market research saved market_research\.md/);
  assert.match(html, />Wren</);
  assert.match(html, />Just now</);
  assert.ok(html.indexOf("Build the service") < html.indexOf("Market research saved"), "the newest goal comes first");
  const open = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, now, expanded: ["Build the service"], onShowAll: () => {} }));
  assert.equal(open.match(/class="pj-activity-row/g)?.length, 11);
  assert.doesNotMatch(open, />Show all/);
});

test("an undone event says so", () => {
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, {
    items: [item(1, 0, "Build the service", { kind: "lead_merged", text: "Master merged change set #17", undone: true })],
    now: 0, expanded: [], onShowAll: () => {},
  }));
  assert.match(html, /Master merged change set #17/);
  assert.match(html, />Undone</);
});

test("Overview reads every project event, not the research jobs", async () => {
  const { readFileSync } = await import("node:fs");
  const source = readFileSync(new URL("../src/screens/project/overview.tsx", import.meta.url), "utf8");
  assert.ok(source.includes("read: load = loadActivity"), "Overview reads the engine activity unless a test hands it a read");
  assert.equal(typeof research.activity, "undefined", "the research-only activity list is gone");
});

test("a goal named Project is its own group, apart from events with no goal", () => {
  const groups = research.groupActivity([item(2, 2_000, "Project"), item(1, 1_000, null)]);
  assert.deepEqual(groups.map((group) => group.goal), ["Project", "Project"]);
  assert.equal(new Set(groups.map((group) => group.key)).size, 2, "the two groups have different keys");
  assert.deepEqual(groups.map((group) => group.items.map((entry) => entry.id)), [[2], [1]]);
});

test("a read the engine cut counts each goal with a plus", () => {
  const now = 10 * 60_000;
  const items = Array.from({ length: 10 }, (_, i) => item(100 + i, now - i * 60_000, "Build the service"));
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: true, now, expanded: [], onShowAll: () => {} }));
  assert.match(html, /10\+ items, latest just now/);
  assert.match(html, />Show all \(10\+\)</);
  const whole = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: false, now, expanded: [], onShowAll: () => {} }));
  assert.match(whole, /10 items, latest just now/);
});

test("the project's creation shows under Project", () => {
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, {
    items: [item(1, 0, null, { kind: "created", text: "You created Harbor Notes" })], more: false, now: 0, expanded: [], onShowAll: () => {},
  }));
  assert.match(html, /aria-label="Project"/);
  assert.match(html, /You created Harbor Notes/);
});

const KINDS = ["created", "research_started", "research_saved", "research_stopped", "task_started", "task_finished", "changes_ready", "accepted", "discarded", "conflict", "sent_back", "lead_merged", "merged_into_main", "undone", "work_started", "work_review_ready", "work_complete", "work_closed", "phase", "test"];

test("every kind falls under one filter chip, and the project's creation only under All", () => {
  assert.deepEqual(research.ACTIVITY_FILTERS.map((filter) => filter.label), ["All", "Merges", "Changes", "Tests", "Tasks", "Phases"]);
  const under = Object.fromEntries(KINDS.map((kind) => [kind, research.activityFilter(kind)]));
  assert.deepEqual(under, {
    created: null, research_started: "task", research_saved: "task", research_stopped: "task", task_started: "task", task_finished: "task",
    changes_ready: "change", accepted: "change", discarded: "change", conflict: "change", sent_back: "change",
    lead_merged: "merge", merged_into_main: "merge", undone: "merge",
    work_started: "task", work_review_ready: "task", work_complete: "task", work_closed: "task", phase: "phase", test: "tests",
  });
});

test("research with no goal is its own Research group, apart from Project and from a goal named Research", () => {
  const groups = research.groupActivity([
    item(4, 4_000, "Research"),
    item(3, 3_000, null, { kind: "research_saved" }),
    item(2, 2_000, null, { kind: "created" }),
    item(1, 1_000, "  ", { kind: "research_started" }),
  ]);
  assert.deepEqual(groups.map((group) => group.goal), ["Research", "Research", "Project"]);
  assert.deepEqual(groups.map((group) => group.items.map((entry) => entry.id)), [[4], [3, 1], [2]]);
  assert.equal(new Set(groups.map((group) => group.key)).size, 3, "each group has its own key");
  assert.equal(groups[1].key, research.RESEARCH_KEY, "Research events with no goal share the research key");
});
