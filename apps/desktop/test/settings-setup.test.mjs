import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { PackSettingsView, packConfirm } = await import("../src/screens/settings/pack.tsx");
const { ProjectsHomeSettingsView, homePickProblem, homeMoveLine } = await import("../src/screens/settings/projects-home.tsx");
const { GitHubSettingsView, GitHubAccountView } = await import("../src/screens/settings/github.tsx");
const { GitHubSignIn } = await import("../src/screens/intake/clone.tsx");

const ok = (value) => ({ kind: "ok", value });
const notReady = { kind: "not_ready" };
const failed = (message) => ({ kind: "error", code: "owner_read_failed", message });
const html = (type, props) => renderToStaticMarkup(createElement(type, props));
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const GB = 1024 ** 3;

const entry = (id, kind, group, state, extra = {}) => ({ id, kind, group, source: `github.com/example/${id}`, license: "MIT", on_by_default: group !== "optional", state, ...extra });
const pack = [
  entry("graphify", "skill", "always", { state: "on" }),
  entry("superpowers", "skill", "workflow", { state: "off" }),
  entry("headroom", "mcp_server", "context", { state: "on" }),
  entry("serena", "mcp_server", "optional", { state: "available" }),
  entry("linear", "mcp_server", "optional", { state: "failed", reason: "Checksum didn't match." }),
];
const setup = (extra = {}) => ({ dependencies: [], pack, running: false, ...extra });
const packProps = (status, extra = {}) => ({ status, busy: null, problem: "", onSwitch: forbidden, onAsk: forbidden, onRetry: forbidden, ...extra });

const rowOf = (markup, id) => {
  const start = markup.indexOf(`<strong>${id}</strong>`);
  assert.ok(start >= 0, `${id} is listed`);
  return markup.slice(start, markup.indexOf("</li>", start));
};

