import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const goTo = await import("../src/ui/go-to.tsx");
const { supportDetails, AppView } = await import("../src/app.tsx");
const { formatRoute } = await import("../src/ui/router.ts");
const { TABS, IDE_TAB } = await import("../src/screens/project/index.tsx");
const { routeSection, fileSection, agentSection, sectionFile, sectionAgent } = await import("../src/screens/project/shared.tsx");
const ide = await import("../src/api-ide.ts");

const projects = [{ id: "harbor", name: "Harbor Notes" }, { id: "fieldnotes-mobile", name: "Fieldnotes Mobile" }];

test("Go to lists every project and its tabs, Home, every Settings section, then the actions", () => {
  const targets = goTo.goToTargets(projects);
  assert.deepEqual(targets.slice(0, 7).map((target) => [target.label, target.kind, formatRoute(target.route)]), [
    ["Harbor Notes", "Project", "#/projects/harbor/overview"],
    ["Harbor Notes / Conversation", "Project tab", "#/projects/harbor/conversation"],
    ["Harbor Notes / Agents", "Project tab", "#/projects/harbor/agents"],
    ["Harbor Notes / Changes", "Project tab", "#/projects/harbor/changes"],
    ["Harbor Notes / Questions", "Project tab", "#/projects/harbor/questions"],
    ["Harbor Notes / Context", "Project tab", "#/projects/harbor/context"],
    ["Harbor Notes / Process", "Project tab", "#/projects/harbor/process"],
    ["Harbor Notes / Metrics", "Project tab", "#/projects/harbor/metrics"],
    ["Harbor Notes / Settings", "Project tab", "#/projects/harbor/settings"],
    ["Harbor Notes / IDE", "Project tab", "#/projects/harbor/ide"],
  ]);
  assert.deepEqual(targets.slice(20).map((target) => [target.label, formatRoute(target.route)]), [
    ["Home", "#/home"],
    ["Profile", "#/settings/profile"],
    ["Settings / Accounts", "#/settings/accounts"],
    ["Settings / Pools", "#/settings/pools"],
    ["Settings / Harnesses", "#/settings/harnesses"],
    ["Settings / Plugins and skills", "#/settings/plugins"],
    ["Settings / Memory", "#/settings/memory"],
    ["New project", "#/projects/new?step=basics"],
    ["Clone from GitHub", "#/projects/add/clone"],
    ["Add existing folder", "#/projects/add/folder"],
    ["Add account", "#/settings/accounts?action=add"],
    ["New pool", "#/settings/pools?action=new"],
  ]);
  assert.equal(goTo.goToTargets([]).length, 12);
});

test("Go to lists a project's tabs from the project page's own list, every tab plus the IDE, so a new tab cannot be missed", () => {
  const tabs = [...TABS, IDE_TAB];
  const listed = goTo.goToTargets([{ id: "harbor", name: "Harbor Notes" }]).filter((target) => target.route.name === "project");
  assert.deepEqual(listed.map((target) => target.route.tab), tabs.map((tab) => tab.id));
  assert.deepEqual(listed.slice(1).map((target) => target.label), tabs.slice(1).map((tab) => `Harbor Notes / ${tab.label}`));
});

const open = { id: "harbor", files: ["src/cart.ts", "README.md"], agents: [{ key: "agent-7", label: "Ada (@north)" }] };

test("with a project open, Go to lists its running agents and then its files after the project tabs, and finds a file by its path", () => {
  const targets = goTo.goToTargets(projects, open);
  const tabs = projects.length * (TABS.length + 1);
  assert.deepEqual(targets.slice(tabs, tabs + 4).map((target) => [target.label, target.kind, formatRoute(target.route)]), [
    ["Ada (@north)", "Agent", "#/projects/harbor/agents?section=agent%3Aagent-7"],
    ["src/cart.ts", "File", "#/projects/harbor/ide?section=file%3Asrc%2Fcart.ts"],
    ["README.md", "File", "#/projects/harbor/ide?section=file%3AREADME.md"],
    ["Home", "Place", "#/home"],
  ]);
  assert.equal(targets.length, goTo.goToTargets(projects).length + 3);
  assert.equal(goTo.matchTargets(targets, "cart")[0].label, "src/cart.ts");
  assert.equal(goTo.matchTargets(targets, "ada")[0].label, "Ada (@north)");
  assert.equal(goTo.goToTargets([], open).length, 12 + 3);
});

