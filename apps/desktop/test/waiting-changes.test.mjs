import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const model = await import("../src/screens/changes/model.ts");
const view = await import("../src/screens/changes/waiting.tsx");

const set = (state, id) => ({
  id, agent: { kind: "sub", id: "team-1", label: "agent-1" }, state, base: "a".repeat(40), head: "b".repeat(40),
  files: 1, added: 1, removed: 0, last_outcome: "succeeded", checked: false, created_ms: 1, updated_ms: 2,
  restorable_until_ms: null, conflicts: [], regenerated: [], message: "Add a file", waiting: false,
});

test("only change sets the owner has to act on count as waiting for them", () => {
  const sets = [set("pending", "cs-1"), set("conflict", "cs-2"), set("updating", "cs-3"), set("accepted", "cs-4"), set("discarded", "cs-5"), set("pending", "cs-6")];
  assert.equal(model.waitingCount(sets), 3);
  assert.equal(model.waitingCount([]), 0);
  assert.equal(model.waitingLine(0), null);
  assert.equal(model.waitingLine(1), "1 change set is waiting for you in Changes");
  assert.equal(model.waitingLine(13), "13 change sets are waiting for you in Changes");
  assert.equal(model.waitingLine(0, "ide"), null);
  assert.equal(model.waitingLine(1, "ide"), "1 change set is waiting for you in the IDE, under Source Control");
  assert.equal(model.waitingLine(13, "ide"), "13 change sets are waiting for you in the IDE, under Source Control");
});

test("the banner names the count and links to the project's Changes tab, and is absent when none wait", () => {
  const markup = renderToStaticMarkup(createElement(view.WaitingChangesView, { projectId: "p 1", count: 13 }));
  assert.match(markup, /13 change sets are waiting for you in Changes/);
  assert.match(markup, /<a[^>]*href="#\/projects\/p%201\/changes"[^>]*>Open Changes<\/a>/);
  assert.match(markup, /role="status"/);
  assert.equal(renderToStaticMarkup(createElement(view.WaitingChangesView, { projectId: "p 1", count: 0 })), "");
});

test("once the IDE replaces Changes, the banner names the IDE's Source Control and opens the IDE tab", () => {
  const markup = renderToStaticMarkup(createElement(view.WaitingChangesView, { projectId: "p 1", count: 2, place: "ide" }));
  assert.match(markup, /2 change sets are waiting for you in the IDE, under Source Control/);
  assert.match(markup, /<a[^>]*href="#\/projects\/p%201\/ide"[^>]*>Open IDE<\/a>/);
  assert.doesNotMatch(markup, /Changes</);
});

test("the project page shows the banner on every tab but the one change sets are reviewed in, the master conversation included", () => {
  const source = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(source, /const review: TabId = tabs\.some\(\(item\) => item\.id === "changes"\) \? "changes" : "ide";/);
  assert.match(source, /\{active !== review && <WaitingChanges projectId=\{projectId\} place=\{review\} \/>\}/);
  const banner = source.indexOf("<WaitingChanges");
  assert.ok(banner > source.indexOf("<Tabs") && banner < source.indexOf('id="pj-tab-panel"'), "it sits between the tabs and every tab's panel");
});
