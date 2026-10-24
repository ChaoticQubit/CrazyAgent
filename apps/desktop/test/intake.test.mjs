import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const intake = await import("../src/screens/intake/intake.ts");
const router = await import("../src/ui/router.ts");
const { HomeFolderView } = await import("../src/screens/intake/home-folder.tsx");
const { FoundProjectsView, SetUpDialogBody } = await import("../src/screens/intake/found.tsx");
const { AddFolderView } = await import("../src/screens/intake/add-folder.tsx");
const { GitHubSignInView, RepoListView } = await import("../src/screens/intake/clone.tsx");
const { OnboardingReviewView } = await import("../src/screens/intake/onboarding.tsx");
const { ToolsSetupView } = await import("../src/screens/intake/setup-step.tsx");
const { Welcome } = await import("../src/screens/first-run/index.tsx");
const { Icon } = await import("../src/ui/icon.tsx");

const ok = (value) => ({ kind: "ok", value });
const notReady = { kind: "not_ready" };
const failed = (code, message = "Something went wrong. Try again.") => ({ kind: "error", code, message });
const html = (type, props) => renderToStaticMarkup(createElement(type, props));
const noop = () => {};
const GB = 1024 ** 3;

const folder = (state, extra = {}) => ({ name: "shop", path: "/Users/mira/Projects/shop", git: true, known: false, state, ...extra });
const setup = (extra = {}) => ({ about: "", tech_stack: [], platforms: [], research: [], pool: null, team: null, documents: [], ...extra });
const stored = (extra = {}) => ({
  project_id: "p1", name: "shop", about: "", tech_stack: [], platforms: [], research: [], pool: "main",
  team: { master: "claude", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 },
  documents: [], folder: "/Users/mira/Projects/shop", created_ms: 1, revision: 4, ...extra,
});
const view = (extra = {}) => ({
  project_id: "p1", graph: { kind: "done" }, mapping: { kind: "done" },
  draft: setup({ about: "A shop for plants.", tech_stack: ["Rust", "React"], platforms: ["website"] }),
  facts: { languages: ["Rust"], build_commands: ["cargo build"], test_commands: ["cargo test"], entry_points: ["src/main.rs"] },
  missing: [], name: "Plant shop", overview: "# Plant shop\n\nBuild with `cargo build`.", ...extra,
});

test("each folder state has its badge, line and one action", () => {
  assert.deepEqual(intake.foundRow(folder("found")), { badge: "Found", tone: "info", line: "New in your projects folder. Set it up to start working in it.", action: "setup" });
  assert.equal(intake.foundRow(folder("found", { known: true })).line, "It has CrazyAgent files from before. Set it up to reuse them.");
  assert.deepEqual(intake.foundRow(folder("onboarding", { project_id: "p1" })), { badge: "Setting up", tone: "warn", line: "Review what CrazyAgent found, then confirm.", action: "continue" });
  assert.deepEqual(intake.foundRow(folder("ready", { project_id: "p1" })), { badge: "Ready", tone: "ok", line: "Set up and ready.", action: "open" });
  assert.deepEqual(intake.foundRow(folder("missing", { project_id: "p1" })),
    { badge: "Folder missing", tone: "danger", line: "The folder isn't in your projects folder anymore. Put it back, or remove the project.", action: "remove" });
  assert.deepEqual(intake.foundRow(folder("missing")), { badge: "Folder missing", tone: "danger", line: "The folder isn't in your projects folder anymore. Put it back to see it here.", action: null });
  assert.equal(intake.foundRow(folder("onboarding")).action, null);
  assert.equal(intake.foundRow(folder("ready")).action, null);
});

test("Home lists found, then setting up, then missing folders by name; ready ones are project cards", () => {
  const list = [folder("ready", { name: "a" }), folder("missing", { name: "b" }), folder("found", { name: "z" }), folder("found", { name: "c" }), folder("onboarding", { name: "d" })];
  assert.deepEqual(intake.pendingFolders(list).map((item) => `${item.state}:${item.name}`), ["found:c", "found:z", "onboarding:d", "missing:b"]);
  assert.deepEqual(list.map((item) => item.name), ["a", "b", "z", "c", "d"]);
});