test("Go to lists only the open project's running sub-agents that the Team view shows, each by its Messages row key", () => {
  const agent = (id, role, status, extra = {}) => ({ id, role, status, label: `Agent ${id}`, finished: false, ...extra });
  const agents = [agent(1, "master", "working"), agent(7, "sub", "working"), agent(8, "sub", "idle"), agent(9, "sub", "starting"),
    agent(10, "sub", "stopped"), agent(11, "sub", "failed"), agent(12, "sub", "idle", { finished: true })];
  assert.deepEqual(goTo.goToAgents({ agents, show_finished: false }), [
    { key: "agent-7", label: "Agent 7" }, { key: "agent-8", label: "Agent 8" }, { key: "agent-9", label: "Agent 9" },
  ]);
  assert.deepEqual(goTo.goToAgents({ agents, show_finished: true }).map((item) => item.key), ["agent-7", "agent-8", "agent-9", "agent-12"]);
});

test("a file or agent section survives the route round trip, and neither reads as the other or as a settings section", () => {
  for (const path of ["src/cart.ts", "docs/a b+c?.md", "dir/name:with:colons.ts", "über/naïve.md"]) {
    const route = { name: "project", id: "harbor", tab: "ide", section: fileSection(path) };
    assert.equal(sectionFile(routeSection(formatRoute(route))), path, path);
    assert.equal(sectionAgent(routeSection(formatRoute(route))), null, path);
  }
  const agent = { name: "project", id: "harbor", tab: "agents", section: agentSection("agent-7") };
  assert.equal(sectionAgent(routeSection(formatRoute(agent))), "agent-7");
  assert.equal(sectionFile(routeSection(formatRoute(agent))), null);
  for (const section of [undefined, "", "access", "settings", "file:", "agent:"]) {
    assert.equal(sectionFile(section), null, String(section));
    assert.equal(sectionAgent(section), null, String(section));
  }
});

test("a Go to file waits for the IDE to listen, a bounded number of times, and stops at any other answer or once the tab is gone", async () => {
  const notRunning = { kind: "error", code: "ide_not_running", message: "" };
  const outside = { kind: "error", code: "ide_reveal_outside", message: "" };
  const ok = { kind: "ok", value: undefined };
  const answers = (list) => {
    const calls = [];
    return { calls, reveal: async () => { calls.push(1); return list.shift(); } };
  };
  const waits = [];
  const wait = async (ms) => { waits.push(ms); };
  let run = answers([notRunning, notRunning, ok]);
  assert.deepEqual(await ide.revealWhenOpen(run.reveal, () => true, wait), ok);
  assert.equal(run.calls.length, 3);
  assert.deepEqual(waits, [ide.REVEAL_WAIT_MS, ide.REVEAL_WAIT_MS]);
  run = answers([outside, ok]);
  assert.deepEqual(await ide.revealWhenOpen(run.reveal, () => true, wait), outside, "a refused path is not asked again");
  assert.equal(run.calls.length, 1);
  run = answers(Array.from({ length: ide.REVEAL_TRIES_MAX + 5 }, () => notRunning));
  assert.deepEqual(await ide.revealWhenOpen(run.reveal, () => true, wait), notRunning);
  assert.equal(run.calls.length, ide.REVEAL_TRIES_MAX);
  run = answers([notRunning, ok]);
  assert.equal(await ide.revealWhenOpen(run.reveal, () => false, wait), null);
  assert.equal(run.calls.length, 1);
});

test("a Go to file the engine refuses says why in plain words", () => {
  assert.equal(ide.revealCopy("ide_reveal_outside"), "CrazyAgent opens only files inside the project's folder.");
  assert.equal(ide.revealCopy("ide_reveal_invalid"), ide.revealCopy("ide_reveal_outside"));
  assert.equal(ide.revealCopy("ide_reveal_missing"), "That file is not there any more.");
  assert.equal(ide.revealCopy("ide_not_running"), "The IDE didn't open in time to show the file. Pick it in Go to again.");
  assert.equal(ide.revealCopy("something_else"), "");
});