test("Settings, Pack lists the pack by group with a switch, Remove or Install on each entry", () => {
  const markup = html(PackSettingsView, packProps(ok(setup())));
  assert.match(markup, /<h2>Always on<\/h2>[\s\S]*<h2>Workflow skills<\/h2>[\s\S]*<h2>Context plugins<\/h2>[\s\S]*<h2>Optional<\/h2>/);
  const always = rowOf(markup, "graphify");
  assert.match(always, /role="switch"[^>]*aria-checked="true"[^>]*aria-disabled="true"/);
  assert.match(always, /Always on\./);
  assert.doesNotMatch(always, />Remove</);
  const off = rowOf(markup, "superpowers");
  assert.match(off, /role="switch"[^>]*aria-checked="false"/);
  assert.doesNotMatch(off, /aria-disabled/);
  assert.match(off, />Remove<\/button>/);
  assert.match(rowOf(markup, "headroom"), /MCP server/);
  const available = rowOf(markup, "serena");
  assert.doesNotMatch(available, /role="switch"/);
  assert.match(available, />Install<\/button>/);
  const broken = rowOf(markup, "linear");
  assert.match(broken, /Checksum didn&#x27;t match\./);
  assert.match(broken, />Try again<\/button>/);
});

test("Settings, Pack locks every change while setup runs or another change is in flight", () => {
  for (const props of [packProps(ok(setup({ running: true }))), packProps(ok(setup()), { busy: "headroom" })]) {
    const markup = html(PackSettingsView, props);
    assert.match(rowOf(markup, "superpowers"), /aria-disabled="true"[\s\S]*Wait for the current change\.[\s\S]*<button[^>]*disabled=""[^>]*>Remove<\/button>/);
    assert.match(rowOf(markup, "serena"), /<button[^>]*disabled=""[^>]*>Install<\/button>/);
  }
  assert.match(html(PackSettingsView, packProps(ok(setup({ running: true })))), /role="status">Setup is installing\. Changes wait until it finishes\./);
  assert.doesNotMatch(html(PackSettingsView, packProps(ok(setup()))), /role="status"/);
  assert.match(html(PackSettingsView, packProps(ok(setup()), { problem: "This entry is always on and can't be removed." })), /role="alert">This entry is always on/);
});

test("Settings, Pack shows not ready, errors and loading without inventing a pack", () => {
  assert.match(html(PackSettingsView, packProps(notReady)), /The default pack isn&#x27;t ready yet/);
  assert.match(html(PackSettingsView, packProps(failed("Couldn't read this from the engine. Try again."))), /role="alert"[\s\S]*Try again/);
  assert.doesNotMatch(html(PackSettingsView, packProps({ kind: "loading" })), /superpowers/);
});

test("pack confirms say what removing deletes and that installing an MCP server lets agents run it", () => {
  assert.deepEqual(packConfirm({ kind: "remove", entry: pack[1] }), { title: "Remove superpowers?", confirm: "Remove",
    body: "CrazyAgent deletes the skill it installed for every project. You can install it again from this page." });
  assert.equal(packConfirm({ kind: "remove", entry: pack[2] }).body, "CrazyAgent deletes the MCP server it installed for every project. You can install it again from this page.");
  assert.deepEqual(packConfirm({ kind: "install", entry: pack[3] }), { title: "Install serena?", confirm: "Install",
    body: "Installing it lets your agents run this MCP server in every project. It comes from github.com/example/serena." });
  assert.equal(packConfirm({ kind: "install", entry: entry("coderabbit", "skill", "optional", { state: "available" }) }).body,
    "Agents in every project can use this skill. It comes from github.com/example/coderabbit.");
});

test("a new projects folder must differ from the current one and sit outside it", () => {
  const current = "/Users/mira/Projects";
  assert.equal(homePickProblem("/Users/mira/Work", current), null);
  assert.equal(homePickProblem("/Users/mira/Projects-old", current), null);
  assert.equal(homePickProblem("/Users/mira/Work", null), null);
  assert.equal(homePickProblem("/Users/mira/Projects/", current), "This is already your projects folder.");
  assert.equal(homePickProblem("/Users/mira/Projects/shop", current), "Choose a folder outside your projects folder.");
  assert.equal(homePickProblem("/Users/mira", current), "This folder holds your projects folder. Choose another one.");
  assert.equal(homePickProblem("Work", current), "Choose the folder again with Choose folder.");
});

test("moving the projects folder shows the size before it asks", () => {
  assert.equal(homeMoveLine("/Users/mira/Projects", "/Users/mira/Work", null), "Checking how much is in ~/Projects...");
  assert.equal(homeMoveLine("/Users/mira/Projects", "/Users/mira/Work", 3 * GB),
    "CrazyAgent moves every project folder in ~/Projects (3 GB) into ~/Work and uses ~/Work from then on. Agents can't run while it moves.");
});

test("Settings, Projects folder says Use this folder moves nothing and offers the move separately", () => {
  const base = { busy: false, problem: "", onChoose: forbidden, onUse: forbidden, onMove: forbidden, onCancel: forbidden, onRetry: forbidden };
  const home = ok({ path: "/Users/mira/Projects", watching: true });
  const shown = html(ProjectsHomeSettingsView, { ...base, home, picked: null });
  assert.match(shown, /Your projects folder is <span[^>]*>~\/Projects<\/span>/);
  assert.match(shown, /Watching ~\/Projects\./);
  assert.match(shown, />Choose another folder<\/button>/);
  assert.doesNotMatch(shown, /Use this folder/);
  const picked = html(ProjectsHomeSettingsView, { ...base, home, picked: "/Users/mira/Work" });
  assert.match(picked, /Use <span[^>]*>~\/Work<\/span>\?/);
  assert.match(picked, /Use this folder moves nothing\. Projects already in ~\/Projects stay there\./);
  assert.match(picked, />Use this folder<\/button>/);
  assert.match(picked, />Move my projects there<\/button>/);
  assert.doesNotMatch(picked, /Choose another folder/);
  const first = html(ProjectsHomeSettingsView, { ...base, home: ok({ path: null, watching: false }), picked: "/Users/mira/Work" });
  assert.match(first, />Use this folder<\/button>/);
  assert.doesNotMatch(first, /Move my projects there|moves nothing/);
  assert.match(html(ProjectsHomeSettingsView, { ...base, home: ok({ path: null, watching: false }), picked: null }), /No projects folder yet[\s\S]*>Choose folder<\/button>/);
  assert.match(html(ProjectsHomeSettingsView, { ...base, home, picked: null, problem: "Nope." }), /role="alert">Nope\.</);
  assert.match(html(ProjectsHomeSettingsView, { ...base, home: notReady, picked: null }), /The projects folder isn&#x27;t ready yet/);
});

test("Settings, GitHub shows the account with Sign out, or the three sign-in ways", () => {
  const signedIn = { signed_in: true, login: "mira", method: "device", waiting: false, problem: null };
  const signedOut = { signed_in: false, login: null, method: "none", waiting: false, problem: null };
  const panels = {
    onRetry: forbidden,
    signIn: (status) => createElement(GitHubSignIn, { status, reloadStatus: forbidden }),
    account: (status) => createElement(GitHubAccountView, { status, busy: false, problem: "", onSignOut: forbidden }),
  };
  const account = html(GitHubSettingsView, { ...panels, status: ok(signedIn) });
  assert.match(account, /Signed in to GitHub as mira\./);
  assert.match(account, /keychain[\s\S]*Agents never get it\./);
  assert.match(account, />Sign out of GitHub<\/button>/);
  assert.doesNotMatch(account, /Sign in with GitHub/);
  const cli = html(GitHubAccountView, { status: { ...signedIn, login: null, method: "cli" }, busy: false, problem: "", onSignOut: forbidden });
  assert.match(cli, /Signed in to GitHub through the GitHub CLI\./);
  assert.match(cli, /Signing out here leaves the GitHub CLI signed in\./);
  const out = html(GitHubSettingsView, { ...panels, status: ok(signedOut) });
  assert.match(out, />Sign in with GitHub<\/button>/);
  assert.match(out, />Use the GitHub CLI<\/button>/);
  assert.match(out, />Paste a token instead<\/button>/);
  assert.doesNotMatch(out, /Sign out/);
  assert.match(html(GitHubSettingsView, { ...panels, status: ok({ ...signedOut, problem: "GitHub didn't accept the saved token." }) }), /role="alert">GitHub didn&#x27;t accept the saved token\./);
  assert.match(html(GitHubSettingsView, { ...panels, status: notReady }), /Signing in to GitHub isn&#x27;t ready yet/);
});
