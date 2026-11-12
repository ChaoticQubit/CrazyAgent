import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const intake = await import("../src/screens/intake/intake.ts");
const router = await import("../src/ui/router.ts");
const { HomeFolderView } = await import("../src/screens/intake/home-folder.tsx");
const { FoundProjectsView, SetUpDialogBody } = await import("../src/screens/intake/found.tsx");
const { AddFolderView, AnywayDialog, ICloudDialog, MoveConfirmDialog } = await import("../src/screens/intake/add-folder.tsx");
const intakeApi = await import("../src/api-intake.ts");
const { GitHubSignInView, RepoListView } = await import("../src/screens/intake/clone.tsx");
const { OnboardingReviewView } = await import("../src/screens/intake/onboarding.tsx");
const fields = await import("../src/screens/new-project/fields.tsx");
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
  missing: [], name: "Plant shop", overview: "# Plant shop\n\nBuild with `cargo build`.", folder_overview: null, finished: false, ...extra,
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

test("a picked Windows folder is accepted, checked against a Windows projects folder and named like one", () => {
  const home = "C:\\Users\\mira\\Projects";
  assert.equal(intake.pickedFolderProblem("C:\\Users\\mira\\Code\\shop", home), null);
  assert.equal(intake.pickedFolderProblem("C:\\Users\\mira\\Code\\shop\\", home), null);
  assert.equal(intake.pickedFolderProblem("D:/work/shop", home), null);
  assert.equal(intake.pickedFolderProblem("\\\\nas\\share\\shop", home), null);
  assert.equal(intake.pickedFolderProblem("C:\\Users\\mira\\Projects\\shop", home), "This folder is already in your projects folder. It shows on Home.");
  assert.equal(intake.pickedFolderProblem("C:\\Users\\mira", home), "This folder holds your projects folder. Choose one project's folder.");
  assert.equal(intake.pickedFolderProblem("C:\\Users\\mira\\Projects-old\\shop", home), null);
  assert.equal(intake.pickedFolderProblem("C:\\", home), "Choose the folder again with Choose folder.");
  assert.equal(intake.pickedFolderProblem("Code\\shop", home), "Choose the folder again with Choose folder.");
  assert.equal(intake.pickedFolderProblem("C:Code\\shop", home), "Choose the folder again with Choose folder.");
  assert.equal(intake.folderName("C:\\Users\\mira\\Code\\shop\\"), "shop");
  assert.equal(intake.joinPath(home, "shop"), "C:\\Users\\mira\\Projects\\shop");
  assert.equal(intake.joinPath("/Users/mira/Projects/", "shop"), "/Users/mira/Projects/shop");
});

