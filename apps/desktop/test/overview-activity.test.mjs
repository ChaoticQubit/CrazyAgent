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
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  assert.equal(html.match(/class="pj-activity-row/g)?.length, 9);
  assert.match(html, /aria-label="Build the service"/);
  assert.match(html, /aria-label="Research"/, "research with no goal shows under Research");
  assert.match(html, />Show all \(10\)</);
  assert.equal(html.match(/>Show all/g)?.length, 1, "a goal with eight rows or fewer has no Show all");
  assert.match(html, /Market research saved market_research\.md/);
  assert.match(html, />Wren</);
  assert.match(html, />Just now</);
  assert.ok(html.indexOf("Build the service") < html.indexOf("Market research saved"), "the newest goal comes first");
  const open = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, now, expanded: ["goal:Build the service"], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  assert.equal(open.match(/class="pj-activity-row/g)?.length, 11);
  assert.doesNotMatch(open, />Show all/);
});

test("an undone event says so", () => {
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, {
    items: [item(1, 0, "Build the service", { kind: "lead_merged", text: "Master merged change set #17", undone: true })],
    now: 0, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {},
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
  assert.deepEqual(groups.map((group) => group.goal), ["Project", "Project (no goal)"]);
  assert.equal(new Set(groups.map((group) => group.key)).size, 2, "the two groups have different keys");
  assert.deepEqual(groups.map((group) => group.items.map((entry) => entry.id)), [[2], [1]]);
});

test("a read the engine cut counts each goal with a plus", () => {
  const now = 10 * 60_000;
  const items = Array.from({ length: 10 }, (_, i) => item(100 + i, now - i * 60_000, "Build the service"));
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: true, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  assert.match(html, /10\+ items · last just now/);
  assert.match(html, />Show all \(10\+\)</);
  const whole = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: false, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  assert.match(whole, /10 items · last just now/);
});

test("the project's creation shows under Project", () => {
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, {
    items: [item(1, 0, null, { kind: "created", text: "You created Harbor Notes" })], more: false, now: 0, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {},
  }));
  assert.match(html, /aria-label="Project"/);
  assert.match(html, /You created Harbor Notes/);
});

const KINDS = ["created", "research_started", "research_saved", "research_stopped", "task_started", "task_finished", "changes_ready", "accepted", "discarded", "conflict", "sent_back", "lead_merged", "merged_into_main", "undone", "work_started", "work_review_ready", "work_complete", "work_closed", "phase", "test"];

test("every kind falls under one filter chip, and the project's creation only under All", () => {
  assert.ok(Array.isArray(research.ACTIVITY_FILTERS) && typeof research.activityFilter === "function", "the chips and their rule come from research.ts");
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
  assert.deepEqual(groups.map((group) => group.goal), ["Research", "Research (no goal)", "Project"]);
  assert.deepEqual(groups.map((group) => group.items.map((entry) => entry.id)), [[4], [3, 1], [2]]);
  assert.equal(new Set(groups.map((group) => group.key)).size, 3, "each group has its own key");
  assert.equal(groups[1].key, research.RESEARCH_KEY, "Research events with no goal share the research key");
});

test("a goal's header shows its phase path, done steps checked and the latest step current", () => {
  const now = 10 * 60_000;
  const phase = (id, at, text) => item(id, at, "Build the service", { kind: "phase", text });
  const items = [
    phase(3, now, "Building: endpoints are in"),
    phase(2, now - 60_000, "Architecture done: one adapter"),
    phase(1, now - 2 * 60_000, "Architecture: adapter drafted"),
    item(4, now - 5 * 60_000, "Write the docs", { text: "You accepted the guide" }),
  ];
  assert.equal(typeof research.phasePath, "function", "the phase path is read from the goal's phase events");
  assert.deepEqual(research.phasePath(items.slice(0, 3)), [{ step: "Architecture", done: true }, { step: "Building", done: false }]);
  assert.deepEqual(research.phasePath([]), []);
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: false, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  assert.match(html, /<span class="pj-activity-path"[^>]*><span class="done">✓ Architecture<\/span><span class="now">Building<\/span><\/span>/);
  assert.equal(html.match(/class="pj-activity-path"/g)?.length, 1, "a goal with no phase shows no path");
  assert.match(html, /3 items · last just now/);
  assert.match(html, /1 item · last 5 min ago/i);
});

test("an activity row names who acted as the mockup does: You for the owner, the master merging by itself at Full access (AL2)", () => {
  const now = Date.UTC(2026, 10, 8, 7, 0);
  const items = [
    item(3, now, null, { kind: "merged_into_main", actor: "Master", text: "Master merged change set #15 (Juniper: calendar API) into main" }),
    item(2, now - 60_000, null, { kind: "accepted", actor: "Owner", text: "You accepted change set #14 (Wren: checkout UI, 9 files)" }),
    item(1, now - 2 * 60_000, null, { kind: "task_started", actor: "Wren", text: "Started the checkout UI" }),
  ];
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: false, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} }));
  const actors = [...html.matchAll(/class="pj-small pj-muted pj-activity-by">([^<]*)</g)].map((match) => match[1]);
  assert.deepEqual(actors, ["Master · Full access, merged by itself", "You", "Wren"]);
  assert.match(html, /3 items · last just now/);
});