test("the projects folder line says whether new folders show up live", () => {
  assert.equal(intake.homeLine({ path: null, watching: false }), null);
  assert.equal(intake.homeLine({ path: "/Users/mira/Projects", watching: true }), "Watching ~/Projects. Drop a project folder into it with Finder and it shows up here.");
  assert.equal(intake.homeLine({ path: "/Users/mira/Projects", watching: false }), "~/Projects is checked when CrazyAgent starts. Live updates are off right now.");
});

test("a picked folder must be one project folder outside the projects folder", () => {
  const home = "/Users/mira/Projects";
  assert.equal(intake.pickedFolderProblem("/Users/mira/Code/shop", home), null);
  assert.equal(intake.pickedFolderProblem("/Users/mira/Code/shop/", home), null);
  assert.equal(intake.pickedFolderProblem("/Users/mira/Projects/shop", home), "This folder is already in your projects folder. It shows on Home.");
  assert.equal(intake.pickedFolderProblem("/Users/mira/Projects", home), "This folder holds your projects folder. Choose one project's folder.");
  assert.equal(intake.pickedFolderProblem("/Users/mira", home), "This folder holds your projects folder. Choose one project's folder.");
  assert.equal(intake.pickedFolderProblem("/Users/mira/Projects-old/shop", home), null);
  assert.equal(intake.pickedFolderProblem("Code/shop", home), "Choose the folder again with Choose folder.");
  assert.equal(intake.pickedFolderProblem("/Users/mira/a\u0007b", home), "Choose the folder again with Choose folder.");
  assert.equal(intake.pickedFolderProblem("/", null), "Choose the folder again with Choose folder.");
  assert.equal(intake.folderName("/Users/mira/Code/shop/"), "shop");
});

test("Move is the default and Copy shows the size first, with a warning above 1 GB", () => {
  const home = "/Users/mira/Projects";
  assert.deepEqual(intake.sizeLine("move", null, home), { text: "Moves the folder into ~/Projects. It won't stay at its old place.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", null, home), { text: "Checking the folder size...", warn: false });
  assert.deepEqual(intake.sizeLine("copy", 512 * 1024 * 1024, home), { text: "Copies 512 MB into ~/Projects. The original stays where it is.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", GB, home), { text: "Copies 1 GB into ~/Projects. The original stays where it is.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", 2.5 * GB, home),
    { text: "This copies 2.5 GB into ~/Projects. That takes a while and uses as much disk again. Move is instant on the same disk.", warn: true });
});

test("the device code shows how long it still works and only GitHub pages open", () => {
  assert.deepEqual(intake.deviceLine(900, 1_000, 1_000), { expired: false, text: "Enter this code on the GitHub page. It works for 15 more minutes." });
  assert.deepEqual(intake.deviceLine(900, 1_000, 1_000 + 899_500), { expired: false, text: "Enter this code on the GitHub page. It works for 1 more minute." });
  assert.deepEqual(intake.deviceLine(900, 1_000, 1_000 + 900_000), { expired: true, text: "This code expired. Start again to get a new one." });
  assert.equal(intake.gitHubPage("https://github.com/login/device"), "https://github.com/login/device");
  assert.equal(intake.gitHubPage("https://github.com.evil.test/login/device"), null);
  assert.equal(intake.gitHubPage("http://github.com/login/device"), null);
  assert.equal(intake.gitHubPage("javascript:alert(1)"), null);
});

test("the GitHub account line names the sign-in method", () => {
  assert.equal(intake.accountLine({ signed_in: true, login: "mira", method: "device", waiting: false, problem: null }), "Signed in to GitHub as mira.");
  assert.equal(intake.accountLine({ signed_in: true, login: "mira", method: "token", waiting: false, problem: null }), "Signed in to GitHub as mira with a token.");
  assert.equal(intake.accountLine({ signed_in: true, login: null, method: "cli", waiting: false, problem: null }), "Signed in to GitHub through the GitHub CLI.");
  assert.equal(intake.accountLine({ signed_in: false, login: null, method: "none", waiting: false, problem: null }), "Not signed in to GitHub.");
});

test("repositories filter by name and read their size and branch", () => {
  const repos = [
    { full_name: "mira/plant-shop", private: true, default_branch: "main", size_kb: 2048 },
    { full_name: "mira/notes", private: false, default_branch: "trunk", size_kb: 3 },
  ];
  assert.deepEqual(intake.repoMatches(repos, " PLANT ").map((repo) => repo.full_name), ["mira/plant-shop"]);
  assert.equal(intake.repoMatches(repos, "").length, 2);
  assert.equal(intake.repoMeta(repos[0]), "Private, 2 MB, branch main");
  assert.equal(intake.repoMeta(repos[1]), "Public, 3 KB, branch trunk");
});