test("using a folder where it is is the default and Copy shows the size first, with a warning above 1 GB", () => {
  const home = "/Users/mira/Projects";
  assert.deepEqual(intake.sizeLine("here", null, home), { text: "Sets it up where it is. Nothing is moved or copied.", warn: false });
  assert.deepEqual(intake.sizeLine("move", null, home), { text: "Moves the folder into ~/Projects. It won't stay at its old place.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", null, home), { text: "Checking the folder size...", warn: false });
  assert.deepEqual(intake.sizeLine("copy", 512 * 1024 * 1024, home), { text: "Copies 512 MB into ~/Projects. The original stays where it is.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", GB, home), { text: "Copies 1 GB into ~/Projects. The original stays where it is.", warn: false });
  assert.deepEqual(intake.sizeLine("copy", 2.5 * GB, home),
    { text: "This copies 2.5 GB into ~/Projects. That takes a while and uses as much disk again. Using it where it is needs no copy.", warn: true });
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

test("the device sign-in polls only while its code works and the owner isn't signed in yet", () => {
  const device = { code: { expires_s: 900 }, started: 1_000 };
  assert.equal(intake.devicePolling(null, false, 1_000), false, "no code, nothing to poll");
  assert.equal(intake.devicePolling(device, false, 1_000), true);
  assert.equal(intake.devicePolling(device, false, 1_000 + 899_999), true);
  assert.equal(intake.devicePolling(device, false, 1_000 + 900_000), false, "polling stops at the code's deadline");
  assert.equal(intake.devicePolling(device, false, 1_000 + 3_600_000), false);
  assert.equal(intake.devicePolling(device, true, 1_000), false, "a signed-in owner is not polled for");
  const clone = readFileSync(new URL("../src/screens/intake/clone.tsx", import.meta.url), "utf8");
  assert.match(clone, /const polling = devicePolling\(device, status\.signed_in, now\);\s*useEffect\(\(\) => \{\s*if \(!polling\) return;/,
    "the status poll runs only while devicePolling says so");
  assert.match(clone, /if \(devicePolling\(device, false, at\)\) reloadStatus\(\);/, "the tick at the deadline does not ask again");
  assert.match(clone, /\}, \[device, polling\]\);/, "the poll stops when the code expires or the owner signs in");
});

test("each folder chooser names what the folder is for, and adding a project never asks for a skill folder", () => {
  const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
  assert.match(source("../src/api.ts"), /export function pickFolder\(purpose: FolderPurpose\)[^{]*\{\s*return call<string \| null>\("desktop_pick_folder", \{ purpose \}\);/);
  for (const [file, expected] of [
    ["../src/screens/intake/add-folder.tsx", ['"project"']],
    ["../src/screens/intake/home-folder.tsx", ['"projects_home"']],
    ["../src/screens/settings/projects-home.tsx", ['"projects_home"']],
    ["../src/screens/settings/memory.tsx", ['"memory"']],
    ["../src/screens/settings/plugins.tsx", ['"skill"', '"skill"', '"plugin"']],
  ]) {
    const calls = [...source(file).matchAll(/\bpickFolder\(([^)]*)\)/g)].map((call) => call[1]);
    assert.deepEqual(calls, expected, file);
  }
  const lib = source("../src-tauri/src/lib.rs");
  assert.match(lib, /async fn desktop_pick_folder\(\s*webview: Webview,\s*purpose: FolderPurpose,\s*\)/);
  assert.match(lib, /bridge::picker::choose\(purpose\.script\(\)\)/);
  const picker = source("../src-tauri/src/bridge/picker.rs");
  assert.match(picker, /#\[serde\(rename_all = "snake_case"\)\]\s*pub enum FolderPurpose \{\s*Skill,\s*Project,\s*ProjectsHome,\s*Memory,\s*Plugin,\s*\}/,
    "the web view's purpose names are the engine's");
});

test("Show more reads one page at a time, so a double click can't add the same page twice", async () => {
  const reads = [];
  let finish = noop;
  const guarded = intake.oneAtATime((page) => {
    reads.push(page);
    return new Promise((resolve) => { finish = resolve; });
  });
  const first = guarded(2);
  assert.equal(await guarded(2), null, "a second click while the page loads reads nothing");
  assert.deepEqual(reads, [2]);
  finish("page 2");
  assert.equal(await first, "page 2");
  const second = guarded(3);
  finish("page 3");
  assert.equal(await second, "page 3");
  assert.deepEqual(reads, [2, 3], "the next page reads once the last one arrived");

  let attempts = 0;
  const failing = intake.oneAtATime(async () => { attempts += 1; throw new Error("offline"); });
  await assert.rejects(failing(4), /offline/);
  await assert.rejects(failing(4), /offline/, "a failed read frees the guard");
  assert.equal(attempts, 2);

  const clone = readFileSync(new URL("../src/screens/intake/clone.tsx", import.meta.url), "utf8");
  assert.match(clone, /const \[readMore\] = useState\(\(\) => oneAtATime\(loadGitHubRepos\)\);/, "one guard lives as long as the page");
  assert.match(clone, /setLoadingMore\(true\);\s*const result = await readMore\(next\);\s*if \(result === null\) return;\s*setLoadingMore\(false\);/,
    "an ignored click leaves the running page's pending state alone");
  assert.doesNotMatch(clone, /await loadGitHubRepos\(next\)/, "Show more never reads past the guard");
  assert.match(clone, /loadingMore=\{loadingMore\} onMore=/);
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
  const started = Date.UTC(2026, 10, 4, 22, 0, 0);
  const first = { number: 1, tries: 3, started_ms: started, earlier: null };
  assert.deepEqual(intake.stepView({ kind: "running" }, false, first, started + 20000), { word: "Working", tone: "info", line: "Mapping, try 1 of 3, under 1 min." });
  const second = { number: 2, tries: 3, started_ms: started, earlier: "Mapping agent ran out of time" };
  assert.deepEqual(intake.stepView({ kind: "running" }, false, second, started + 150000), { word: "Working", tone: "info", line: "Mapping, try 2 of 3, 2 min. Try 1 failed: Mapping agent ran out of time" });
  assert.equal(intake.stepView({ kind: "running" }, false, first, started - 5000).line, "Mapping, try 1 of 3, under 1 min.");
  assert.deepEqual(intake.stepView({ kind: "done" }, false, second, started), { word: "Done", tone: "ok", line: null });
  assert.deepEqual(intake.stepView({ kind: "done" }), { word: "Done", tone: "ok", line: null });
  assert.deepEqual(intake.stepView({ kind: "failed", reason: "graphify isn't installed." }), { word: "Failed", tone: "danger", line: "graphify isn't installed." });
  assert.deepEqual(intake.stepView({ kind: "off" }), { word: "Off", tone: "neutral", line: "Graphify is off in Settings, Pack, so no code graph is built." });
  assert.deepEqual(intake.stepView({ kind: "not_installed" }), { word: "Not installed", tone: "neutral", line: "Graphify isn't installed, so no code graph is built. Install it in Settings, Pack." });
});

test("a mapping paused on every pool account shows when the pause ends in local time", () => {
  process.env.TZ = "America/New_York";
  const paused = { kind: "waiting", until_ms: 1_792_729_920_000 };
  assert.deepEqual(intake.stepView(paused), { word: "Rate limited until 12:32 AM", tone: "warn", line: null });
  assert.deepEqual(intake.stepView(paused, true), { word: "Skipped", tone: "neutral", line: null });
  assert.deepEqual(intake.stepView({ kind: "waiting" }), { word: "Waiting", tone: "neutral", line: null });
});

test("a finished onboarding opens its saved setup, shows unstarted steps as skipped and saves changes", () => {
  const finished = view({ finished: true, graph: { kind: "running" }, mapping: { kind: "waiting" } });
  assert.equal(intake.settled(finished), true);
  assert.deepEqual(intake.confirmReady(finished), { ready: true, line: "This project is set up. Change anything below, then save." });
  assert.deepEqual(intake.stepView({ kind: "waiting" }, true), { word: "Skipped", tone: "neutral", line: null });
  assert.deepEqual(intake.stepView({ kind: "failed", reason: "x" }, true), { word: "Failed", tone: "danger", line: "x" });
});

test("only a failed mapping of an unfinished setup offers Retry, from the project's own folder", () => {
  assert.equal(intake.mappingRetry(view({ mapping: { kind: "failed", reason: "x" } })), true);
  assert.equal(intake.mappingRetry(view({ mapping: { kind: "failed", reason: "x" }, finished: true })), false);
  assert.equal(intake.mappingRetry(view({ mapping: { kind: "running" } })), false);
  assert.equal(intake.projectFolder(stored({ folder: "/Users/mira/Projects/shop/.crazyagent" })), "/Users/mira/Projects/shop");
  assert.equal(intake.projectFolder(stored({ folder: "C:\\Projects\\shop\\.crazyagent" })), "C:\\Projects\\shop");
  assert.equal(intake.projectFolder(stored({ folder: "/Users/mira/Projects/shop" })), "/Users/mira/Projects/shop");
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

test("a failed graph alone does not make the form ready while the mapping waits", () => {
  const waiting = view({ graph: { kind: "failed", reason: "Graph build failed" }, mapping: { kind: "waiting" } });
  assert.equal(intake.confirmReady(waiting).ready, false);
  assert.equal(intake.settled(waiting), false);
  const running = view({ graph: { kind: "failed", reason: "Graph build failed" }, mapping: { kind: "running" } });
  assert.equal(intake.confirmReady(running).ready, false);
  assert.equal(intake.settled(running), false);
});

test("a saved empty setup with a finished mapping offers the mapping's answers", () => {
  const saved = view({ finished: true, mapping: { kind: "done" }, proposed: setup({ about: "A research reader.", tech_stack: ["Rust"] }) });
  assert.equal(intake.offerProposed(saved), true);
  assert.equal(intake.offerProposed(view({ finished: true })), false);
  assert.equal(intake.offerProposed(view({ finished: false, proposed: setup({ about: "A research reader." }) })), false);
  assert.equal(intake.offerProposed(view({ finished: true, proposed: setup({ about: "  ", tech_stack: [] }) })), false);
  const before = intake.onboardingDraft(saved, stored());
  const used = intake.withProposed(before, saved);
  assert.equal(used.about, "A research reader.");
  assert.deepEqual(used.stack, ["Rust"]);
  assert.equal(intake.withProposed(before, view()), before);
});

test("using the mapping's answers keeps the owner's unsaved name, research and pool", () => {
  const saved = view({ finished: true, mapping: { kind: "done" }, proposed: setup({ about: "A research reader.", tech_stack: ["Rust"], platforms: ["website"] }) });
  const edited = { ...intake.onboardingDraft(saved, stored()), name: "ScholarScope", research: ["market_research"], pool: "work", poolChosen: true };
  const used = intake.withProposed(edited, saved);
  assert.equal(used.name, "ScholarScope");
  assert.deepEqual(used.research, ["market_research"]);
  assert.equal(used.pool, "work");
  assert.equal(used.poolChosen, true);
  assert.equal(used.about, "A research reader.");
  assert.deepEqual(used.stack, ["Rust"]);
  assert.deepEqual(used.platforms, ["website"]);
});

test("confirm waits for the mapping agent unless a step failed", () => {
  assert.deepEqual(intake.confirmReady(view()), { ready: true, line: "Check the answers, fill in anything missing, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ mapping: { kind: "failed", reason: "x" } })), { ready: true, line: "The mapping agent stopped. Fill in the form yourself, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "failed", reason: "x" }, mapping: { kind: "waiting" } })),
    { ready: false, line: "The code graph couldn't be built. The mapping agent still reads the project; the form fills in when it finishes." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "failed", reason: "x" } })),
    { ready: true, line: "Check the answers, fill in anything missing, then confirm." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "running" }, mapping: { kind: "waiting" } })),
    { ready: false, line: "Building the code graph. The form fills in when the mapping agent finishes." });
  assert.deepEqual(intake.confirmReady(view({ mapping: { kind: "running" } })),
    { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." });
  assert.equal(intake.settled(view()), true);
  assert.equal(intake.settled(view({ mapping: { kind: "running" } })), false);
  assert.equal(intake.settled(view({ graph: { kind: "failed", reason: "x" }, mapping: { kind: "waiting" } })), false);
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "off" }, mapping: { kind: "waiting" } })),
    { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "off" } })), { ready: true, line: "Check the answers, fill in anything missing, then confirm." });
  assert.equal(intake.settled(view({ graph: { kind: "off" }, mapping: { kind: "waiting" } })), false);
  assert.equal(intake.settled(view({ graph: { kind: "off" } })), true);
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "not_installed" }, mapping: { kind: "waiting" } })),
    { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." });
  assert.deepEqual(intake.confirmReady(view({ graph: { kind: "not_installed" } })), { ready: true, line: "Check the answers, fill in anything missing, then confirm." });
  assert.equal(intake.settled(view({ graph: { kind: "not_installed" }, mapping: { kind: "waiting" } })), false);
  assert.equal(intake.settled(view({ graph: { kind: "not_installed" } })), true);
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
  assert.deepEqual(intake.dependencyView(dep("tmux", { state: "checking" })),
    { title: "tmux", about: "Runs each agent in a live terminal.", word: "Checking", tone: "info", line: null });
  assert.deepEqual(intake.dependencyView(dep("tmux", { state: "missing" })).line, "Setup installs it.");
  assert.equal(intake.dependencyView(dep("uv", { state: "missing" }, "uv (Python tools)")).title, "uv (Python tools)");
  assert.equal(intake.dependencyView(dep("uv", { state: "missing" }, "  ")).title, "uv");
});

const entry = (id, kind, group, on_by_default, state, extra = {}) => ({ id, kind, group, source: `github.com/example/${id}`, license: "MIT", on_by_default, state, ...extra });
const pack = [
  entry("graphify", "skill", "graph", true, { state: "available" }),
  entry("superpowers", "skill", "workflow", true, { state: "available" }),
  entry("context-mode", "mcp_server", "context", true, { state: "available" }),
  entry("headroom", "mcp_server", "context", true, { state: "on" }, { license: "Apache-2.0" }),
  entry("serena", "mcp_server", "optional", false, { state: "available" }),
];

test("the pack starts with the on-by-default entries that are not installed yet ticked and names every ticked MCP server before approval", () => {
  assert.deepEqual(intake.packTicks(pack), ["graphify", "superpowers", "context-mode"]);
  assert.deepEqual(intake.packTicks([entry("graphify", "skill", "graph", true, { state: "off" }), entry("x", "skill", "workflow", true, { state: "failed", reason: "no" })]), ["x"]);
  assert.equal(intake.approvalLine(pack, ["superpowers", "context-mode", "headroom"]),
    "Continuing lets your agents run these MCP servers: context-mode, headroom. You can turn any of them off later.");
  assert.equal(intake.approvalLine(pack, ["superpowers"]), null);
  assert.equal(intake.setupBusy({ dependencies: [dep("uv", { state: "installing" })], pack: [], running: false }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack: [entry("serena", "mcp_server", "optional", false, { state: "installing" })], running: false }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack, running: true }), true);
  assert.equal(intake.setupBusy({ dependencies: [], pack, running: false }), false);
  assert.equal(intake.setupBusy({ dependencies: [dep("uv", { state: "checking" })], pack, running: false }), false);
  assert.equal(intake.toolsChecking({ dependencies: [dep("uv", { state: "checking" })], pack, running: false }), true);
  assert.equal(intake.toolsChecking({ dependencies: [dep("uv", { state: "missing" })], pack, running: false }), false);
});

