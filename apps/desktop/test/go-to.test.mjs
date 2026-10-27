import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const goTo = await import("../src/ui/go-to.tsx");
const { supportDetails, AppView } = await import("../src/app.tsx");
const { formatRoute } = await import("../src/ui/router.ts");

const projects = [{ id: "harbor", name: "Harbor Notes" }, { id: "fieldnotes-mobile", name: "Fieldnotes Mobile" }];

test("Go to lists every project and its tabs, Home, every Settings section, then the actions", () => {
  const targets = goTo.goToTargets(projects);
  assert.deepEqual(targets.slice(0, 6).map((target) => [target.label, target.kind, formatRoute(target.route)]), [
    ["Harbor Notes", "Project", "#/projects/harbor/overview"],
    ["Harbor Notes / Conversation", "Project tab", "#/projects/harbor/conversation"],
    ["Harbor Notes / Agents", "Project tab", "#/projects/harbor/agents"],
    ["Harbor Notes / Changes", "Project tab", "#/projects/harbor/changes"],
    ["Harbor Notes / Context", "Project tab", "#/projects/harbor/context"],
    ["Harbor Notes / Settings", "Project tab", "#/projects/harbor/settings"],
  ]);
  assert.deepEqual(targets.slice(12).map((target) => [target.label, formatRoute(target.route)]), [
    ["Home", "#/home"],
    ["Settings / Accounts", "#/settings/accounts"],
    ["Settings / Pools", "#/settings/pools"],
    ["Settings / Harnesses", "#/settings/harnesses"],
    ["Settings / Plugins and skills", "#/settings/plugins"],
    ["Settings / Memory", "#/settings/memory"],
    ["Settings / Profile", "#/settings/profile"],
    ["New project", "#/projects/new?step=basics"],
    ["Clone from GitHub", "#/projects/add/clone"],
    ["Add existing folder", "#/projects/add/folder"],
    ["Add account", "#/settings/accounts?action=add"],
    ["New pool", "#/settings/pools?action=new"],
  ]);
  assert.equal(goTo.goToTargets([]).length, 12);
});

test("Go to ranks a prefix first, then a match inside, then letters in order, like the TUI", () => {
  assert.equal(goTo.rank("", "Home"), 0);
  assert.equal(goTo.rank("har", "Harbor Notes"), 0);
  assert.equal(goTo.rank("NOTES", "Harbor Notes"), 1);
  assert.equal(goTo.rank("hbn", "Harbor Notes"), 2);
  assert.equal(goTo.rank("h n", "Harbor Notes"), 2);
  assert.equal(goTo.rank("nh", "Harbor Notes"), null);
  assert.equal(goTo.rank("zz", "Harbor Notes"), null);
});

test("Go to shows at most eight best matches in rank then list order", () => {
  const targets = goTo.goToTargets(projects);
  assert.deepEqual(goTo.matchTargets(targets, "agents").map((target) => target.label), ["Harbor Notes / Agents", "Fieldnotes Mobile / Agents"]);
  assert.deepEqual(goTo.matchTargets(targets, "settings").map((target) => target.label).slice(0, 3), ["Settings / Accounts", "Settings / Pools", "Settings / Harnesses"]);
  assert.equal(goTo.matchTargets(targets, "").length, goTo.GO_TO_ROWS_MAX);
  assert.deepEqual(goTo.matchTargets(targets, "qqq"), []);
  assert.equal(goTo.matchTargets(targets, "h".repeat(500)).length, 0);
});

test("the Go to and shortcuts dialogs stay closed until asked for and the shell carries them", () => {
  assert.equal(renderToStaticMarkup(createElement(goTo.GoToLayer, { projects })), "");
  assert.equal(renderToStaticMarkup(createElement(goTo.GoToDialog, { open: false, projects, onGo() {}, onClose() {} })), "");
  const session = {
    engine: "running", unconfirmed: false, profile: { kind: "ok", value: { name: "Mira", role: "", bio: "", preferred_stack: [], working_style: { detail: "terse", initiative: "ask_first" } } },
    projects: { kind: "ok", value: [] }, toast: "", revision: 1, retry() {}, checkUnconfirmed() {}, refreshProfile() {}, refreshProjects() {}, showToast() {}, noteResult: (r) => r,
  };
  assert.doesNotMatch(renderToStaticMarkup(createElement(AppView, { session, route: { name: "settings", tab: "profile" } })), /Go to|Keyboard shortcuts/);
  assert.ok(goTo.SHORTCUTS.some(([keys, what]) => keys === "Cmd+K" && what === "Go to anything"));
  assert.ok(goTo.SHORTCUTS.some(([keys]) => keys === "Cmd+/"));
});

test("support details name the problem, window and time, and nothing private", () => {
  const details = supportDetails(1180.4, 799.6, new Date("2026-10-23T23:59:00Z"));
  assert.equal(details, "CrazyAgent desktop\nEngine: not running\nProblem: engine_unreachable\nWindow: 1180x800\nTime: 2026-10-23T23:59:00.000Z\n");
  assert.doesNotMatch(details, /\/Users|sock|token|key/i);
});

test("the engine-down screen offers Copy details for support next to Try again", () => {
  const session = { engine: "down", unconfirmed: false, profile: { kind: "loading" }, projects: { kind: "loading" }, toast: "", revision: 0,
    retry() {}, checkUnconfirmed() {}, refreshProfile() {}, refreshProjects() {}, showToast() {}, noteResult: (r) => r };
  const html = renderToStaticMarkup(createElement(AppView, { session, route: { name: "home" } }));
  assert.match(html, /Try again<\/button>/);
  assert.match(html, /Copy details for support<\/button>/);
});