test("onboarding steps use the fixed words and keep the failure reason", () => {
  assert.deepEqual(intake.stepView({ kind: "waiting" }), { word: "Waiting", tone: "neutral", line: null });
  assert.deepEqual(intake.stepView({ kind: "running" }), { word: "Working", tone: "info", line: null });
  assert.deepEqual(intake.stepView({ kind: "done" }), { word: "Done", tone: "ok", line: null });
  assert.deepEqual(intake.stepView({ kind: "failed", reason: "graphify isn't installed." }), { word: "Failed", tone: "danger", line: "graphify isn't installed." });
});

test("missing items name the form fields, once each", () => {
  assert.deepEqual(intake.missingLabels(["about", "tech_stack", "about", "build_commands"]), ["What is it about", "Tech stack", "build commands"]);
  assert.deepEqual(intake.missingLabels([]), []);
});

test("the review form starts from the mapping agent's draft and keeps the saved pool and team", () => {
  const draft = intake.onboardingDraft(view(), stored());
  assert.equal(draft.name, "Plant shop");
  assert.equal(draft.about, "A shop for plants.");
  assert.deepEqual(draft.stack, ["Rust", "React"]);
  assert.deepEqual(draft.platforms, ["website"]);
  assert.deepEqual(draft.research, []);
  assert.equal(draft.pool, "main");
  assert.equal(draft.team.master, "claude");
  assert.equal(intake.onboardingDraft(view({ name: "  " }), stored()).name, "shop");
  assert.equal(intake.onboardingDraft(view({ name: undefined }), stored()).name, "shop");
});

test("confirm waits for the mapping agent unless a step failed", () => {
  assert.deepEqual(intake.confirmReady(view()), { ready: true, line: "Check the answers, fill in anything missing, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ mapping: { kind: "failed", reason: "x" } })), { ready: true, line: "The mapping agent stopped. Fill in the form yourself, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "failed", reason: "x" }, mapping: { kind: "waiting" } })),
    { ready: true, line: "The code graph couldn't be built. Fill in the form yourself, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "running" }, mapping: { kind: "waiting" } })),
    { ready: false, line: "Building the code graph. The form fills in when the mapping agent finishes." });
  assert.deepEqual(intake.confirmReady(view({ mapping: { kind: "running" } })),
    { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." });
  assert.equal(intake.settled(view()), true);
  assert.equal(intake.settled(view({ mapping: { kind: "running" } })), false);
  assert.equal(intake.settled(view({ graph: { kind: "failed", reason: "x" }, mapping: { kind: "waiting" } })), true);
});

test("a started onboarding opens only with a project id from an applied receipt", () => {
  const base = { kind: "start_onboarding", generation: "3", project_id: "p9", baseline_id: null, model_access: null, project_spec: null, operation_id: "o" };
  assert.deepEqual(intake.startedProject({ ...base, outcome: "applied", pending: false, rejection: null }), { kind: "started", id: "p9" });
  assert.deepEqual(intake.startedProject({ ...base, project_id: null, outcome: "applied", pending: false, rejection: null }), { kind: "unconfirmed" });
  assert.deepEqual(intake.startedProject({ ...base, outcome: "rejected", pending: false, rejection: "pool_missing" }), { kind: "rejected", code: "pool_missing" });
  assert.deepEqual(intake.startedProject({ ...base, outcome: "uncertain", pending: true, rejection: null }), { kind: "unconfirmed" });
});

const dep = (name, state, label = name) => ({ name, label, state });