test("pack states read in plain words and only on or off entries count as installed", () => {
  assert.deepEqual(intake.packStateView({ state: "available" }), { word: "Not installed", tone: "neutral", line: null });
  assert.deepEqual(intake.packStateView({ state: "installing" }), { word: "Installing", tone: "info", line: null });
  assert.deepEqual(intake.packStateView({ state: "on" }), { word: "On", tone: "ok", line: null });
  assert.deepEqual(intake.packStateView({ state: "off" }), { word: "Off", tone: "neutral", line: null });
  assert.deepEqual(intake.packStateView({ state: "failed", reason: "Checksum didn't match." }), { word: "Failed", tone: "danger", line: "Checksum didn't match." });
  const refused = "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.";
  assert.deepEqual(intake.packStateView({ state: "on" }, refused), { word: "On", tone: "warn", line: `Hooks not added: ${refused}` });
  assert.deepEqual(intake.packStateView({ state: "off" }, refused), { word: "Off", tone: "warn", line: `Hooks not added: ${refused}` });
  assert.deepEqual(intake.packStateView({ state: "on" }, null), { word: "On", tone: "ok", line: null });
  assert.deepEqual(pack.filter(intake.installed).map((item) => item.id), ["headroom"]);
  assert.equal(intake.installed(entry("x", "skill", "workflow", true, { state: "off" })), true);
  assert.equal(intake.installed(entry("x", "skill", "workflow", true, { state: "failed", reason: "no" })), false);
});

test("the pack groups follow the manifest order and keep pack order inside each group", () => {
  const groups = intake.packGroups([...pack, entry("caveman", "skill", "reply_style", true, { state: "available" }), entry("odd", "skill", "later", false, { state: "available" })]);
  assert.deepEqual(groups.map((group) => [group.title, group.entries.map((item) => item.id)]), [
    ["Code graph", ["graphify"]], ["Workflow skills", ["superpowers"]], ["Reply style", ["caveman"]],
    ["Context plugins", ["context-mode", "headroom"]], ["Optional", ["serena"]], ["Other", ["odd"]],
  ]);
  assert.deepEqual(intake.packGroups([]), []);
});

test("setup progress counts every tool and only the ticked pack, so an unticked graphify entry is not waited for", () => {
  const status = {
    dependencies: [dep("tmux", { state: "found", version: "3.5a" }), dep("uv", { state: "installing" }), dep("graphify", { state: "failed", reason: "no" })],
    pack: [entry("graphify", "skill", "graph", true, { state: "available" }), entry("superpowers", "skill", "workflow", true, { state: "installing" }),
      entry("serena", "mcp_server", "optional", false, { state: "available" })],
    running: true,
  };
  assert.deepEqual(intake.setupProgress(status, ["superpowers"]), { done: 2, total: 4, failed: 1 });
  assert.deepEqual(intake.setupProgress(status, []), { done: 2, total: 3, failed: 1 });
});

test("the graphify command and the graphify pack entry have names that can't be confused", () => {
  assert.equal(intake.entryName(entry("graphify", "skill", "graph", true, { state: "available" })), "Graphify skill for agents");
  assert.equal(intake.entryName(entry("superpowers", "skill", "workflow", true, { state: "available" })), "superpowers");
  assert.equal(intake.dependencyView(dep("graphify", { state: "found", version: "0.9.9" }, "Graphify command")).title, "Graphify command");
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

test("adding an existing folder uses it where it is by default, shows the target before a Move or Copy, the Copy size warning and the clone suggestion", () => {
  const base = { home: ok({ path: "/Users/mira/Projects", watching: true }), busy: false, problem: "", onChoose: noop, onMode: noop, onAdd: noop, onClone: noop };
  const none = html(AddFolderView, { ...base, source: null, size: null, mode: "here" });
  assert.match(none, /<h1>Add an existing folder<\/h1>/);
  assert.match(none, />Choose folder<\/button>/);
  assert.doesNotMatch(none, /Set up here|Move into projects folder/);
  const here = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, movable: true }), mode: "here" });
  assert.match(here, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Use where it is</);
  assert.match(here, /role="radio" aria-checked="false"[^>]*>(<svg[\s\S]*?<\/svg>)?Move</);
  assert.match(here, /~\/Code\/shop<\/span> stays where it is\./);
  assert.match(here, /Nothing is moved or copied\./);
  assert.match(here, />Set up here<\/button>/);
  assert.doesNotMatch(here, /goes to|into projects folder/);
  const move = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, movable: true }), mode: "move" });
  assert.match(move, /role="radio" aria-checked="true"[^>]*>(<svg[\s\S]*?<\/svg>)?Move</);
  assert.match(move, /goes to <span class="pj-mono">~\/Projects\/shop<\/span>/);
  assert.match(move, /Moves the folder into ~\/Projects\./);
  assert.match(move, />Move into projects folder<\/button>/);
  const big = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 3 * GB, copyable: true }), mode: "copy" });
  assert.match(big, /This copies 3 GB/);
  assert.match(big, /class="notice"/);
  assert.match(big, />Copy into projects folder<\/button>/);
  assert.doesNotMatch(big, /linked git worktree/);
  const linked = (worktrees, mode) => html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, worktrees, copyable: true }), mode });
  assert.match(linked(3, "copy"), /This folder has 3 linked git worktrees\. They stay with the original folder; the copy gets the main checkout and every branch\./);
  assert.match(linked(1, "copy"), /This folder has 1 linked git worktree\. It stays with the original folder; the copy gets the main checkout and every branch\./);
  assert.doesNotMatch(linked(0, "copy"), /linked git worktree/);
  assert.doesNotMatch(linked(3, "here"), /linked git worktree/);
  const hosted = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, github: "mira/plant-shop" }), mode: "move" });
  assert.match(hosted, /mira\/plant-shop/);
  assert.match(hosted, />Clone instead<\/button>/);
  assert.match(html(AddFolderView, { ...base, home: ok({ path: null, watching: false }), source: null, size: null, mode: "move" }), /Choose your projects folder first/);
  assert.match(html(AddFolderView, { ...base, home: notReady, source: null, size: null, mode: "move" }), /Not ready yet/);
});

test("Move and Copy are offered for every folder once it is read, and only checking the folder holds them back", () => {
  const base = { home: ok({ path: "/Users/mira/Projects", watching: true }), busy: false, problem: "", onChoose: noop, onMode: noop, onAdd: noop, onClone: noop };
  const radio = (label) => new RegExp(`role="radio"[^>]*>(<svg[\\s\\S]*?<\\/svg>)?${label}<`);
  const read = (value) => ok({ bytes: 10, worktrees: 0, worktree_paths: [], movable: true, copyable: true, git: true, same_volume: true, ...value });
  for (const size of [
    read({ movable: false, worktrees: 1 }), read({ movable: false, copyable: false }), read({ git: false }),
    read({ bytes: null, size_problem: "folder_size_timeout" }), read({ bytes: null, size_problem: "folder_size_too_large" }),
    read({ bytes: null, size_problem: "folder_size_unreadable" }), failed("folder_size_failed"), notReady, ok({ bytes: 10 }),
  ]) {
    for (const mode of ["move", "copy"]) {
      const markup = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size, mode });
      const label = `${JSON.stringify(size)} ${mode}`;
      assert.match(markup, radio("Move"), label);
      assert.match(markup, radio("Copy"), label);
      assert.match(markup, new RegExp(`role="radio" aria-checked="true"[^>]*>(<svg[\\s\\S]*?<\\/svg>)?${mode === "move" ? "Move" : "Copy"}<`), label);
      assert.match(markup, new RegExp(`>${mode === "move" ? "Move" : "Copy"} into projects folder</button>`), label);
      assert.doesNotMatch(markup, /isn(&#x27;|')t offered/, label);
      assert.doesNotMatch(markup, /<button[^>]*disabled=""[^>]*>[\s\S]{0,400}into projects folder/, label);
    }
  }
  const measuring = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: { kind: "loading" }, mode: "copy" });
  assert.doesNotMatch(measuring, radio("Copy"));
  assert.match(measuring, /Move and Copy are offered once CrazyAgent has checked this folder(&#x27;|')s git links\./);
  const unmeasured = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: read({ bytes: null, size_problem: "folder_size_timeout" }), mode: "copy" });
  assert.match(unmeasured, /CrazyAgent couldn(&#x27;|')t measure this folder\./);
  assert.doesNotMatch(unmeasured, /Checking the folder size/);
});

