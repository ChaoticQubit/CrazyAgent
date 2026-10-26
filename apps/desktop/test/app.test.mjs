import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { App, AppView, EngineOutdatedView } = await import("../src/app.tsx");
const state = await import("../src/state.ts");
const { HomeView, setupSteps, needsYou, cardActivity } = await import("../src/screens/dashboard/index.tsx");

const profile = { name: "Mira Okafor", role: "", bio: "", preferred_stack: [], working_style: { detail: "terse", initiative: "ask_first" } };

function session(patch = {}) {
  const forbidden = () => { throw new Error("static rendering must not call the engine"); };
  return {
    engine: "running", unconfirmed: false, profile: { kind: "ok", value: profile }, projects: { kind: "ok", value: [] },
    toast: "", revision: 1, retry: forbidden, checkUnconfirmed: forbidden, refreshProfile: forbidden, refreshProjects: forbidden,
    showToast: forbidden, noteResult: (result) => result, ...patch,
  };
}

const render = (patch, route = { name: "home" }) => renderToStaticMarkup(createElement(AppView, { session: session(patch), route }));

test("the real App first renders the launch skeleton without calling the engine during render", () => {
  const markup = renderToStaticMarkup(createElement(App));
  assert.match(markup, /aria-busy="true"/);
  assert.match(markup, /CrazyAgent/);
  assert.doesNotMatch(markup, /role="alert"|Connect to engine|Approvals|Owner disconnected/);
  assert.ok(Buffer.byteLength(markup) < 16384);
});