test("setup lists each tool's state in plain words", () => {
  assert.deepEqual(intake.dependencyView(dep("tmux", { state: "found", version: "3.5a" })),
    { title: "tmux", about: "Runs each agent in a live terminal.", word: "Found", tone: "ok", line: "Version 3.5a on this Mac." });
  assert.deepEqual(intake.dependencyView(dep("uv", { state: "installed", version: "0.9.2" })),
    { title: "uv", about: "Installs graphify.", word: "Installed", tone: "ok", line: "Version 0.9.2 installed." });
  assert.deepEqual(intake.dependencyView(dep("graphify", { state: "installing" })),
    { title: "graphify", about: "Builds a code graph of each project.", word: "Installing", tone: "info", line: null });
  assert.deepEqual(intake.dependencyView(dep("graphify", { state: "failed", reason: "Checksum didn't match." })).line, "Checksum didn't match.");
  assert.deepEqual(intake.dependencyView(dep("tmux", { state: "missing" })).line, "Setup installs it.");
  assert.equal(intake.dependencyView(dep("uv", { state: "missing" }, "uv (Python tools)")).title, "uv (Python tools)");
  assert.equal(intake.dependencyView(dep("uv", { state: "missing" }, "  ")).title, "uv");
});

const entry = (id, kind, group, on_by_default, state, extra = {}) => ({ id, kind, group, source: `github.com/example/${id}`, license: "MIT", on_by_default, state, ...extra });
const pack = [
  entry("graphify", "skill", "always", true, { state: "available" }),
  entry("superpowers", "skill", "workflow", true, { state: "available" }),
  entry("context-mode", "mcp_server", "context", true, { state: "available" }),
  entry("headroom", "mcp_server", "context", true, { state: "on" }, { license: "Apache-2.0" }),
  entry("serena", "mcp_server", "optional", false, { state: "available" }),
];

test("the pack starts with the on-by-default entries ticked and names every ticked MCP server before approval", () => {
  assert.deepEqual(intake.packTicks(pack), ["graphify", "superpowers", "context-mode", "headroom"]);
  assert.equal(intake.approvalLine(pack, ["superpowers", "context-mode", "headroom"]),
    "Continuing lets your agents run these MCP servers: context-mode, headroom. You can turn any of them off later.");
  assert.equal(intake.approvalLine(pack, ["superpowers"]), null);
  assert.equal(intake.setupBusy({ dependencies: [dep("uv", { state: "installing" })], pack: [], running: false }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack: [entry("serena", "mcp_server", "optional", false, { state: "installing" })], running: false }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack, running: true }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack, running: false }), false);
});

test("pack states read in plain words and only on or off entries count as installed", () => {
  assert.deepEqual(intake.packStateView({ state: "available" }), { word: "Not installed", tone: "neutral", line: null });
  assert.deepEqual(intake.packStateView({ state: "installing" }), { word: "Installing", tone: "info", line: null });
  assert.deepEqual(intake.packStateView({ state: "on" }), { word: "On", tone: "ok", line: null });
  assert.deepEqual(intake.packStateView({ state: "off" }), { word: "Off", tone: "neutral", line: null });
  assert.deepEqual(intake.packStateView({ state: "failed", reason: "Checksum didn't match." }), { word: "Failed", tone: "danger", line: "Checksum didn't match." });
  assert.deepEqual(pack.filter(intake.installed).map((item) => item.id), ["headroom"]);
  assert.equal(intake.installed(entry("x", "skill", "workflow", true, { state: "off" })), true);
  assert.equal(intake.installed(entry("x", "skill", "workflow", true, { state: "failed", reason: "no" })), false);
});

test("the pack groups follow the manifest order and keep pack order inside each group", () => {
  const groups = intake.packGroups([...pack, entry("caveman", "skill", "reply_style", true, { state: "available" }), entry("odd", "skill", "later", false, { state: "available" })]);
  assert.deepEqual(groups.map((group) => [group.title, group.entries.map((item) => item.id)]), [
    ["Always on", ["graphify"]], ["Workflow skills", ["superpowers"]], ["Reply style", ["caveman"]],
    ["Context plugins", ["context-mode", "headroom"]], ["Optional", ["serena"]], ["Other", ["odd"]],
  ]);
  assert.deepEqual(intake.packGroups([]), []);
});

test("setup progress counts every tool, the always-on entries and the ticked pack", () => {
  const status = {
    dependencies: [dep("tmux", { state: "found", version: "3.5a" }), dep("uv", { state: "installing" }), dep("graphify", { state: "failed", reason: "no" })],
    pack: [entry("graphify", "skill", "always", true, { state: "on" }), entry("superpowers", "skill", "workflow", true, { state: "installing" }),
      entry("serena", "mcp_server", "optional", false, { state: "available" })],
    running: true,
  };
  assert.deepEqual(intake.setupProgress(status, ["superpowers"]), { done: 3, total: 5, failed: 1 });
  assert.deepEqual(intake.setupProgress(status, []), { done: 3, total: 4, failed: 1 });
});