test("each issue a folder read or an engine refusal shows names exactly one question issue for that way", () => {
  const read = (value) => ok({ bytes: 10, worktrees: 0, worktree_paths: [], movable: true, copyable: true, git: true, same_volume: true, ...value });
  const issues = (way, size, refused = null) => intake.anywayIssues(way, size, refused);
  assert.deepEqual(issues("move", read({})), []);
  assert.deepEqual(issues("copy", read({})), []);
  assert.deepEqual(issues("move", null), []);
  assert.deepEqual(issues("move", { kind: "loading" }), []);
  assert.deepEqual(issues("move", read({ movable: false })), ["worktrees"]);
  assert.deepEqual(issues("copy", read({ movable: false })), []);
  assert.deepEqual(issues("copy", read({ copyable: false })), ["linked_checkout"]);
  assert.deepEqual(issues("move", read({ copyable: false })), []);
  assert.deepEqual(issues("move", read({ git: false })), ["no_git"]);
  assert.deepEqual(issues("copy", read({ git: false })), ["no_git"]);
  assert.deepEqual(issues("move", read({ bytes: null, size_problem: "folder_size_unreadable" })), ["unreadable"]);
  assert.deepEqual(issues("copy", read({ bytes: null, size_problem: "folder_size_timeout" })), ["large"]);
  assert.deepEqual(issues("move", read({ bytes: null, size_problem: "folder_size_timeout", same_volume: false })), ["large"]);
  assert.deepEqual(issues("move", read({ bytes: null, size_problem: "folder_size_timeout", same_volume: true })), []);
  assert.deepEqual(issues("copy", read({ bytes: null, size_problem: "folder_size_too_large" })), ["large", "too_many"]);
  assert.deepEqual(issues("move", read({ bytes: null, size_problem: "folder_size_too_large", same_volume: null })), ["large", "too_many"]);
  assert.deepEqual(issues("copy", read({ bytes: 3 * GB })), ["large"]);
  assert.deepEqual(issues("move", read({ bytes: 3 * GB, same_volume: false })), ["large"]);
  assert.deepEqual(issues("move", read({ bytes: 3 * GB, same_volume: true })), []);
  assert.deepEqual(issues("copy", read({ bytes: GB })), []);
  for (const size of [failed("folder_size_failed"), notReady, ok({ bytes: 10 })]) {
    assert.deepEqual(issues("move", size), ["git_unread"], size.kind);
    assert.deepEqual(issues("copy", size), ["git_unread"], size.kind);
  }
  assert.deepEqual(issues("move", read({}), "home_move_worktrees"), ["worktrees"]);
  for (const [link, issue] of [["worktrees", "worktrees"], ["linked_worktree", "linked_worktree"], ["submodule", "submodule"], ["unreadable", "git_data_unread"]]) {
    assert.deepEqual(issues("move", read({ movable: false, git_link: link })), [issue], link);
    assert.deepEqual(issues("move", read({ movable: false, git_link: link }), "home_move_worktrees"), [issue], `${link} refused`);
    assert.deepEqual(issues("copy", read({ movable: false, git_link: link })), [], `${link} copy`);
  }
  assert.deepEqual(issues("copy", read({}), "home_copy_linked_checkout"), ["linked_checkout"]);
  assert.deepEqual(issues("move", read({}), "home_copy_too_large"), ["too_many"]);
  assert.deepEqual(issues("copy", read({}), "home_copy_too_large"), ["too_many"]);
  assert.deepEqual(issues("move", read({}), "home_destination_exists"), []);
  assert.deepEqual(issues("move", ok({ bytes: 10 }), "home_move_worktrees"), ["worktrees", "git_unread"]);
  assert.deepEqual(issues("copy", read({ git: false, copyable: false, bytes: null, size_problem: "folder_size_unreadable" })), ["unreadable", "linked_checkout", "no_git"]);
});

test("Move anyway sends an override only for an issue the engine checks and the owner was shown", () => {
  const read = (value) => ok({ bytes: 10, worktrees: 0, worktree_paths: [], movable: true, copyable: true, git: true, same_volume: false, ...value });
  const overrides = (way, size, refused = null) => intake.anywayOverrides(way, intake.anywayIssues(way, size, refused), size);
  assert.deepEqual(overrides("move", read({})), []);
  assert.deepEqual(overrides("move", read({ movable: false })), ["worktrees"]);
  assert.deepEqual(overrides("copy", read({ copyable: false })), ["linked_checkout"]);
  assert.deepEqual(overrides("copy", read({ bytes: null, size_problem: "folder_size_timeout" })), ["too_large"]);
  assert.deepEqual(overrides("copy", read({ bytes: null, size_problem: "folder_size_too_large" })), ["too_large"]);
  assert.deepEqual(overrides("move", read({}), "home_copy_too_large"), ["too_large"]);
  assert.deepEqual(overrides("copy", read({ bytes: 3 * GB })), []);
  assert.deepEqual(overrides("move", read({ git: false })), []);
  assert.deepEqual(overrides("move", read({ bytes: null, size_problem: "folder_size_unreadable" })), []);
  assert.deepEqual(overrides("move", failed("folder_size_failed")), []);
  assert.deepEqual(overrides("move", read({ movable: false, bytes: null, size_problem: "folder_size_too_large" })), ["worktrees", "too_large"]);
  assert.deepEqual(overrides("move", read({}), "home_move_worktrees"), ["worktrees"]);
  for (const link of ["linked_worktree", "submodule", "unreadable"]) assert.deepEqual(overrides("move", read({ movable: false, git_link: link })), ["worktrees"], link);
});

test("the Move anyway warnings put every data loss case first, each in the owner's exact words", () => {
  const W = intakeApi.ADD_WARNINGS;
  const read = (value) => ok({ bytes: 10, worktrees: 0, worktree_paths: [], movable: true, copyable: true, git: true, same_volume: true, ...value });
  const warnings = (way, size, refused = null) => intake.anywayWarnings(way, intake.anywayIssues(way, size, refused), size);
  const flat = (blocks) => blocks.flatMap((block) => block.lines);
  assert.deepEqual(flat(warnings("move", read({}))), [W.close_apps, W.old_place, W.same_disk]);
  assert.deepEqual(flat(warnings("move", read({ same_volume: false }))), [W.close_apps, W.across_disks, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({ same_volume: null }))), [W.close_apps, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({ git: false }))), [W.close_apps, W.old_place, W.no_git, W.same_disk]);
  assert.deepEqual(flat(warnings("move", read({ bytes: 42 * GB, same_volume: false }))), [W.close_apps, W.large, "It is about 42 GB.", W.across_disks, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({ bytes: null, size_problem: "folder_size_timeout", same_volume: false }))), [W.close_apps, W.large, W.unmeasured, W.across_disks, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({ bytes: null, size_problem: "folder_size_too_large", same_volume: false }))), [W.close_apps, W.large, W.unmeasured, W.across_disks, W.too_many, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({}), "home_copy_too_large")), [W.close_apps, W.across_disks, W.too_many, W.old_place]);
  assert.deepEqual(flat(warnings("move", read({ bytes: null, size_problem: "folder_size_unreadable" }))), [W.close_apps, W.unreadable, W.old_place, W.same_disk]);
  assert.deepEqual(flat(warnings("move", ok({ bytes: 10 }))), [W.close_apps, W.old_place, W.git_unread]);
  const linked = warnings("move", read({ movable: false, worktrees: 2, worktree_paths: ["/Users/mira/Code/shop-a", "/Users/mira/Code/shop-b"] }));
  assert.deepEqual(flat(linked), [W.close_apps, W.old_place, W.worktrees, W.same_disk]);
  assert.deepEqual(linked.find((block) => block.lines.includes(W.worktrees)).paths, ["/Users/mira/Code/shop-a", "/Users/mira/Code/shop-b"]);
  assert.deepEqual(flat(warnings("copy", read({ copyable: false, git: false }))), [W.linked_checkout, W.no_git]);
  const worktree = warnings("move", read({ movable: false, git_link: "linked_worktree" }));
  assert.deepEqual(worktree[0], { loss: true, lines: [W.linked_worktree], paths: [] }, "a moved linked worktree can lose its staged changes and branch link, so it leads the loss blocks");
  assert.deepEqual(flat(worktree), [W.linked_worktree, W.close_apps, W.old_place, W.same_disk]);
  assert.deepEqual(flat(warnings("move", read({ movable: false, git_link: "submodule" }))), [W.close_apps, W.old_place, W.submodule, W.same_disk]);
  assert.deepEqual(flat(warnings("move", read({ movable: false, git_link: "unreadable" }))), [W.close_apps, W.old_place, W.git_data_unread, W.same_disk]);
  const shared = warnings("copy", read({ copyable: false, git: false, bytes: 42 * GB }));
  assert.deepEqual(shared[0], { loss: true, lines: [W.linked_checkout], paths: [] }, "a shared checkout is a data loss line, shown first");
  assert.deepEqual(flat(shared), [W.linked_checkout, W.large, "It is about 42 GB.", W.copy_all, W.no_git]);
  assert.deepEqual(flat(warnings("copy", read({ bytes: 42 * GB }))), [W.large, "It is about 42 GB.", W.copy_all]);
  assert.deepEqual(flat(warnings("copy", read({ bytes: null, size_problem: "folder_size_unreadable" }))), [W.unreadable_copy]);
  for (const [way, size, refused] of [
    ["move", read({ movable: false, git: false, bytes: null, size_problem: "folder_size_too_large", same_volume: false }), null],
    ["move", read({ bytes: null, size_problem: "folder_size_unreadable", same_volume: false }), null],
    ["copy", read({ copyable: false, bytes: 42 * GB }), null],
  ]) {
    const blocks = warnings(way, size, refused);
    const firstEffect = blocks.findIndex((block) => !block.loss);
    assert.ok(blocks.slice(firstEffect === -1 ? blocks.length : firstEffect).every((block) => !block.loss), `${way} loss blocks come first`);
  }
});

