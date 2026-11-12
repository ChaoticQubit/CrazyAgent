import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { HomeView, setupSteps } = await import("../src/screens/dashboard/index.tsx");
const { learnSteps, marksForRoute, TOPIC_HEADINGS } = await import("../src/screens/dashboard/learn.tsx");

const ok = (value) => ({ kind: "ok", value });
const loading = { kind: "loading" };
const found = { state: "found", version: "1" };
const tools = ok({ dependencies: ["tmux", "uv", "graphify"].map((name) => ({ name, label: name, state: found })), pack: [], running: false });
const account = { id: "a1", name: "ChatGPT 1", provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription",
  method_label: "", url: null, model: null, status: "signed_in", status_label: "Signed in" };
const pool = { id: "pool-1", name: "Main", summary: "", members: [], project_count: 0 };
const WORDS = {
  project: ["basics", "Create a project"], master: ["core_loop", "Talk to the master"], agents: ["core_loop", "Watch the team work"],
  question: ["core_loop", "Answer a question"], change_set: ["core_loop", "Accept a change set"], ide: ["ide_and_terminals", "Open the IDE"],
  terminal: ["ide_and_terminals", "Open a terminal"], process: ["process_metrics_usage", "Set your process"],
  metrics: ["process_metrics_usage", "Read your metrics"], usage: ["process_metrics_usage", "Check your usage"],
};
const progress = (done = [], skipped = false) => ok({
  skipped,
  steps: Object.entries(WORDS).map(([id, [topic, title]]) => ({ id, topic, title, body: "One sentence. Two sentences.", count: done.includes(id) ? 1 : 0 })),
});
const ready = { accounts: ok([account]), pools: ok([pool]), harnesses: ok([]), tools };
const fresh = { accounts: ok([]), pools: ok([]), harnesses: ok([]), tools };
const model = (reads, learning, projects = []) => learnSteps(setupSteps(reads.accounts, reads.pools, reads.harnesses, reads.tools), learning, projects);

test("with setup done and learning open the panel is Learn CrazyAgent with every topic, setup under Basics", () => {
  const shown = model(ready, progress(["project"]), [{ id: "p1", name: "Harbor", repository_count: 0 }]);
  assert.equal(shown.visible, true);
  assert.equal(shown.title, "Learn CrazyAgent");
  assert.deepEqual(shown.groups.map((group) => group.heading), ["Basics", "Core loop", "IDE and terminals", "Process, Metrics and Usage"]);
  assert.deepEqual(shown.groups[0].items.map((item) => item.id), ["accounts", "pools", "harnesses", "tools", "project"]);
  assert.deepEqual([shown.done, shown.total], [4, 13], "Check your harnesses is a link, never counted");
  const master = shown.groups[1].items[0];
  assert.deepEqual([master.title, master.action, master.href, master.done], ["Talk to the master", "Open conversation", "#/projects/p1/conversation", false]);
  assert.equal(shown.groups[3].items.find((item) => item.id === "usage").href, "#/metrics");
  assert.equal(shown.groups[3].items.find((item) => item.id === "process").href, "#/settings/process");
});

test("a project step with no project yet links to New project", () => {
  const shown = model(fresh, progress());
  for (const id of ["master", "agents", "question", "change_set", "ide", "terminal"]) {
    const item = shown.groups.flatMap((group) => group.items).find((entry) => entry.id === id);
    assert.deepEqual([item.action, item.href], ["New project", "#/projects/new?step=basics"], id);
  }
});

test("the panel waits for the learning read too, so Home never flashes it", () => {
  assert.equal(model(fresh, loading).visible, false);
  assert.equal(model({ ...ready, tools: loading }, progress()).visible, false);
  assert.equal(model(ready, progress()).visible, true);
});

test("skipping the guide hides only the learning steps; setup that is not done still shows", () => {
  assert.equal(model(ready, progress([], true)).visible, false);
  const unset = model(fresh, progress([], true));
  assert.equal(unset.visible, true);
  assert.equal(unset.title, "Get set up");
  assert.equal(unset.learning, false);
  assert.deepEqual(unset.groups.flatMap((group) => group.items).map((item) => item.id), ["accounts", "pools", "harnesses", "tools"]);
});

test("with every learning step done only unfinished setup keeps the panel", () => {
  const all = progress(Object.keys(WORDS));
  assert.equal(model(ready, all).visible, false);
  assert.equal(model(fresh, all).title, "Get set up");
  assert.equal(model(fresh, all).visible, true);
});

test("an engine without the learning read keeps the Get set up panel and its rule", () => {
  for (const learning of [{ kind: "not_ready" }, { kind: "error", code: "x", message: "y" }]) {
    assert.equal(model(ready, learning).visible, false);
    const unset = model(fresh, learning);
    assert.deepEqual([unset.visible, unset.title, unset.learning], [true, "Get set up", false]);
  }
});

test("Home renders the guide with its topics, the progress and Hide guide, and keeps setup-panel for the setup rows", () => {
  const markup = renderToStaticMarkup(createElement(HomeView, { projects: ok([]), ...ready, learning: progress(["process"]), onHideLearning: () => {}, onRetryProjects: () => {} }));
  assert.match(markup, /class="home with-setup"/);
  assert.match(markup, /<h2>Learn CrazyAgent<\/h2>/);
  assert.match(markup, /class="panel setup-panel"/);
  for (const heading of Object.values(TOPIC_HEADINGS)) assert.match(markup, new RegExp(`<h3>${heading}</h3>`));
  assert.match(markup, /4 of 13 done/);
  assert.match(markup, />Hide guide</);
  assert.match(markup, /<strong class="grow">Set your process<\/strong>/);
  assert.match(markup, /One sentence\. Two sentences\./);
  const skipped = renderToStaticMarkup(createElement(HomeView, { projects: ok([]), ...fresh, learning: progress([], true), onRetryProjects: () => {} }));
  assert.match(skipped, /<h2>Get set up<\/h2>/);
  assert.doesNotMatch(skipped, /Hide guide|Core loop/);
});

test("opening Process, Metrics or a project's Process or Metrics tab marks those steps; other pages mark nothing", () => {
  assert.deepEqual(marksForRoute({ name: "metrics" }), ["metrics", "usage"]);
  assert.deepEqual(marksForRoute({ name: "settings", tab: "process" }), ["process"]);
  assert.deepEqual(marksForRoute({ name: "project", id: "p1", tab: "process" }), ["process"]);
  assert.deepEqual(marksForRoute({ name: "project", id: "p1", tab: "metrics" }), ["metrics"]);
  for (const route of [{ name: "home" }, { name: "settings", tab: "accounts" }, { name: "project", id: "p1", tab: "agents" }]) {
    assert.deepEqual(marksForRoute(route), [], route.name);
  }
});