test("new routes for adding a project and reviewing its setup parse and format back", () => {
  for (const [hash, route] of [
    ["#/projects/add/clone", { name: "add-project", way: "clone" }],
    ["#/projects/add/folder", { name: "add-project", way: "folder" }],
    ["#/projects/p%2F1/setup", { name: "onboarding", id: "p/1" }],
  ]) {
    assert.deepEqual(router.parseRoute(hash), route);
    assert.equal(router.formatRoute(route), hash);
  }
  assert.deepEqual(router.parseRoute("#/projects/add/zip"), { name: "project", id: "add", tab: "overview" });
  assert.deepEqual(router.parseRoute("#/projects/p1/overview"), { name: "project", id: "p1", tab: "overview" });
});

test("the projects folder chooser shows each state", () => {
  assert.match(html(HomeFolderView, { home: notReady, picked: null, busy: false, problem: "", onChoose: noop, onSave: noop, onContinue: noop }), /Not ready yet/);
  const empty = html(HomeFolderView, { home: ok({ path: null, watching: false }), picked: null, busy: false, problem: "", onChoose: noop, onSave: noop });
  assert.match(empty, />Choose folder<\/button>/);
  assert.doesNotMatch(empty, /Use this folder/);
  const picked = html(HomeFolderView, { home: ok({ path: null, watching: false }), picked: "/Users/mira/Projects", busy: false, problem: "", onChoose: noop, onSave: noop });
  assert.match(picked, /~\/Projects/);
  assert.match(picked, />Use this folder<\/button>/);
  const set = html(HomeFolderView, { home: ok({ path: "/Users/mira/Projects", watching: true }), picked: null, busy: false, problem: "", onChoose: noop, onSave: noop, onContinue: noop });
  assert.match(set, /Your projects folder is/);
  assert.match(set, />Continue<\/button>/);
  const moved = html(HomeFolderView, { home: ok({ path: "/Users/mira/Projects", watching: true }), picked: "/Users/mira/Work", busy: false, problem: "Nope.", onChoose: noop, onSave: noop });
  assert.match(moved, /Projects already in the old folder stay there\./);
  assert.match(moved, /role="alert">Nope\.</);
});

test("Home shows found, setting up and missing folders with one action each", () => {
  const props = { home: ok({ path: "/Users/mira/Projects", watching: true }), onSetUp: noop, onOpen: noop, onRemove: noop, onRetry: noop };
  const markup = html(FoundProjectsView, { ...props, found: ok([folder("found", { name: "garden" }), folder("onboarding", { name: "shop", project_id: "p1" }), folder("missing", { name: "old", project_id: "p2", git: false }), folder("ready", { name: "live", project_id: "p3" })]) });
  assert.match(markup, /In your projects folder/);
  assert.match(markup, /Watching ~\/Projects\./);
  assert.match(markup, /garden[\s\S]*Found[\s\S]*>Set up this project<\/button>/);
  assert.match(markup, /shop[\s\S]*Setting up[\s\S]*>Continue setup<\/button>/);
  assert.match(markup, /old[\s\S]*Folder missing[\s\S]*>Remove project<\/button>/);
  assert.match(markup, /No git yet/);
  assert.doesNotMatch(markup, /live/);
  assert.match(html(FoundProjectsView, { ...props, found: ok([folder("ready")]) }), /Watching ~\/Projects\./);
  assert.doesNotMatch(html(FoundProjectsView, { ...props, found: ok([folder("ready")]) }), /In your projects folder/);
  assert.match(html(FoundProjectsView, { ...props, found: failed("owner_read_failed", "Couldn't read this from the engine. Try again.") }), /role="alert"[\s\S]*Try again/);
  assert.match(html(FoundProjectsView, { ...props, home: notReady, found: { kind: "loading" } }), /Not ready yet/);
  const unset = html(FoundProjectsView, { ...props, home: ok({ path: null, watching: false }), found: { kind: "loading" }, chooser: createElement("p", null, "CHOOSER") });
  assert.match(unset, /Choose your projects folder/);
  assert.match(unset, /CHOOSER/);
});