test("the Move anyway question lists each warning, starts on Use where it is and makes Move anyway the danger button", () => {
  const W = intakeApi.ADD_WARNINGS;
  const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/'/g, "(&#x27;|')");
  const read = { kind: "ok", value: { bytes: null, size_problem: "folder_size_too_large", worktrees: 1, worktree_paths: ["/Users/mira/Code/shop-side"], movable: false, copyable: true, git: false, same_volume: false } };
  const props = (way, extra = {}) => ({ open: true, way, name: "shop", size: read, refused: null, busy: false, onUseHere: noop, onAnyway: noop, ...extra });
  const move = html(AnywayDialog, props("move"));
  assert.match(move, /<h2[^>]*>Move shop anyway\?<\/h2>/);
  for (const line of [W.close_apps, W.old_place, W.across_disks, W.large, W.unmeasured, W.too_many, W.worktrees, W.no_git]) assert.match(move, new RegExp(escape(line)), line);
  assert.match(move, /\/Users\/mira\/Code\/shop-side/);
  const loss = move.indexOf("Close editors, terminals and apps");
  assert.ok(loss >= 0 && loss < move.indexOf("Git worktrees linked"), "the data loss line comes before the git effects");
  assert.match(move, /<button[^>]*data-autofocus[^>]*>(<svg[\s\S]*?<\/svg>)?Use where it is<\/button>/);
  assert.match(move, /<button[^>]*class="[^"]*danger[^"]*"[^>]*>(<svg[\s\S]*?<\/svg>)?Move anyway<\/button>/);
  assert.ok(move.indexOf(">Use where it is<") < move.indexOf(">Move anyway<"));
  const copy = html(AnywayDialog, props("copy", { size: { kind: "ok", value: { ...read.value, copyable: false, size_problem: null, bytes: 10 } } }));
  assert.match(copy, /<h2[^>]*>Copy shop anyway\?<\/h2>/);
  assert.match(copy, new RegExp(escape(W.linked_checkout)));
  assert.doesNotMatch(copy, new RegExp(escape(W.close_apps)));
  assert.match(copy, /<button[^>]*class="[^"]*danger[^"]*"[^>]*>(<svg[\s\S]*?<\/svg>)?Copy anyway<\/button>/);
  assert.doesNotMatch(html(AnywayDialog, props("move", { open: false })), /anyway\?/);
  const refused = html(AnywayDialog, props("move", { size: { kind: "ok", value: { ...read.value, movable: true, git: true, size_problem: null, bytes: 10, same_volume: true } }, refused: "home_move_worktrees" }));
  assert.match(refused, new RegExp(escape(W.worktrees)));
});

test("the Move confirm only opens while Move is the way, and says what a move can lose", () => {
  assert.equal(typeof MoveConfirmDialog, "function", "add-folder.tsx exports MoveConfirmDialog");
  const W = intakeApi.ADD_WARNINGS;
  const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/'/g, "(&#x27;|')");
  const props = (way, sameVolume = true) => ({ open: true, way, name: "shop", source: "/Users/mira/Code/shop", root: "/Users/mira/Projects", sameVolume, busy: false, onCancel: noop, onConfirm: noop });
  const title = /Move shop into your projects folder\?/;
  assert.match(html(MoveConfirmDialog, props("move")), title);
  assert.match(html(MoveConfirmDialog, props("move")), />Move folder<\/button>/);
  for (const line of [W.close_apps, W.old_place, W.same_disk]) assert.match(html(MoveConfirmDialog, props("move")), new RegExp(escape(line)), line);
  assert.match(html(MoveConfirmDialog, props("move", false)), new RegExp(escape(W.across_disks)));
  assert.doesNotMatch(html(MoveConfirmDialog, props("move", false)), new RegExp(escape(W.same_disk)));
  for (const way of ["here", "copy"]) assert.doesNotMatch(html(MoveConfirmDialog, props(way)), title, way);
  assert.doesNotMatch(html(MoveConfirmDialog, { ...props("move"), open: false }), title);
  assert.doesNotMatch(html(MoveConfirmDialog, { ...props("move"), root: null }), title);
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
  const base = { status: { signed_in: true, login: "mira", method: "device", waiting: false, problem: null }, query: "", onQuery: noop, more: false, loadingMore: false, onMore: noop, cloning: null, onClone: noop };
  const markup = html(RepoListView, { ...base, repos: ok(repos) });
  assert.match(markup, /Signed in to GitHub as mira\./);
  assert.match(markup, /mira\/plant-shop/);
  assert.match(markup, /Private, 2 MB, branch main/);
  assert.match(markup, />Clone<\/button>/);
  assert.match(html(RepoListView, { ...base, repos: ok([]) }), /No repositories on this account\./);
  assert.match(html(RepoListView, { ...base, query: "zzz", repos: ok(repos) }), /No repositories match/);
  assert.match(html(RepoListView, { ...base, repos: ok(repos), more: true }), />Show more<\/button>/);
  const loading = html(RepoListView, { ...base, repos: ok(repos), more: true, loadingMore: true });
  assert.match(loading, /<button[^>]*disabled=""[^>]*aria-busy="true"[^>]*>.*Loading more\.\.\.<\/button>/, "a page on its way blocks another Show more");
  assert.doesNotMatch(loading, />Show more<\/button>/);
  assert.match(html(RepoListView, { ...base, repos: notReady }), /Not ready yet/);
});