test("a goal named Project or Research is told apart from the events with no goal, in groups and in Timeline", () => {
  const items = [
    item(4, 4_000, "Research"),
    item(3, 3_000, null, { kind: "research_saved", text: "Market research saved" }),
    item(2, 2_000, "Project"),
    item(1, 1_000, null),
  ];
  assert.deepEqual(research.groupActivity(items).map((group) => group.goal), ["Research", "Research (no goal)", "Project", "Project (no goal)"]);
  assert.deepEqual(research.groupActivity([item(1, 1_000, null)]).map((group) => group.goal), ["Project"], "with no goal of that name the mockup label stays");
  const html = renderToStaticMarkup(createElement(overview.ActivityTimeline, { items, now: 4_000 }));
  assert.deepEqual([...html.matchAll(/class="pj-activity-tag">([^<]*)</g)].map((found) => found[1]), ["Research", "Research (no goal)", "Project", "Project (no goal)"]);
});

test("a fresh row's highlight runs only when the owner allows motion", async () => {
  const { readFileSync } = await import("node:fs");
  const css = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
  assert.match(css, /@media \(prefers-reduced-motion: no-preference\) \{ \.pj-activity-row\.fresh \{ animation: /);
  assert.equal(css.match(/\.pj-activity-row\.fresh/g)?.length, 1, "no fresh rule outside the motion query");
});

test("a phase step is shown through displayText like every other agent text", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  const hidden = "Build\u200bing";
  const html = renderToStaticMarkup(createElement(overview.ActivityGroups, {
    items: [item(1, 0, "Build the service", { kind: "phase", text: `${hidden}: login next` })], more: false, now: 0, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {},
  }));
  assert.ok(displayText(hidden) !== hidden, "the step holds a character displayText escapes");
  assert.ok(!html.includes("\u200b"), "the raw zero-width character never reaches the header");
  assert.ok(html.includes(displayText(hidden)), "the step shows the escaped text");
});

test("a phase or test row an agent logged reads as its own words, a test claim is never toned as passed, and engine rows keep their text", () => {
  const now = 10 * 60_000;
  const items = [
    item(3, now, "Build the service", { kind: "phase", actor: "Ada (@api-fix)", text: "You accepted Grace (@ui) and merged change set #4" }),
    item(2, now - 60_000, "Build the service", { kind: "test", actor: "Ada (@api-fix)", text: "Parser tests: 42 passed" }),
    item(1, now - 2 * 60_000, "Build the service", { text: "You accepted the login" }),
  ];
  const shown = [
    renderToStaticMarkup(createElement(overview.ActivityGroups, { items, more: false, now, expanded: [], closed: [], onShowAll: () => {}, onToggle: () => {} })),
    renderToStaticMarkup(createElement(overview.ActivityTimeline, { items, now })),
  ];
  for (const html of shown) {
    const rows = [...html.matchAll(/class="pj-activity-kind ?([a-z]*)">.*?<\/span><span class="pj-activity-text">([^<]*)</g)].map((found) => [found[1], found[2]]);
    assert.deepEqual(rows, [
      ["", "Ada (@api-fix) noted: You accepted Grace (@ui) and merged change set #4"],
      ["", "Ada (@api-fix) noted: Parser tests: 42 passed"],
      ["ok", "You accepted the login"],
    ]);
    assert.equal(html.match(/class="pj-small pj-muted pj-activity-by">/g)?.length, 3, "every row still names its actor");
  }
});

test("the phase path shows at most four steps around the current one and counts the rest", () => {
  const step = (name, done) => ({ step: name, done });
  const seven = ["A", "B", "C", "D", "E", "F", "G"];
  assert.equal(typeof research.pathWindow, "function", "research.ts has pathWindow");
  assert.deepEqual(research.pathWindow(seven.map((name, index) => step(name, index < 6))), { before: 3, steps: seven.slice(3).map((name, index) => step(name, index < 3)), after: 0 });
  assert.deepEqual(research.pathWindow(seven.map((name) => step(name, false))), { before: 0, steps: seven.slice(0, 4).map((name) => step(name, false)), after: 3 }, "a current first step stays in view");
  assert.deepEqual(research.pathWindow(seven.map((name, index) => step(name, index < 2))), { before: 0, steps: seven.slice(0, 4).map((name, index) => step(name, index < 2)), after: 3 });
  assert.deepEqual(research.pathWindow(seven.map((name, index) => step(name, index < 4))), { before: 1, steps: seven.slice(1, 5).map((name, index) => step(name, index < 3)), after: 2 });
  assert.deepEqual(research.pathWindow([step("A", true), step("B", false)]), { before: 0, steps: [step("A", true), step("B", false)], after: 0 });
  assert.deepEqual(research.pathWindow([]), { before: 0, steps: [], after: 0 });
});