test("setting up a found folder asks for a pool first", () => {
  const pools = ok([{ pool: { id: "main", name: "Main", members: ["a"], projects: [], roles: [], fallback_allowed: false }, members: [] }]);
  const markup = html(SetUpDialogBody, { folder: folder("found"), pools, pool: "main", onPool: noop, problem: "" });
  assert.match(markup, /builds a code graph of shop/);
  assert.match(markup, /Research stays off/);
  assert.match(markup, /Main/);
  assert.match(html(SetUpDialogBody, { folder: folder("found"), pools: ok([]), pool: "", onPool: noop, problem: "" }), /Create a pool first/);
});

test("adding an existing folder shows Move by default, the Copy size warning and the clone suggestion", () => {
  const base = { home: ok({ path: "/Users/mira/Projects", watching: true }), busy: false, problem: "", onChoose: noop, onMode: noop, onAdd: noop, onClone: noop };
  const none = html(AddFolderView, { ...base, source: null, size: null, mode: "move" });
  assert.match(none, /<h1>Add an existing folder<\/h1>/);
  assert.match(none, />Choose folder<\/button>/);
  assert.doesNotMatch(none, /Move into projects folder/);
  const move = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: { kind: "loading" }, mode: "move" });
  assert.match(move, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Move</);
  assert.match(move, /Moves the folder into ~\/Projects\./);
  assert.match(move, />Move into projects folder<\/button>/);
  const big = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 3 * GB }), mode: "copy" });
  assert.match(big, /This copies 3 GB/);
  assert.match(big, /class="notice"/);
  assert.match(big, />Copy into projects folder<\/button>/);
  const hosted = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, github: "mira/plant-shop" }), mode: "move" });
  assert.match(hosted, /mira\/plant-shop/);
  assert.match(hosted, />Clone instead<\/button>/);
  assert.match(html(AddFolderView, { ...base, home: ok({ path: null, watching: false }), source: null, size: null, mode: "move" }), /Choose your projects folder first/);
  assert.match(html(AddFolderView, { ...base, home: notReady, source: null, size: null, mode: "move" }), /Not ready yet/);
});

test("GitHub sign-in shows the three ways, then the device code, and the token field never holds a value", () => {
  const base = { device: null, now: 0, busy: false, problem: "", tokenOpen: false, tokenRef: { current: null }, onDevice: noop, onCli: noop, onTokenOpen: noop, onToken: noop, onOpenPage: noop };
  const start = html(GitHubSignInView, base);
  assert.match(start, />Sign in with GitHub<\/button>/);
  assert.match(start, />Use the GitHub CLI<\/button>/);
  assert.match(start, />Paste a token instead<\/button>/);
  assert.match(start, /Agents never get it/);
  const code = html(GitHubSignInView, { ...base, device: { code: { user_code: "WDJB-MJHT", verification_uri: "https://github.com/login/device", expires_s: 900 }, started: 0 } });
  assert.match(code, /WDJB-MJHT/);
  assert.match(code, /It works for 15 more minutes\./);
  assert.match(code, />Open GitHub<\/button>/);
  const expired = html(GitHubSignInView, { ...base, now: 901_000, device: { code: { user_code: "WDJB-MJHT", verification_uri: "https://github.com/login/device", expires_s: 900 }, started: 0 } });
  assert.match(expired, />Start again<\/button>/);
  const token = html(GitHubSignInView, { ...base, tokenOpen: true });
  assert.match(token, /<input id="gh-token"[^>]*type="password"/);
  assert.doesNotMatch(token, /<input id="gh-token"[^>]*value=/);
});

test("the repository list shows each repository with Clone and a search", () => {
  const repos = [{ full_name: "mira/plant-shop", private: true, default_branch: "main", size_kb: 2048 }];
  const base = { status: { signed_in: true, login: "mira", method: "device", waiting: false, problem: null }, query: "", onQuery: noop, more: false, onMore: noop, cloning: null, onClone: noop };
  const markup = html(RepoListView, { ...base, repos: ok(repos) });
  assert.match(markup, /Signed in to GitHub as mira\./);
  assert.match(markup, /mira\/plant-shop/);
  assert.match(markup, /Private, 2 MB, branch main/);
  assert.match(markup, />Clone<\/button>/);
  assert.match(html(RepoListView, { ...base, repos: ok([]) }), /No repositories on this account\./);
  assert.match(html(RepoListView, { ...base, query: "zzz", repos: ok(repos) }), /No repositories match/);
  assert.match(html(RepoListView, { ...base, repos: ok(repos), more: true }), />Show more<\/button>/);
  assert.match(html(RepoListView, { ...base, repos: notReady }), /Not ready yet/);
});