test("an overview the engine did not write is shown apart and labelled, never as the mapping agent's", () => {
  const catalog = ok({ tech_stack: [], platforms: [], research: [], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "Plant shop", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop, draft: null };
  const engine = html(OnboardingReviewView, { ...base, view: ok(view()) });
  assert.match(engine, />Overview</);
  assert.doesNotMatch(engine, /Overview found in the folder|did not write this/);
  const found = html(OnboardingReviewView, { ...base, view: ok(view({ overview: null, folder_overview: "# Shipped\n\nFrom the repository." })) });
  assert.match(found, /Overview found in the folder/);
  assert.match(found, /The mapping agent did not write this: it came with the folder or was changed since\./);
  assert.match(found, /From the repository\./);
  assert.doesNotMatch(found, />Overview</);
});

test("a setup saved without the mapping's answers offers them in the review", () => {
  const catalog = ok({ tech_stack: [], platforms: [], research: [], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "ScholarScope", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop, draft: null };
  const proposed = setup({ about: "A research reader.", tech_stack: ["Rust"] });
  const offered = html(OnboardingReviewView, { ...base, view: ok(view({ finished: true, draft: setup(), proposed })), onUseProposed: noop });
  assert.match(offered, /This setup was saved without the mapping&#x27;s description and tech stack\./);
  assert.match(offered, />Use the mapping&#x27;s answers</);
  assert.doesNotMatch(html(OnboardingReviewView, { ...base, view: ok(view({ finished: true })), onUseProposed: noop }), /Use the mapping/);
});

test("the setup review shows each research toggle the repo already covers and the documents found", () => {
  const catalog = ok({ tech_stack: [], platforms: [], research: [{ kind: "market_research", label: "Market research" }, { kind: "competitor_research", label: "Competitor research" }], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "Plant shop", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop };
  const facts = { ...view().facts, research: [{ kind: "competitor_research", files: ["research/landscape-2026.md", "notes/rivals.txt"] }], documents: ["docs/q3-plan.md"] };
  const mapped = view({ facts });
  const markup = html(OnboardingReviewView, { ...base, view: ok(mapped), draft: intake.onboardingDraft(mapped, stored()) });
  const research = markup.slice(markup.indexOf("<legend>Research</legend>"), markup.indexOf("</fieldset>", markup.indexOf("<legend>Research</legend>")));
  const items = research.split('class="pj-switch-item"').slice(1);
  assert.equal(items.length, 2, "one switch per research kind; the roadmap switch sits outside the Research fieldset");
  assert.doesNotMatch(items[0], /Linked|Already in the repo|Use these files/);
  assert.match(items[1], /Linked: research\/landscape-2026\.md, notes\/rivals\.txt\. CrazyAgent reads them from your repo; edits go to those files\./);
  assert.match(items[1], /<input type="checkbox" checked=""\/><span>Use these files<\/span>/);
  assert.match(items[1], /id="ob-research-competitor_research"[^>]*aria-checked="false"[^>]*aria-disabled="true"/);
  assert.match(markup, /Project documents found[\s\S]*<li>docs\/q3-plan\.md<\/li>/);
  const off = html(OnboardingReviewView, { ...base, view: ok(mapped), draft: intake.onboardingDraft(mapped, stored()), dropped: ["competitor_research"] });
  assert.match(off, /Already in the repo: research\/landscape-2026\.md, notes\/rivals\.txt/);
  assert.match(off, /<input type="checkbox"\/><span>Use these files<\/span>/);
  const saved = view({ facts, finished: true });
  const later = html(OnboardingReviewView, { ...base, view: ok(saved), draft: intake.onboardingDraft(saved, stored()) });
  assert.match(later, /Already in the repo: research\/landscape-2026\.md/);
  assert.doesNotMatch(later, /Linked:|Use these files/);
  const plain = html(OnboardingReviewView, { ...base, view: ok(view()), draft: intake.onboardingDraft(view(), stored()) });
  assert.doesNotMatch(plain, /Linked:|Already in the repo|Project documents found|Roadmap found|Use these files/);
});

test("a research kind the repo already holds starts off and names the linked files", () => {
  const markup = html(fields.ResearchSwitches, {
    prefix: "t", catalog: undefined, value: ["competitor_research"], onChange: noop,
    found: { competitor_research: ["docs/research-competitors.md"] },
  });
  assert.match(markup, /Linked: docs\/research-competitors\.md\. CrazyAgent reads them from your repo; edits go to those files\./);
  assert.match(markup, /id="t-research-competitor_research"[^>]*aria-checked="false"[^>]*aria-disabled="true"/);
  assert.doesNotMatch(markup, /Use these files/, "the checkbox comes only with its handler");
  const plain = html(fields.ResearchSwitches, { prefix: "t", catalog: undefined, value: ["competitor_research"], onChange: noop });
  assert.match(plain, /id="t-research-competitor_research"[^>]*aria-checked="true"/);
  assert.doesNotMatch(plain, /Linked|aria-disabled/);
});

test("the setup review links a found roadmap and hides roadmap planning while it is used", () => {
  const catalog = ok({ tech_stack: [], platforms: [], research: [], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "ScholarScope", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop };
  const mapped = view({ facts: { ...view().facts, roadmap: ["research/docs/ss-phase1-revision/report.md"] } });
  const draft = intake.onboardingDraft(mapped, stored());
  const linked = html(OnboardingReviewView, { ...base, view: ok(mapped), draft });
  assert.match(linked, /<legend>Roadmap found<\/legend><p class="pj-help">Linked: research\/docs\/ss-phase1-revision\/report\.md\. CrazyAgent reads them from your repo; edits go to those files\.<\/p>/);
  assert.match(linked, /<input type="checkbox" checked=""\/><span>Use these files<\/span>/);
  assert.doesNotMatch(linked, /id="ob-roadmap"/);
  const off = html(OnboardingReviewView, { ...base, view: ok(mapped), draft, dropped: ["roadmap"] });
  assert.match(off, /In the repo: research\/docs\/ss-phase1-revision\/report\.md/);
  assert.match(off, /id="ob-roadmap"/);
  const saved = html(OnboardingReviewView, { ...base, view: ok({ ...mapped, finished: true }), draft });
  assert.doesNotMatch(saved, /Roadmap found/);
});

test("a roadmap planning group is shown as a found roadmap, hides roadmap planning while used and is kept as its own kind", () => {
  const catalog = ok({ tech_stack: [], platforms: [], research: [], document_extensions: [], documents_max: 16, document_bytes_max: 1, project_document_bytes_max: 1, default_team: stored().team });
  const base = { name: "ScholarScope", catalog, errors: {}, showErrors: false, busy: false, problem: "", onDraft: noop, onConfirm: noop, onOpenLink: noop };
  const planning = { kind: "roadmap_planning", files: ["docs/next.md"] };
  const facts = { ...view().facts, research: [{ kind: "market_research", files: ["docs/market.md"] }, planning] };
  const mapped = view({ facts });
  const repo = intake.inRepo(facts);
  assert.deepEqual(repo.roadmap, ["docs/next.md"]);
  assert.deepEqual(repo.research, { market_research: ["docs/market.md"] });
  const draft = intake.onboardingDraft(mapped, stored());
  const linked = html(OnboardingReviewView, { ...base, view: ok(mapped), draft });
  assert.match(linked, /<legend>Roadmap found<\/legend><p class="pj-help">Linked: docs\/next\.md\. /);
  assert.match(linked, /<input type="checkbox" checked=""\/><span>Use these files<\/span>/);
  assert.doesNotMatch(linked, /id="ob-roadmap"/);
  const off = html(OnboardingReviewView, { ...base, view: ok(mapped), draft, dropped: ["roadmap"] });
  assert.match(off, /In the repo: docs\/next\.md/);
  assert.match(off, /id="ob-roadmap"/);
  const market = { path: "docs/market.md", research: "market_research" };
  const next = { path: "docs/next.md", research: "roadmap_planning" };
  assert.deepEqual(intake.confirmKept(mapped, []), [next, market]);
  assert.deepEqual(intake.confirmKept(mapped, ["roadmap"]), [market], "unticked with the roadmap, the planning file is linked as nothing");
  assert.deepEqual(intake.confirmKept(mapped, ["market_research"]), [next]);
  const both = view({ facts: { ...facts, roadmap: ["docs/next.md", "ROADMAP.md"] } });
  assert.deepEqual(intake.inRepo(both.facts).roadmap, ["docs/next.md", "ROADMAP.md"], "a file found both ways is shown once");
  assert.deepEqual(intake.confirmKept(both, ["market_research"]), [{ path: "docs/next.md", research: null }, { path: "ROADMAP.md", research: null }, next]);
  assert.deepEqual(intake.confirmKept(both, ["roadmap", "market_research"]), []);
});

test("the first confirm keeps the found roadmap and research files the owner left ticked", () => {
  const facts = {
    ...view().facts,
    roadmap: ["research/docs/ss-phase1-revision/report.md"],
    research: [
      { kind: "competitor_research", files: ["docs/rivals.md", "research/docs/ss-phase1-revision/report.md"] },
      { kind: "market_research", files: ["docs/market.md"] },
    ],
  };
  const mapped = view({ facts });
  const roadmap = "research/docs/ss-phase1-revision/report.md";
  const rivals = [{ path: "docs/rivals.md", research: "competitor_research" }, { path: roadmap, research: "competitor_research" }];
  assert.deepEqual(intake.confirmKept(mapped, []), [{ path: roadmap, research: null }, ...rivals, { path: "docs/market.md", research: "market_research" }]);
  assert.deepEqual(intake.confirmKept(mapped, ["roadmap", "market_research"]), rivals, "a file also found as the roadmap goes only with the group left on");
  assert.deepEqual(intake.confirmKept(mapped, ["competitor_research"]), [{ path: roadmap, research: null }, { path: "docs/market.md", research: "market_research" }]);
  assert.deepEqual(intake.confirmKept(mapped, ["roadmap", "competitor_research", "market_research"]), []);
  const twice = view({ facts: { ...facts, roadmap: [roadmap, roadmap] } });
  assert.deepEqual(intake.confirmKept(twice, ["competitor_research", "market_research"]), [{ path: roadmap, research: null }], "a file named twice in one group is kept once");
  assert.equal(intake.confirmKept(view({ facts, finished: true }), []), null, "a saved setup leaves its links alone");
  assert.equal(intake.confirmKept(view(), []), null, "nothing found, nothing to link");
  const many = view({ facts: { ...view().facts, research: [{ kind: "market_research", files: Array.from({ length: 40 }, (_, at) => `docs/${at}.md`) }] } });
  assert.equal(intake.confirmKept(many, []).length, intake.LINKS_MAX);
});

test("a repository CrazyAgent can't clone is listed with why and without a working Clone", () => {
  const repos = [
    { full_name: "octo-cat/.github", private: false, default_branch: "main", size_kb: 4, refused: "github_repo_name_unsupported" },
    { full_name: "octo-cat/notes", private: false, default_branch: "main", size_kb: 4, refused: null },
  ];
  const base = { status: { signed_in: true, login: "octo-cat", method: "token", waiting: false, problem: null }, query: "", onQuery: noop, more: false, loadingMore: false, onMore: noop, cloning: null, onClone: noop };
  const rows = html(RepoListView, { ...base, repos: ok(repos) }).split("<li").slice(1);
  assert.equal(rows.length, 2);
  assert.match(rows[0], /octo-cat\/\.github/);
  assert.match(rows[0], /CrazyAgent can(&#x27;|')t clone a repository whose name starts with a dot/);
  assert.match(rows[0], /<button[^>]*disabled=""[^>]*>.*Clone<\/button>/);
  assert.doesNotMatch(rows[0], /github_repo_name_unsupported/);
  assert.match(rows[1], /Public, 4 KB, branch main/);
  assert.doesNotMatch(rows[1], /disabled=""/);
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
  assert.match(html(OnboardingReviewView, { ...base, view: ok(view({ graph: { kind: "failed", reason: "graphify isn't installed." }, mapping: { kind: "waiting" } })), draft }), /<span class="pj-small pj-error">graphify isn&#x27;t installed\.<\/span>/);
  for (const kind of ["off", "not_installed"]) {
    const markup = html(OnboardingReviewView, { ...base, view: ok(view({ graph: { kind }, mapping: { kind: "running" } })), draft: null });
    assert.match(markup, /<span class="pj-small pj-secondary">Graphify [^<]*Settings, Pack[^<]*<\/span>/, kind);
    assert.doesNotMatch(markup, /pj-error/, kind);
  }
  assert.match(html(OnboardingReviewView, { ...base, view: notReady, draft: null }), /Not ready yet/);
  const stopped = html(OnboardingReviewView, { ...base, view: ok(view({ mapping: { kind: "failed", reason: "Agent stopped: workspace routing discovery failed" } })), draft });
  assert.match(stopped, /Agent stopped: workspace routing discovery failed/);
  assert.match(stopped, /<button[^>]*>Retry mapping<\/button>/);
  assert.match(stopped, /<button[^>]*>Skip mapping<\/button>/);
  assert.doesNotMatch(stopped, /<button[^>]*disabled=""[^>]*>Confirm and finish setup<\/button>/);
  const skipped = html(OnboardingReviewView, { ...base, view: ok(view({ mapping: { kind: "failed", reason: "x" } })), draft, skipped: true });
  assert.doesNotMatch(skipped, /Retry mapping|Skip mapping/);
  const saved = html(OnboardingReviewView, { ...base, view: ok(view({ finished: true, graph: { kind: "failed", reason: "x" }, mapping: { kind: "waiting" } })), draft });
  assert.match(saved, /This project is set up\. Change anything below, then save\./);
  assert.match(saved, /Map the project[\s\S]*Skipped/);
  assert.match(saved, /<button[^>]*>Save changes<\/button>/);
  assert.doesNotMatch(saved, /Retry mapping|Confirm and finish setup/);
});

test("the tools step lists dependencies and the pack by group, and names the MCP servers it approves", () => {
  const value = { dependencies: [dep("tmux", { state: "found", version: "3.5a" }), dep("graphify", { state: "missing" }, "Graphify command")], pack, running: false };
  const base = { status: ok(value), ticks: intake.packTicks(pack), onTick: noop, busy: false, started: false, problem: "", onRun: noop, onContinue: noop };
  const markup = html(ToolsSetupView, base);
  const rows = markup.split('<li class="in-row">');
  const rowWith = (text) => { const found = rows.filter((row) => row.includes(text)); assert.equal(found.length, 1, text); return found[0]; };
  assert.match(markup, /tmux[\s\S]*Version 3\.5a on this Mac\.[\s\S]*Found/);
  assert.match(rowWith("Graphify command"), /Not installed/);
  assert.match(markup, /<legend>Code graph<\/legend>[\s\S]*Graphify skill for agents/);
  assert.doesNotMatch(markup, /always on/i);
  const skill = rowWith("Graphify skill for agents");
  assert.match(skill, /checked=""/);
  assert.doesNotMatch(skill, /disabled=""/);
  const unticked = html(ToolsSetupView, { ...base, ticks: base.ticks.filter((id) => id !== "graphify") }).split('<li class="in-row">').find((row) => row.includes("Graphify skill for agents"));
  assert.doesNotMatch(unticked, /checked=""/);
  const present = rowWith("headroom");
  assert.doesNotMatch(present, /type="checkbox"/);
  assert.match(present, /Already installed/);
  assert.match(markup, /<legend>Workflow skills<\/legend>[\s\S]*superpowers/);
  assert.match(markup, /<legend>Context plugins<\/legend>[\s\S]*context-mode[\s\S]*MCP server/);
  assert.match(markup, /<legend>Optional<\/legend>[\s\S]*serena[\s\S]*off by default/);
  assert.match(markup, /headroom[\s\S]*>On</);
  assert.match(markup, /Continuing lets your agents run these MCP servers: context-mode\./);
  assert.match(markup, />Set up tools<\/button>/);
  assert.match(markup, />Skip for now<\/button>/);
  assert.doesNotMatch(markup, /role="status"/);
  const finished = html(ToolsSetupView, { ...base, started: true });
  assert.match(finished, /Setup finished: 1 of 5 done\./);
  assert.match(finished, />Continue<\/button>/);
  assert.doesNotMatch(finished, /Skip for now/);
  const running = html(ToolsSetupView, { ...base, status: ok({ ...value, running: true }) });
  assert.match(running, /Installing: 1 of 5 done\. You can continue; setup keeps going\./);
  assert.match(running, />Continue<\/button>/);
  assert.match(html(ToolsSetupView, { ...base, status: notReady }), /Not ready yet[\s\S]*>Continue<\/button>/);
});

test("the tools step says why an installed entry's hooks were not added, on that entry only", () => {
  const refused = "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.";
  const listed = pack.map((item) => item.id === "headroom" ? { ...item, hooks_refused: refused } : item);
  const markup = html(ToolsSetupView, { status: ok({ dependencies: [], pack: listed, running: false }), ticks: intake.packTicks(listed), onTick: noop, busy: false, started: false, problem: "", onRun: noop, onContinue: noop });
  const rows = markup.split('<li class="in-row">').filter((row) => row.includes("headroom"));
  assert.equal(rows.length, 1);
  assert.match(rows[0], /Already installed/);
  assert.ok(rows[0].includes(`<span class="pj-small pj-error">Hooks not added: ${refused}</span>`), rows[0]);
  assert.equal(markup.split("Hooks not added").length - 1, 1, "only the refused entry says so");
});

test("a failed setup read keeps the shown setup, says so, backs off, and a newer read clears it", () => {
  const installing = { dependencies: [dep("uv", { state: "installing" })], pack, running: true };
  const shown = intake.foldSetupRead(intake.SETUP_READS, ok(installing));
  assert.deepEqual(shown, { seen: installing, latest: installing, failures: 0, line: "" });
  assert.equal(intake.foldSetupRead(shown, { kind: "loading" }), shown);
  const once = intake.foldSetupRead(shown, failed("setup_owner_unavailable", "CrazyAgent's engine isn't running."));
  assert.notEqual(once, shown);
  assert.deepEqual(once, { seen: installing, latest: installing, failures: 1, line: "Couldn't refresh setup progress: CrazyAgent's engine isn't running." });
  assert.equal(intake.foldSetupRead(once, ok(installing)), once);
  assert.equal(intake.foldSetupRead(once, { kind: "loading" }), once);
  const twice = intake.foldSetupRead(once, failed("setup_owner_unavailable", "CrazyAgent's engine isn't running."));
  assert.notEqual(twice, once);
  assert.equal(twice.failures, 2);
  const thrice = intake.foldSetupRead(twice, notReady);
  assert.equal(thrice.failures, 3);
  assert.equal(thrice.latest, installing);
  assert.equal(thrice.line, "Couldn't refresh setup progress: Tool setup isn't in this build.");
  const done = { dependencies: [dep("uv", { state: "installed" })], pack, running: false };
  assert.deepEqual(intake.foldSetupRead(thrice, ok(done)), { seen: done, latest: done, failures: 0, line: "" });
  const started = { ...shown, latest: { ...installing } };
  assert.equal(intake.foldSetupRead(started, ok(installing)), started);
  assert.deepEqual([0, 1, 2, 3, 4, 5, 9, 4000].map(intake.setupPollMs), [2000, 4000, 8000, 16000, 30000, 30000, 30000, 30000]);
  const base = { status: ok(installing), ticks: intake.packTicks(pack), onTick: noop, busy: false, started: true, problem: "", onRun: noop, onContinue: noop };
  const stale = html(ToolsSetupView, { ...base, stale: once.line });
  assert.match(stale, /Installing: \d+ of \d+ done\.[\s\S]*<p class="pj-error" role="alert">Couldn&#x27;t refresh setup progress: CrazyAgent&#x27;s engine isn&#x27;t running\.<\/p>/);
  assert.doesNotMatch(html(ToolsSetupView, base), /role="alert"/);
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
  const here = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10 }), mode: "here" });
  assert.ok(here.includes(`${svg("list-checks", 15)}Set up here</button>`), here);
  const move = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, github: "mira/plant-shop", movable: true }), mode: "move" });
  assert.ok(move.includes(`${svg("folder-input", 15)}Move into projects folder</button>`), move);
  assert.ok(move.includes(`${svg("git-branch", 15)}Clone instead</button>`), move);
  const copy = html(AddFolderView, { ...base, source: "/Users/mira/Code/shop", size: ok({ bytes: 10, copyable: true }), mode: "copy" });
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

test("a soft engine refusal asks again only when the override for it was not sent", () => {
  assert.equal(intake.asksAgain("home_move_worktrees", []), true);
  assert.equal(intake.asksAgain("home_move_worktrees", ["worktrees"]), false);
  assert.equal(intake.asksAgain("home_copy_linked_checkout", ["too_large"]), true);
  assert.equal(intake.asksAgain("home_copy_linked_checkout", ["linked_checkout"]), false);
  assert.equal(intake.asksAgain("home_copy_too_large", []), true);
  assert.equal(intake.asksAgain("home_copy_too_large", ["too_large"]), false);
  assert.equal(intake.asksAgain("home_destination_exists", []), false);
  assert.equal(intake.asksAgain("home_move_project", ["worktrees", "too_large"]), false);
  assert.equal(intake.asksAgain("constructor", []), false);
});

const icloudProps = (extra = {}) => ({
  open: true, name: "shop", source: "/Users/mira/Desktop/shop", placeholders: 3, way: "move", downloading: false, progress: null, stopping: false,
  onDownload: noop, onStop: noop, onCancel: noop, ...extra,
});
const footerButtons = (dialog) => {
  const footer = dialog.props.footer;
  return typeof footer.type === "function" ? [footer] : [].concat(footer.props.children ?? []).filter(Boolean);
};
const footerButton = (dialog, label) => footerButtons(dialog).find((button) => button.props.children === label);

test("the iCloud question names the files, the download and what a move does on other Macs", () => {
  const page = html(ICloudDialog, icloudProps());
  assert.match(page, /Download shop from iCloud first\?/);
  assert.match(page, /3 files are only in iCloud; CrazyAgent downloads them first \(needs internet, can take a while\)\./);
  assert.match(page, /Moving takes the folder out of iCloud Desktop on your other Macs too\./);
  const order = ["Download and move", "Download and copy", "Cancel"].map((label) => page.indexOf(label));
  assert.ok(order.every((at) => at >= 0), page);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  assert.equal(footerButton(ICloudDialog(icloudProps()), "Download and move").props.variant, "primary");
  assert.equal(footerButton(ICloudDialog(icloudProps({ way: "copy" })), "Download and copy").props.variant, "primary");
  assert.match(html(ICloudDialog, icloudProps({ placeholders: null })), /Some files are only in iCloud/);
  assert.equal(html(ICloudDialog, icloudProps({ open: false })), "");
});

test("Download and move in the iCloud question downloads, then moves", () => {
  const calls = [];
  footerButton(ICloudDialog(icloudProps({ onDownload: (way) => calls.push(way) })), "Download and move").props.onClick();
  assert.deepEqual(calls, ["move"]);
});

test("Download and copy in the iCloud question downloads, then copies, even when Move was picked", () => {
  const calls = [];
  footerButton(ICloudDialog(icloudProps({ onDownload: (way) => calls.push(way) })), "Download and copy").props.onClick();
  assert.deepEqual(calls, ["copy"]);
});

test("Cancel in the iCloud question closes it and downloads nothing", () => {
  const calls = [];
  const dialog = ICloudDialog(icloudProps({ onDownload: (way) => calls.push(way), onCancel: () => calls.push("cancel") }));
  footerButton(dialog, "Cancel").props.onClick();
  assert.deepEqual(calls, ["cancel"]);
});

test("a running iCloud download shows its progress and only Stop download, which stops it", () => {
  const calls = [];
  const props = icloudProps({ downloading: true, progress: { total: 3, done: 1 }, onStop: () => calls.push("stop") });
  const page = html(ICloudDialog, props);
  assert.match(page, /Downloading from iCloud: 1 of 3 files/);
  assert.doesNotMatch(page, /Download and move|Download and copy/);
  const dialog = ICloudDialog(props);
  assert.equal(dialog.props.busy, true);
  footerButton(dialog, "Stop download").props.onClick();
  assert.deepEqual(calls, ["stop"]);
  assert.match(html(ICloudDialog, icloudProps({ downloading: true })), /Downloading from iCloud\.\.\./);
  assert.match(html(ICloudDialog, icloudProps({ downloading: true, stopping: true })), /Stopping the download\.\.\./);
});

test("the iCloud lines count files, name the iCloud place and read progress", () => {
  assert.equal(intake.icloudCountLine(1), "1 file is only in iCloud; CrazyAgent downloads them first (needs internet, can take a while).");
  assert.throws(() => intake.icloudCountLine(0));
  assert.match(intake.icloudMoveLine("/Users/mira/Documents/shop"), /out of iCloud Documents on/);
  assert.match(intake.icloudMoveLine("/Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop"), /out of iCloud Drive on/);
  assert.match(intake.icloudMoveLine("/Volumes/Work/shop"), /out of iCloud Drive on/);
  assert.equal(intake.icloudProgressLine({ total: 0, done: 0 }), "Downloading from iCloud...");
  assert.throws(() => intake.icloudProgressLine({ total: 1, done: 2 }));
  assert.equal(intake.icloudPlaceholders({ kind: "ok", value: { bytes: 1, placeholders: 7 } }), 7);
  assert.equal(intake.icloudPlaceholders({ kind: "ok", value: { bytes: 1 } }), 0);
  assert.equal(intake.icloudPlaceholders({ kind: "loading" }), 0);
  assert.equal(intake.icloudPlaceholders(null), 0);
});

test("a running iCloud download reads progress each tick and goes on to the add only when nobody stopped it", (t) => {
  t.mock.timers.enable({ apis: ["setInterval"] });
  const calls = [];
  const run = intake.icloudRun(() => calls.push("cancel"), () => calls.push("read"), 1000);
  t.mock.timers.tick(2000);
  assert.deepEqual(calls, ["read", "read"]);
  assert.equal(run.end(), "go");
  t.mock.timers.tick(5000);
  assert.deepEqual(calls, ["read", "read"]);
  assert.throws(() => intake.icloudRun(() => {}, () => {}, 0));
});

test("Stop download asks the engine to stop at once and again each tick until the download returns", (t) => {
  t.mock.timers.enable({ apis: ["setInterval"] });
  const calls = [];
  const run = intake.icloudRun(() => calls.push("cancel"), () => calls.push("read"), 1000);
  run.stop();
  assert.deepEqual(calls, ["cancel"]);
  t.mock.timers.tick(1000);
  assert.deepEqual(calls, ["cancel", "cancel", "read"]);
  assert.equal(run.end(), "stopped");
  t.mock.timers.tick(3000);
  assert.equal(calls.length, 3);
});

test("leaving Add folder mid-download stops it and reads no more progress", (t) => {
  t.mock.timers.enable({ apis: ["setInterval"] });
  const calls = [];
  const run = intake.icloudRun(() => calls.push("cancel"), () => calls.push("read"), 1000);
  run.leave();
  t.mock.timers.tick(2000);
  assert.deepEqual(calls, ["cancel", "cancel", "cancel"]);
  assert.equal(run.end(), "left");
});

test("after an iCloud download Add folder adds only when it finished and was neither stopped nor left", () => {
  const ok = { kind: "ok", value: 3 };
  const ended = { kind: "error", code: "home_icloud_not_downloaded", message: "Some files are still only in iCloud." };
  const denied = { kind: "error", code: "home_permission_denied", message: "Allow it in System Settings." };
  assert.deepEqual(intake.icloudOutcome("go", ok), { next: "add" });
  assert.deepEqual(intake.icloudOutcome("stopped", ok), { next: "say", text: intake.ICLOUD_STOPPED });
  assert.deepEqual(intake.icloudOutcome("stopped", ended), { next: "say", text: intake.ICLOUD_STOPPED });
  assert.deepEqual(intake.icloudOutcome("stopped", denied), { next: "say", text: denied.message });
  assert.deepEqual(intake.icloudOutcome("go", ended), { next: "say", text: ended.message });
  assert.deepEqual(intake.icloudOutcome("go", { kind: "not_ready" }), { next: "say", text: "Downloading from iCloud isn't in this build." });
  for (const result of [ok, ended, { kind: "not_ready" }]) assert.deepEqual(intake.icloudOutcome("left", result), { next: "none" });
});