test("an unreachable engine shows one full-window state with Try again and no shell", () => {
  const markup = render({ engine: "down" });
  assert.match(markup, /<h1>CrazyAgent&#x27;s engine isn&#x27;t running<\/h1>/);
  assert.match(markup, /Start it, then try again\./);
  assert.match(markup, /class="btn primary"[^>]*>[\s\S]*Try again<\/button>/);
  assert.doesNotMatch(markup, /class="sidebar"/);
});

test("an engine of another version asks for a restart with both versions, not the engine-down state", () => {
  const { engineState } = state;
  const ok = { kind: "ok", value: { connected: true, pending: false } };
  assert.equal(engineState(ok), "running");
  assert.equal(engineState({ kind: "error", code: "engine_version_mismatch", message: "x" }), "outdated");
  assert.equal(engineState({ kind: "error", code: "engine_unreachable", message: "x" }), "down");
  assert.equal(engineState({ kind: "error", code: "engine_start_failed", message: "x" }), "down");

  const screen = render({ engine: "outdated" });
  assert.match(screen, /<h1>CrazyAgent&#x27;s engine needs a restart<\/h1>/);
  assert.doesNotMatch(screen, /class="sidebar"|isn&#x27;t running/);

  const forbidden = () => { throw new Error("static rendering must not call the engine"); };
  const versions = { kind: "ok", value: { engine: "0.1.0", app: "0.2.0", message: "Engine is version 0.1.0, this app is 0.2.0; restart the engine" } };
  const view = (patch) => renderToStaticMarkup(createElement(EngineOutdatedView, { versions, restart: { kind: "idle" }, onRestart: forbidden, onRetry: forbidden, ...patch }));
  const markup = view();
  assert.match(markup, /Engine is version 0\.1\.0, this app is 0\.2\.0; restart the engine/);
  assert.match(markup, /class="btn primary"[^>]*>[\s\S]*Restart engine<\/button>/);
  assert.match(markup, /Try again<\/button>/);
  assert.match(view({ restart: { kind: "busy" } }), /Restarting\.\.\./);
  assert.match(view({ restart: { kind: "failed", message: "CrazyAgent's engine didn't start. Try again." } }), /role="alert"[^>]*>CrazyAgent&#x27;s engine didn&#x27;t start/);
  const hostile = view({ versions: { kind: "ok", value: { engine: "9\u202e1", app: "0.2.0", message: "Engine is version 9\u202e1, this app is 0.2.0; restart the engine" } } });
  assert.doesNotMatch(hostile, /\u202e/);
});

test("an empty engine profile opens Welcome; a saved profile opens Home in the shell", () => {
  assert.match(render({ profile: { kind: "ok", value: null } }), /<h1>Tell CrazyAgent about you<\/h1>/);
  const home = render({});
  assert.match(home, /class="sidebar"/);
  assert.match(home, /<a class="nav-item" href="#\/home" aria-current="page">/);
  assert.match(home, /Mira Okafor/);
  assert.match(home, /Engine running/);
  assert.match(home, /<h1>Projects<\/h1>/);
  assert.match(home, /Skip to content/);
  assert.doesNotMatch(home, /Tell CrazyAgent about you/);
});

test("a profile read that is not in this build yet skips Welcome instead of blocking the app", () => {
  const markup = render({ profile: { kind: "not_ready" } });
  assert.match(markup, /<h1>Projects<\/h1>/);
  assert.match(markup, /Profile/);
});

test("an unconfirmed change shows a banner naming it, with Check again and Retry", () => {
  const intent = { kind: "setup_project", generation: "3", project_id: "p-1", baseline_id: null, model_access: null, project_spec: null, operation_id: "op-1" };
  const markup = render({ unconfirmed: { intent, outcome: null, busy: false } });
  assert.match(markup, /We couldn&#x27;t confirm saving the project setup\. Other changes wait until it&#x27;s checked\./);
  assert.match(markup, />Check again<\/button>/);
  assert.match(markup, />Retry<\/button>/);
  assert.doesNotMatch(markup, /intent|Reconcile|operation|still being saved/i);
  const unsent = render({ unconfirmed: { intent, outcome: "not_recorded_at_barrier", busy: false } });
  assert.match(unsent, /Saving the project setup hasn&#x27;t reached the engine\. Retry sends the same change again\./);
  const checking = render({ unconfirmed: { intent, outcome: null, busy: true } });
  assert.match(checking, /Checking\.\.\./);
  assert.match(checking, /<button[^>]*disabled=""[^>]*>[\s\S]*?Retry<\/button>/);
  assert.doesNotMatch(render({}), /class="banner"/);
});

test("routes reach Settings tabs and the project screens", () => {
  const settings = render({}, { name: "settings", tab: "pools" });
  assert.match(settings, /<h1>Settings<\/h1>/);
  assert.match(settings, /role="tablist" aria-label="Settings sections"/);
  assert.match(settings, /<a id="tab-pools" role="tab" class="tab" href="#\/settings\/pools" aria-selected="true" tabindex="0">/);
  for (const label of ["Accounts", "Pools", "Harnesses", "Plugins and skills", "Memory", "Profile"]) assert.match(settings, new RegExp(`</svg>${label}</a>`));
  const project = render({ projects: { kind: "ok", value: [{ id: "p-1", name: "Harbor Notes", repository_count: 1 }] } }, { name: "project", id: "p-1", tab: "overview" });
  assert.match(project, /<h1>Harbor Notes<\/h1>/);
  assert.match(project, /role="tablist" aria-label="Project"/);
  assert.match(project, /id="tab-overview" role="tab" class="tab" href="#\/projects\/p-1\/overview" aria-selected="true"/);
  assert.match(project, /aria-current="page"><svg[\s\S]*?<\/svg><span class="name">Harbor Notes<\/span>/);
  assert.match(render({}, { name: "new-project", step: "basics" }), /<h1>New project<\/h1>/);
});

const ok = (value) => ({ kind: "ok", value });
const account = (patch) => ({ id: "a1", name: "ChatGPT 1", provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription",
  method_label: "Sign in with ChatGPT", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const harness = (id, kind) => ({ id, name: id, status: kind === "limited" ? { kind, reason: "x" } : { kind }, status_label: "", executable: null,
  headless: "", tools: { kind: "switchable" }, model_selection: true, account_kinds: [], aliases: [] });
const tool = (name, state) => ({ name, label: name, state });
const toolsRead = (tmux, uv, graphify, patch = {}) => ok({
  dependencies: [tool("tmux", tmux), tool("uv", uv), tool("graphify", graphify)], pack: [], running: false, ...patch,
});
const found = { state: "found", version: "3.5a" };
const toolsReady = toolsRead(found, { state: "installed", version: "0.9.2" }, found);

test("Get set up stays until one account is ready and one pool exists; harnesses never block", () => {
  const harnesses = ok([harness("codex", "ready"), harness("claude-code", "ready"), harness("pi", "not_installed")]);
  const empty = setupSteps(ok([]), ok([]), harnesses, toolsReady);
  assert.equal(empty.visible, true);
  assert.deepEqual(empty.items.map((item) => [item.id, item.done]), [["accounts", false], ["pools", false], ["harnesses", false], ["tools", true]]);
  assert.equal(empty.items[2].body, "2 of 3 harnesses ready on this Mac.");
  assert.equal(empty.items[0].href, "#/settings/accounts?action=add");
  assert.equal(empty.items[1].href, "#/settings/pools?action=new");
  assert.equal(setupSteps(ok([account({ status: "not_signed_in" })]), ok([]), harnesses, toolsReady).items[0].done, false);
  assert.equal(setupSteps(ok([account({ status: "key_saved" })]), ok([]), harnesses, toolsReady).items[0].done, true);
  const pool = { id: "pool-1", name: "Main", summary: "1 ChatGPT", members: [], project_count: 0 };
  assert.equal(setupSteps(ok([account({})]), ok([pool]), harnesses, toolsReady).visible, false);
  const unknown = setupSteps({ kind: "not_ready" }, { kind: "not_ready" }, { kind: "not_ready" }, { kind: "not_ready" });
  assert.equal(unknown.visible, true);
  assert.equal(unknown.items[2].body, "See which coding agents are installed on this Mac.");
});

test("Add an account is done only when an account is ready for agents, and says which; otherwise it links to Accounts", () => {
  const harnesses = ok([]);
  const claude = (id, status) => account({ id, name: "ChaoticQubit", provider: "claude", provider_name: "Claude", status });
  const qwen = account({ id: "q1", name: "Qwen 2.5 - 7B", provider: "local", provider_name: "Local model", method: "url", status: "signed_in" });
  const step = (accounts) => setupSteps(ok(accounts), ok([]), harnesses, toolsReady).items[0];

  const none = step([claude("c1", "not_signed_in"), claude("c2", "not_signed_in"), claude("c3", "unreachable")]);
  assert.equal(none.done, false);
  assert.equal(none.body, "No account is ready for agents yet. Sign one in or check its key or URL.");
  assert.equal(none.action, "Open accounts");
  assert.equal(none.href, "#/settings/accounts");

  assert.equal(step([claude("c1", "not_signed_in")]).href, "#/settings/accounts");

  const one = step([claude("c1", "not_signed_in"), claude("c2", "signing_in"), qwen]);
  assert.equal(one.done, true);
  assert.equal(one.body, "Qwen 2.5 - 7B is ready for agents.");

  const two = step([qwen, account({ id: "k1", name: "Key", provider: "openai_api", status: "key_saved" }), claude("c1", "not_signed_in")]);
  assert.equal(two.done, true);
  assert.equal(two.body, "2 accounts are ready for agents.");

  const empty = step([]);
  assert.equal(empty.done, false);
  assert.equal(empty.body, "Sign in with ChatGPT or Claude, or add an API key or local model.");
  assert.equal(empty.action, "Add account");
  assert.equal(empty.href, "#/settings/accounts?action=add");
});

test("Get set up keeps Install tools until tmux, uv and graphify are found; the pack never blocks", () => {
  const pool = { id: "pool-1", name: "Main", summary: "1 ChatGPT", members: [], project_count: 0 };
  const steps = (tools) => setupSteps(ok([account({})]), ok([pool]), ok([]), tools);
  const item = (tools) => steps(tools).items.find((entry) => entry.id === "tools");
  const missing = { state: "missing" };

  const skipped = steps(toolsRead(missing, missing, missing));
  assert.equal(skipped.items.length, 4);
  assert.equal(skipped.items[3].id, "tools");
  assert.equal(skipped.visible, true);
  assert.deepEqual({ ...skipped.items[3] }, {
    id: "tools", icon: "wrench", title: "Install tools",
    body: "tmux, uv and graphify aren't installed. Live terminals and the team graph need tmux; setting up a project needs graphify.",
    action: "Set up tools", href: "#/settings/pack", done: false,
  });
  assert.equal(item(toolsRead(found, found, missing)).body,
    "graphify isn't installed. Live terminals and the team graph need tmux; setting up a project needs graphify.");
  assert.equal(item(toolsRead(missing, found, { state: "failed", reason: "Checksum didn't match." })).body,
    "tmux and graphify aren't installed. Live terminals and the team graph need tmux; setting up a project needs graphify.");

  assert.equal(item(toolsReady).done, true);
  assert.equal(item(toolsReady).body, "tmux, uv and graphify are installed.");
  assert.equal(steps(toolsReady).visible, false);
  const pack = [{ id: "serena", kind: "mcp_server", group: "optional", source: "x", license: "MIT", on_by_default: false, state: { state: "failed", reason: "no" } },
    { id: "superpowers", kind: "skill", group: "workflow", source: "x", license: "MIT", on_by_default: true, state: { state: "available" } }];
  assert.equal(steps(toolsRead(found, found, found, { pack })).visible, false);

  for (const tools of [{ kind: "loading" }, toolsRead({ state: "checking" }, found, found), toolsRead({ state: "checking" }, { state: "checking" }, { state: "checking" })]) {
    const checking = item(tools);
    assert.equal(checking.done, false);
    assert.equal(checking.body, "Checking tmux, uv and graphify on this computer.");
    assert.equal(steps(tools).visible, true);
  }
  assert.equal(item(toolsRead({ state: "installing" }, found, missing)).body, "Setup is installing the tools. This updates when it finishes.");
  assert.equal(item(toolsRead(found, found, found, { running: true })).done, true);
  assert.equal(item(ok({ dependencies: [tool("tmux", found), tool("uv", found)], pack: [], running: false })).done, false);
  assert.equal(item({ kind: "not_ready" }).done, false);
  assert.equal(item({ kind: "not_ready" }).body, "Tool setup isn't in this build yet.");
  const failedRead = item({ kind: "error", code: "setup_owner_unavailable", message: "CrazyAgent's engine isn't running." });
  assert.equal(failedRead.done, false);
  assert.equal(failedRead.body, "Couldn't check tmux, uv and graphify. Open setup to try again.");
});

test("Home keeps the Get set up panel with Set up tools after a skipped tools step", () => {
  const props = { projects: ok([{ id: "p1", name: "Harbor", repository_count: 0 }]), accounts: ok([account({})]),
    pools: ok([{ id: "pool-1", name: "Main", summary: "", members: [], project_count: 1 }]), harnesses: ok([]), onRetryProjects: () => {} };
  const skipped = renderToStaticMarkup(createElement(HomeView, { ...props, tools: toolsRead(found, found, { state: "missing" }) }));
  assert.match(skipped, /<h2>Get set up<\/h2>/);
  assert.match(skipped, /<strong class="grow">Install tools<\/strong>/);
  assert.match(skipped, /graphify isn&#x27;t installed\./);
  assert.match(skipped, /<a class="btn small" href="#\/settings\/pack">Set up tools<\/a>/);
  const checking = renderToStaticMarkup(createElement(HomeView, { ...props, tools: { kind: "loading" } }));
  assert.match(checking, /Checking tmux, uv and graphify on this computer\./);
  assert.doesNotMatch(checking, /Couldn&#x27;t check|role="alert"/);
  assert.doesNotMatch(renderToStaticMarkup(createElement(HomeView, { ...props, tools: toolsReady })), /Get set up/);
});

test("Home shows the empty state, setup panel and Needs you from real reads only", () => {
  const accounts = ok(Array.from({ length: 5 }, (_, index) => account({ id: `a${index}`, name: `Claude ${index}`, status: "not_signed_in", status_label: "Not signed in" })));
  const markup = renderToStaticMarkup(createElement(HomeView, { projects: ok([]), accounts, pools: ok([]), harnesses: { kind: "not_ready" }, tools: { kind: "loading" }, onRetryProjects: () => {} }));
  assert.match(markup, /<h2>No projects yet<\/h2>/);
  assert.match(markup, /Create a project and agents start the research you pick\./);
  assert.match(markup, /<h2>Get set up<\/h2>/);
  assert.match(markup, /Claude 0 isn&#x27;t signed in\./);
  assert.doesNotMatch(markup, /Claude 3 isn/);
  assert.match(markup, /Show all \(5\)/);
  assert.equal(needsYou(accounts).rows.length, 3);
  const cards = renderToStaticMarkup(createElement(HomeView, {
    projects: ok([{ id: "p 1", name: "<Harbor> & Notes", repository_count: 1 }]), accounts: ok([account({})]),
    pools: ok([{ id: "pool-1", name: "Main", summary: "", members: [], project_count: 1 }]), harnesses: ok([]), tools: toolsReady, onRetryProjects: () => {},
  }));
  assert.match(cards, /<a class="project-card" href="#\/projects\/p%201\/overview" aria-label="&lt;Harbor&gt; &amp; Notes">/);
  assert.doesNotMatch(cards, /Get set up|No projects yet/);
  for (const [projects, text] of [[{ kind: "loading" }, /skeleton card-shape/], [{ kind: "not_ready" }, /Not ready yet/], [{ kind: "error", code: "x", message: "y" }, /Couldn&#x27;t load your projects\./]]) {
    assert.match(renderToStaticMarkup(createElement(HomeView, { projects, accounts: ok([]), pools: ok([]), harnesses: ok([]), tools: toolsReady, onRetryProjects: () => {} })), text);
  }
});

test("every app surface draws the Ember 4 brand mark, never the text-only placeholder", async () => {
  const brand = await import("../src/ui/brand.tsx");
  const shell = await import("../src/ui/shell.tsx");
  assert.equal(shell.BrandMark, brand.BrandMark);
  const mark = /<span class="brand-mark" role="img" aria-label="CrazyAgent"><svg class="brand-glyph full"/;
  const large = /<span class="brand-mark large" role="img" aria-label="CrazyAgent"><svg class="brand-glyph full"/;
  const placeholder = /<span class="brand-mark(?: large)?">CrazyAgent<\/span>/;
  for (const markup of [renderToStaticMarkup(createElement(App)), render({ engine: "down" }), render({})]) {
    assert.match(markup, mark);
    assert.doesNotMatch(markup, placeholder);
  }
  assert.match(render({}), /<aside class="sidebar" aria-label="CrazyAgent"><span class="brand-mark" role="img"/);
  const welcome = render({ profile: { kind: "ok", value: null } });
  assert.match(welcome, large);
  assert.doesNotMatch(welcome, placeholder);
});

test("a project card sums up live agents and research from the team and research reads", () => {
  const team = (agents, tasks = []) => ({ kind: "ok", value: { agents: agents.map((status, index) => ({ id: index, role: index ? "sub" : "master", status })), tasks: tasks.map((status, id) => ({ id, status })) } });
  const research = (statuses) => ({ kind: "ok", value: { jobs: statuses.map((status, id) => ({ id, status })) } });
  assert.deepEqual(cardActivity(undefined, undefined), { research: null, agents: null, waiting: false, live: false });
  assert.deepEqual(cardActivity(team(["idle", "working", "stopped"]), research(["done", "running", "queued"])),
    { research: "Research 1 of 3 done", agents: "2 agents working", waiting: false, live: true });
  assert.deepEqual(cardActivity(team(["idle"]), research(["needs_pool", "needs_pool"])),
    { research: "Research is waiting for a pool.", agents: "No agents working", waiting: true, live: true });
  assert.equal(cardActivity(team(["idle"]), { kind: "ok", value: { jobs: [{ id: 1, status: "needs_pool", error: "No account in this project's pool can run this research" }] } }).research,
    "Research is waiting: No account in this project's pool can run this research.");
  assert.deepEqual(cardActivity(team(["idle"]), research(["done", "failed", "cancelled"])),
    { research: "Research 1 of 3 done, 1 failed, 1 stopped", agents: "No agents working", waiting: false, live: false });
  assert.deepEqual(cardActivity(team(["working"]), research([])), { research: null, agents: "1 agent working", waiting: false, live: true });
  assert.equal(cardActivity(team(["starting"]), research([])).live, true);
  assert.equal(cardActivity(team(["idle"], ["queued"]), research(["done"])).live, true);
  assert.equal(cardActivity(team(["idle"], ["done", "cancelled"]), research(["done"])).live, false);
  assert.deepEqual(cardActivity({ kind: "not_ready" }, research(["running"])), { research: "Research 0 of 1 done", agents: "1 agent working", waiting: false, live: true });
  assert.deepEqual(cardActivity({ kind: "error", code: "x", message: "y" }, { kind: "not_ready" }), { research: null, agents: null, waiting: false, live: false });

  const rows = [{ id: "p1", name: "Harbor Notes", repository_count: 0 }, { id: "p2", name: "Quayside", repository_count: 0 }];
  const view = (activity) => renderToStaticMarkup(createElement(HomeView, {
    projects: ok(rows), accounts: ok([account({})]), pools: ok([{ id: "pool-1", name: "Main", summary: "", members: [], project_count: 1 }]),
    harnesses: ok([]), tools: toolsReady, onRetryProjects: () => {}, activity,
  }));
  const cards = view(new Map([["p1", cardActivity(team(["idle", "working"]), research(["done", "running"]))],
    ["p2", cardActivity(team(["idle"]), research(["needs_pool"]))]]));
  const first = cards.slice(cards.indexOf('aria-label="Harbor Notes"'), cards.indexOf('aria-label="Quayside"'));
  const second = cards.slice(cards.indexOf('aria-label="Quayside"'));
  assert.match(first, /<span class="activity">.*Research 1 of 2 done<\/span>/);
  assert.match(first, /<span class="activity">.*2 agents working<\/span>/);
  assert.match(second, /<span class="activity warn">.*Research is waiting for a pool\.<\/span>/);
  assert.match(second, /No agents working/);
  assert.doesNotMatch(view(undefined), /class="activity/);
  assert.doesNotMatch(view(new Map()), /class="activity/);
});

test("Needs you shows one row per account to sign in, and folds accounts that share a name and provider into one", () => {
  const claude = (id, name, status = "not_signed_in") => account({ id, name, provider: "claude", provider_name: "Claude", status });
  const accounts = ok([claude("c1", "ChaoticQubit"), claude("c2", "ChaoticQubit"), claude("c3", "ChaoticQubit"), claude("c5", "ChaoticQubit", "signed_in"),
    account({ id: "g1", name: "ChaoticQubit", status: "not_signed_in" }), claude("c6", "Arman03"), claude("c7", "Arman2000", "signed_in")]);
  assert.deepEqual(needsYou(accounts).all.map((row) => [row.key, row.text, row.action, row.href]), [
    ["account-c1", "3 Claude accounts named ChaoticQubit aren't signed in.", "Open accounts", "#/settings/accounts"],
    ["account-g1", "ChaoticQubit isn't signed in.", "Sign in again", "#/settings/accounts?action=sign_in&account=g1"],
    ["account-c6", "Arman03 isn't signed in.", "Sign in again", "#/settings/accounts?action=sign_in&account=c6"],
  ]);
});

test("Needs you lists accounts to sign in, research waiting for a pool and memories to review", () => {
  const accounts = ok([account({ id: "a1", name: "Claude 1", status: "not_signed_in", status_label: "Not signed in" }), account({ id: "a2", name: "Codex", status: "signed_in" })]);
  const projects = [{ id: "p1", name: "Harbor Notes", repository_count: 0 }, { id: "p 2", name: "Quayside", repository_count: 0 }];
  const jobs = (status) => ({ kind: "ok", value: { jobs: [{ id: 1, status, error: null }] } });
  const activity = new Map([["p1", cardActivity({ kind: "not_ready" }, jobs("running"))], ["p 2", cardActivity({ kind: "not_ready" }, jobs("needs_pool"))]]);
  const suggestion = (project_id, project_name, id) => ({ project_id, project_name, id, version: 1, text: "A fact" });
  const suggestions = ok([suggestion("p1", "Harbor Notes", "m1"), suggestion("p1", "Harbor Notes", "m2"), suggestion("p 2", "Quayside", "m3")]);
  const needs = needsYou(accounts, projects, activity, suggestions);
  assert.deepEqual(needs.all.map((row) => [row.project, row.text, row.action, row.href]), [
    [null, "Claude 1 isn't signed in.", "Sign in again", "#/settings/accounts?action=sign_in&account=a1"],
    ["Quayside", "Research is waiting for a pool.", "Choose pool", "#/projects/p%202/settings?section=pool"],
    ["Harbor Notes", "2 suggested memories to review.", "Review", "#/settings/memory"],
    ["Quayside", "1 suggested memory to review.", "Review", "#/settings/memory"],
  ]);
  assert.equal(needs.total, 4);
  assert.deepEqual(needs.rows, needs.all.slice(0, 3));
  assert.equal(new Set(needs.all.map((row) => row.key)).size, 4);
  assert.deepEqual(needsYou(accounts, projects, new Map(), { kind: "not_ready" }).all.map((row) => row.text), ["Claude 1 isn't signed in."]);
  assert.equal(needsYou({ kind: "loading" }).total, 0);

  const markup = renderToStaticMarkup(createElement(HomeView, {
    projects: ok(projects), accounts, pools: ok([{ id: "pool-1", name: "Main", summary: "", members: [], project_count: 1 }]),
    harnesses: ok([]), tools: toolsReady, onRetryProjects: () => {}, activity, suggestions,
  }));
  assert.match(markup, /<section class="needs" aria-label="Needs you">/);
  assert.match(markup, /<strong>Quayside<\/strong> Research is waiting for a pool\./);
  assert.match(markup, /<a class="btn small" href="#\/projects\/p%202\/settings\?section=pool">Choose pool<\/a>/);
  assert.match(markup, /<strong>Harbor Notes<\/strong> 2 suggested memories to review\./);
  assert.doesNotMatch(markup, /1 suggested memory to review/);
  assert.match(markup, /<button aria-expanded="false" type="button" class="btn quiet small">Show all \(4\)<\/button>/);
});

test("each project gets its own page, so a switch never keeps the last project's ticket or name", async () => {
  const { Screen } = await import("../src/app.tsx");
  const { ProjectPage } = await import("../src/screens/project/index.tsx");
  const projects = { kind: "ok", value: [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }] };
  const page = (id) => Screen({ route: { name: "project", id, tab: "conversation" }, session: session({ projects }) });
  assert.equal(page("alpha").type, ProjectPage);
  assert.equal(page("alpha").key, "alpha");
  assert.equal(page("beta").key, "beta");
  assert.equal(page("beta").props.name, "Beta");
});

test("each project's onboarding review and settings get their own page, so a switch never seeds from the last project", async () => {
  const { Screen } = await import("../src/app.tsx");
  const { OnboardingReview } = await import("../src/screens/intake/onboarding.tsx");
  const projects = { kind: "ok", value: [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }] };
  const review = (id) => Screen({ route: { name: "onboarding", id }, session: session({ projects }) });
  assert.equal(review("alpha").type, OnboardingReview);
  assert.equal(review("alpha").key, "alpha");
  assert.equal(review("beta").key, "beta");
  const settings = (id) => Screen({ route: { name: "project", id, tab: "settings" }, session: session({ projects }) });
  assert.notEqual(settings("alpha").key, settings("beta").key);
});