test("the setup review shows the steps, the overview, missing items and a confirm that waits", () => {
  const catalog = ok({ tech_stack: [], platforms: [{ kind: "website", label: "Website" }], research: [{ kind: "market_research", label: "Market research" }], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "Plant shop", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop };
  const working = html(OnboardingReviewView, { ...base, view: ok(view({ mapping: { kind: "running" }, missing: [] })), draft: null });
  assert.match(working, /<h1>Set up Plant shop<\/h1>/);
  assert.match(working, /Build the code graph[\s\S]*Done/);
  assert.match(working, /Map the project[\s\S]*Working/);
  assert.match(working, /The mapping agent is reading the project\./);
  assert.match(working, /<button[^>]*disabled=""[^>]*>Confirm and finish setup<\/button>/);
  const draft = intake.onboardingDraft(view(), stored());
  const done = html(OnboardingReviewView, { ...base, view: ok(view({ missing: ["platforms", "about"] })), draft });
  assert.match(done, /<h2[^>]*>Plant shop<\/h2>/);
  assert.match(done, /Not found: Target platforms, What is it about\./);
  assert.match(done, /id="ob-name"[^>]*value="Plant shop"/);
  assert.match(done, /Target platforms[\s\S]*Missing/);
  assert.match(done, /Research is off for existing code/);
  assert.doesNotMatch(done, /<button[^>]*disabled=""[^>]*>Confirm and finish setup<\/button>/);
  assert.match(html(OnboardingReviewView, { ...base, view: ok(view({ graph: { kind: "failed", reason: "graphify isn't installed." }, mapping: { kind: "waiting" } })), draft }), /graphify isn&#x27;t installed\./);
  assert.match(html(OnboardingReviewView, { ...base, view: notReady, draft: null }), /Not ready yet/);
});

test("the tools step lists dependencies and the pack by group, and names the MCP servers it approves", () => {
  const value = { dependencies: [dep("tmux", { state: "found", version: "3.5a" }), dep("graphify", { state: "missing" })], pack, running: false };
  const base = { status: ok(value), ticks: intake.packTicks(pack), onTick: noop, busy: false, started: false, problem: "", onRun: noop, onContinue: noop };
  const markup = html(ToolsSetupView, base);
  assert.match(markup, /tmux[\s\S]*Found[\s\S]*Version 3\.5a on this Mac\./);
  assert.match(markup, /graphify[\s\S]*Not installed/);
  assert.match(markup, /<legend>Always on<\/legend>[\s\S]*graphify[\s\S]*always on/);
  assert.match(markup, /<legend>Workflow skills<\/legend>[\s\S]*superpowers/);
  assert.match(markup, /<legend>Context plugins<\/legend>[\s\S]*context-mode[\s\S]*MCP server/);
  assert.match(markup, /<legend>Optional<\/legend>[\s\S]*serena[\s\S]*off by default/);
  assert.match(markup, /headroom[\s\S]*>On</);
  assert.match(markup, /Continuing lets your agents run these MCP servers: context-mode, headroom\./);
  assert.match(markup, />Set up tools<\/button>/);
  assert.match(markup, />Skip for now<\/button>/);
  assert.doesNotMatch(markup, /role="status"/);
  const finished = html(ToolsSetupView, { ...base, started: true });
  assert.match(finished, /Setup finished: 2 of 6 done\./);
  assert.match(finished, />Continue<\/button>/);
  assert.doesNotMatch(finished, /Skip for now/);
  const running = html(ToolsSetupView, { ...base, status: ok({ ...value, running: true }) });
  assert.match(running, /Installing: 2 of 6 done\. You can continue; setup keeps going\./);
  assert.match(running, />Continue<\/button>/);
  assert.match(html(ToolsSetupView, { ...base, status: notReady }), /Not ready yet[\s\S]*>Continue<\/button>/);
});

test("first run walks profile, projects folder, tools and accounts in that order", () => {
  const steps = (step) => html(Welcome, { onSaved: noop, initialStep: step });
  assert.match(steps("profile"), /<h1>Tell CrazyAgent about you<\/h1>/);
  assert.match(steps("profile"), /<li aria-current="step">Profile<\/li><li>Projects folder<\/li><li>Tools<\/li><li>Accounts<\/li>/);
  assert.match(steps("home"), /<h1>Choose your projects folder<\/h1>/);
  assert.match(steps("tools"), /<h1>Set up tools<\/h1>/);
  const accounts = steps("accounts");
  assert.match(accounts, /<h1>Add an account<\/h1>/);
  assert.match(accounts, />Add account<\/button>/);
  assert.match(accounts, />Skip for now<\/button>/);
});

test("the projects folder, Add, clone and setup screens draw their Lucide icons", () => {
  const svg = (name, size) => html(Icon, { name, size });
  const props = { home: ok({ path: "/Users/mira/Projects", watching: true }), onSetUp: noop, onOpen: noop, onRemove: noop, onRetry: noop };
  const found = html(FoundProjectsView, { ...props, found: ok([folder("found", { name: "garden" }), folder("onboarding", { name: "shop", project_id: "p1" }), folder("missing", { name: "old", project_id: "p2", git: false })]) });
  assert.ok(found.includes(`${svg("git-branch", 14)}<strong>garden</strong>`), found);
  assert.ok(found.includes(`${svg("folder", 14)}<strong>old</strong>`), found);
  assert.ok(found.includes(`${svg("list-checks", 15)}Set up this project</button>`), found);
  assert.ok(found.includes(`${svg("arrow-right", 15)}Continue setup</button>`), found);
  assert.ok(found.includes(`${svg("trash", 15)}Remove project</button>`), found);
  assert.ok(found.includes(svg("search", 12)), "Found carries its status icon");
  assert.ok(found.includes(svg("list-checks", 12)), "Setting up carries its status icon");
  assert.ok(found.includes(svg("circle-x", 12)), "Folder missing carries its status icon");

  const base = { home: ok({ path: "/Users/mira/Projects", watching: true }), busy: false, problem: "", onChoose: noop, onMode: noop, onAdd: noop, onClone: noop };
  const move = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, github: "mira/plant-shop" }), mode: "move" });
  assert.ok(move.includes(`${svg("folder-input", 15)}Move into projects folder</button>`), move);
  assert.ok(move.includes(`${svg("git-branch", 15)}Clone instead</button>`), move);
  const copy = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10 }), mode: "copy" });
  assert.ok(copy.includes(`${svg("copy", 15)}Copy into projects folder</button>`), copy);

  const repos = [{ full_name: "mira/plant-shop", private: true, default_branch: "main", size_kb: 2048 }, { full_name: "mira/notes", private: false, default_branch: "main", size_kb: 12 }];
  const list = html(RepoListView, { status: { signed_in: true, login: "mira", method: "device", waiting: false, problem: null }, query: "", onQuery: noop, more: false, onMore: noop, cloning: null, onClone: noop, repos: ok(repos) });
  assert.ok(list.includes(`${svg("lock", 14)}<strong class="pj-mono">mira/plant-shop</strong>`), list);
  assert.ok(list.includes(`${svg("globe", 14)}<strong class="pj-mono">mira/notes</strong>`), list);
  assert.ok(list.includes(`${svg("git-branch", 15)}Clone</button>`), list);

  const value = { dependencies: [dep("tmux", { state: "found", version: "3.5a" })], pack, running: false };
  const tools = html(ToolsSetupView, { status: ok(value), ticks: intake.packTicks(pack), onTick: noop, busy: false, started: false, problem: "", onRun: noop, onContinue: noop });
  assert.ok(tools.includes(`${svg("wrench", 14)}<strong>tmux</strong>`), tools);
  assert.ok(tools.includes(`${svg("wrench", 15)}Set up tools</button>`), tools);
  assert.ok(tools.includes(`${svg("blocks", 14)}superpowers`), tools);
  assert.ok(tools.includes(`${svg("server", 14)}context-mode`), tools);
  const started = html(ToolsSetupView, { status: ok(value), ticks: intake.packTicks(pack), onTick: noop, busy: false, started: true, problem: "", onRun: noop, onContinue: noop });
  assert.ok(started.includes(`${svg("arrow-right", 15)}Continue</button>`), started);
  assert.equal(intake.entryIcon(pack[1]), "blocks");
  assert.equal(intake.entryIcon(pack[2]), "server");
});
