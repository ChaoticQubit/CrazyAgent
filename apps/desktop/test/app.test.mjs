import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { App, AppView } = await import("../src/app.tsx");
const { HomeView, setupSteps, needsYou } = await import("../src/screens/dashboard/index.tsx");

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

test("an unconfirmed change shows the plain banner with Check again", () => {
  const markup = render({ unconfirmed: true });
  assert.match(markup, /We couldn&#x27;t confirm your last change\. Check it and save again\./);
  assert.match(markup, />Check again<\/button>/);
  assert.doesNotMatch(markup, /intent|Reconcile|operation/i);
});

test("routes reach Settings tabs and the project stubs owned by the projects worker", () => {
  const settings = render({}, { name: "settings", tab: "pools" });
  assert.match(settings, /<h1>Settings<\/h1>/);
  assert.match(settings, /role="tablist" aria-label="Settings sections"/);
  assert.match(settings, /<a id="tab-pools" role="tab" class="tab" href="#\/settings\/pools" aria-selected="true" tabindex="0">/);
  for (const label of ["Accounts", "Pools", "Harnesses", "Plugins and skills", "Memory", "Profile"]) assert.match(settings, new RegExp(`</svg>${label}</a>`));
  const project = render({ projects: { kind: "ok", value: [{ id: "p-1", name: "Harbor Notes", repository_count: 1 }] } }, { name: "project", id: "p-1", tab: "overview" });
  assert.match(project, /<h1>Harbor Notes<\/h1>/);
  assert.match(project, /Not ready yet/);
  assert.match(project, /aria-current="page"><svg[\s\S]*?<\/svg><span class="name">Harbor Notes<\/span>/);
  assert.match(render({}, { name: "new-project", step: "basics" }), /<h1>New project<\/h1>/);
});

const ok = (value) => ({ kind: "ok", value });
const account = (patch) => ({ id: "a1", name: "ChatGPT 1", provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription",
  method_label: "Sign in with ChatGPT", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const harness = (id, kind) => ({ id, name: id, status: kind === "limited" ? { kind, reason: "x" } : { kind }, status_label: "", executable: null,
  headless: "", tools: { kind: "switchable" }, model_selection: true, account_kinds: [] });

test("Get set up stays until one account is ready and one pool exists; harnesses never block", () => {
  const harnesses = ok([harness("codex", "ready"), harness("claude-code", "ready"), harness("pi", "not_installed")]);
  const empty = setupSteps(ok([]), ok([]), harnesses);
  assert.equal(empty.visible, true);
  assert.deepEqual(empty.items.map((item) => [item.id, item.done]), [["accounts", false], ["pools", false], ["harnesses", false]]);
  assert.equal(empty.items[2].body, "2 of 3 harnesses ready on this Mac.");
  assert.equal(empty.items[0].href, "#/settings/accounts?action=add");
  assert.equal(empty.items[1].href, "#/settings/pools?action=new");
  assert.equal(setupSteps(ok([account({ status: "not_signed_in" })]), ok([]), harnesses).items[0].done, false);
  assert.equal(setupSteps(ok([account({ status: "key_saved" })]), ok([]), harnesses).items[0].done, true);
  const pool = { id: "pool-1", name: "Main", summary: "1 ChatGPT", members: [], project_count: 0 };
  assert.equal(setupSteps(ok([account({})]), ok([pool]), harnesses).visible, false);
  const unknown = setupSteps({ kind: "not_ready" }, { kind: "not_ready" }, { kind: "not_ready" });
  assert.equal(unknown.visible, true);
  assert.equal(unknown.items[2].body, "See which coding agents are installed on this Mac.");
});

test("Home shows the empty state, setup panel and Needs you from real reads only", () => {
  const accounts = ok(Array.from({ length: 5 }, (_, index) => account({ id: `a${index}`, name: `Claude ${index}`, status: "not_signed_in", status_label: "Not signed in" })));
  const markup = renderToStaticMarkup(createElement(HomeView, { projects: ok([]), accounts, pools: ok([]), harnesses: { kind: "not_ready" }, onRetryProjects: () => {} }));
  assert.match(markup, /<h2>No projects yet<\/h2>/);
  assert.match(markup, /Create a project and agents start the research you pick\./);
  assert.match(markup, /<h2>Get set up<\/h2>/);
  assert.match(markup, /Claude 0 isn&#x27;t signed in\./);
  assert.doesNotMatch(markup, /Claude 3 isn/);
  assert.match(markup, /Show all \(5\)/);
  assert.equal(needsYou(accounts).rows.length, 3);
  const cards = renderToStaticMarkup(createElement(HomeView, {
    projects: ok([{ id: "p 1", name: "<Harbor> & Notes", repository_count: 1 }]), accounts: ok([account({})]),
    pools: ok([{ id: "pool-1", name: "Main", summary: "", members: [], project_count: 1 }]), harnesses: ok([]), onRetryProjects: () => {},
  }));
  assert.match(cards, /<a class="project-card" href="#\/projects\/p%201\/overview" aria-label="&lt;Harbor&gt; &amp; Notes">/);
  assert.doesNotMatch(cards, /Get set up|No projects yet/);
  for (const [projects, text] of [[{ kind: "loading" }, /skeleton card-shape/], [{ kind: "not_ready" }, /Not ready yet/], [{ kind: "error", code: "x", message: "y" }, /Couldn&#x27;t load your projects\./]]) {
    assert.match(renderToStaticMarkup(createElement(HomeView, { projects, accounts: ok([]), pools: ok([]), harnesses: ok([]), onRetryProjects: () => {} })), text);
  }
});