test("Go to reads the open project's files and agents only while it is open, files only while the IDE can open, and the pick reaches the IDE and Team views", () => {
  const read = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");
  const source = read("ui/go-to.tsx");
  assert.match(source, /const ideOpens = ideReplacesTabs\(cachedIdeView\(\)\?\.value\);/, "the project page's own rule for listing its IDE tab");
  assert.match(source, /useProjectFiles\(project \?\? "", open && project !== null && ideOpens \? query : null\)/);
  assert.match(source, /files: ideOpens \? files \?\? \[\] : \[\]/);
  assert.equal(ide.ideReplacesTabs(undefined), false, "no IDE state read yet lists no files");
  assert.match(source, /useOpenAgents\(open \? project : null\)/);
  assert.match(read("app.tsx"), /<GoToLayer projects=\{projects \?\? \[\]\} project=\{page\.name === "project" \? page\.id : null\} \/>/);
  assert.match(read("screens/project/index.tsx"), /<IdeTab key=\{data\.id\} projectId=\{projectId\} reveal=\{sectionFile\(settingsSection\)\}/);
  assert.match(read("screens/project/agents.tsx"), /const agent = sectionAgent\(section\);/);
  assert.match(read("screens/project/agents.tsx"), /<TeamPanel [^>]*opened=\{agent\}/);
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
    projects: { kind: "ok", value: [] }, toast: "", revision: 1, intake: null, found: { kind: "loading" }, retry() {}, checkUnconfirmed() {}, refreshProfile() {}, refreshProjects() {}, showToast() {}, noteResult: (r) => r,
  };
  assert.doesNotMatch(renderToStaticMarkup(createElement(AppView, { session, route: { name: "settings", tab: "profile" } })), /Go to|Keyboard shortcuts/);
  assert.ok(goTo.SHORTCUTS.some(([keys, what]) => keys === "Cmd+K" && what === "Go to anything"));
  assert.ok(goTo.SHORTCUTS.some(([keys]) => keys === "Cmd+/"));
});

test("the shortcuts dialog names no number key past the tabs the project page shows", async () => {
  const page = await import("../src/screens/project/index.tsx");
  const ready = { state: { build: { state: "ready", version: "1.143.17183", root: "/x" }, unqualified: null }, platform: "macos" };
  const shown = [page.projectTabs(undefined), page.projectTabs(ready), page.projectTabs(ready, "changes")].map((tabs) => tabs.length);
  const rows = goTo.SHORTCUTS.filter(([, what]) => what.startsWith("Project tabs"));
  assert.equal(rows.length, 1, "one row names the tab keys");
  const named = Math.max(...[...rows[0][0].matchAll(/Cmd\+(\d)/g)].map((match) => Number(match[1])));
  assert.ok(named <= Math.min(...shown), `${rows[0][0]} names Cmd+${named}, and a page can show only ${Math.min(...shown)} tabs`);
});

test("support details name the problem, window and time, and nothing private", () => {
  const details = supportDetails(1180.4, 799.6, new Date("2026-10-23T23:59:00Z"));
  assert.equal(details, "CrazyAgent desktop\nEngine: not running\nProblem: engine_unreachable\nWindow: 1180x800\nTime: 2026-10-23T23:59:00.000Z\n");
  assert.doesNotMatch(details, /\/Users|sock|token|key/i);
});

test("the engine-down screen offers Copy details for support next to Try again", () => {
  const session = { engine: "down", unconfirmed: false, profile: { kind: "loading" }, projects: { kind: "loading" }, toast: "", revision: 0, intake: null, found: { kind: "loading" },
    retry() {}, checkUnconfirmed() {}, refreshProfile() {}, refreshProjects() {}, showToast() {}, noteResult: (r) => r };
  const html = renderToStaticMarkup(createElement(AppView, { session, route: { name: "home" } }));
  assert.match(html, /Try again<\/button>/);
  assert.match(html, /Copy details for support<\/button>/);
});
