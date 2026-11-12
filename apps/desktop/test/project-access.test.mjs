import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const access = await import("../src/screens/project/access.tsx");
const settings = await import("../src/screens/project/settings.tsx");
const wizard = await import("../src/screens/new-project/wizard.ts");
const api = await import("../src/api.ts");

const view = {
  access: "sandboxed",
  network: "open",
  levels: [
    { access: "sandboxed", label: "Sandboxed", unavailable: null },
    { access: "own_branch", label: "Own branch", unavailable: null },
    { access: "full", label: "Full access", unavailable: null },
  ],
  networks: [
    { network: "open", label: "Open" },
    { network: "https", label: "Port 443 only" },
    { network: "none", label: "None" },
  ],
  toolchains: [
    { toolchain: "rust", label: "Rust", installed: true, used: true, allowed: true },
    { toolchain: "node", label: "Node", installed: true, used: false, allowed: false },
  ],
  ide_git_hooks: false,
  tool_cache: "shared",
  tool_caches: [
    { scope: "shared", label: "Shared by agents with the same access" },
    { scope: "per_agent", label: "One per agent" },
  ],
  ide_agent_files: "allow_all",
  shared_folders: { scope: "all", access: "read_only", chosen: [], secrets: [] },
  local_folders: ["build", "program"],
};
const SHARED = { scope: "all", access: "read_only", chosen: [], secrets: [] };

test("Save sends the level, the network choice and only the toolchain switches that differ from the project's own use", () => {
  assert.deepEqual(access.policyOf(view, "own_branch", "open", [true, false], false, "allow_all"), { access: "own_branch", network: "open", toolchains: [], ide_git_hooks: false, tool_cache: "shared", ide_agent_files: "allow_all", shared_folders: SHARED });
  assert.deepEqual(access.policyOf(view, "sandboxed", "https", [false, true], false, "allow_all"), {
    access: "sandboxed",
    network: "https",
    toolchains: [{ toolchain: "rust", allowed: false }, { toolchain: "node", allowed: true }],
    ide_git_hooks: false,
    tool_cache: "shared",
    ide_agent_files: "allow_all",
    shared_folders: SHARED,
  });
  assert.throws(() => access.policyOf(view, "full", "none", [true], false, "allow_all"), /one switch per toolchain/);
});

test("Full access asks for the plain warning first, and the project page names the level and toolchains", () => {
  assert.equal(access.confirmFirst("full"), true, "Full asks on every save, whatever level the screen showed");
  assert.equal(access.confirmFirst("own_branch"), false);
  assert.equal(access.confirmFirst("sandboxed"), false);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /onClick=\{\(\) => confirmFirst\(access\) \? setConfirming\(true\) : void save\(\)\}/);
  assert.equal(access.FULL_ACCESS_WARNING, "Agents run with your user's permissions: they can read your keys and change any file. Processes an agent starts outside its terminal can keep running after it stops.");
  assert.equal(access.accessSummary(view), "Sandboxed · Rust");
  assert.equal(access.accessSummary({ ...view, access: "full", toolchains: [] }), "Full access · no toolchains");
  assert.equal(settings.settingsSection("access"), "access");
});

test("New Project preselects Own branch and every level the engine knows has the engine's label", () => {
  assert.equal(wizard.emptyDraft().access, "own_branch");
  assert.equal(wizard.NEW_PROJECT_ACCESS, "own_branch");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/policy.rs", import.meta.url), "utf8");
  assert.match(engine, /pub const NEW_PROJECT: Self = Self::OwnBranch;/);
  for (const level of access.ACCESS_LEVELS) assert.ok(engine.includes(`=> "${level.access}"`) && engine.includes(`=> "${level.label}"`), level.access);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.FULL_ACCESS_WARNING}"`));
});

test("New Project preselects Sandboxed where the engine says Own branch can't run, and greys what can't", () => {
  const linux = "On Linux, agents run Sandboxed in this version.";
  const levels = (reason) => [
    { access: "sandboxed", label: "Sandboxed", unavailable: null },
    { access: "own_branch", label: "Own branch", unavailable: reason },
    { access: "full", label: "Full access", unavailable: reason },
  ];
  const draft = wizard.emptyDraft();
  assert.equal(wizard.draftAccess(draft, levels(null)), "own_branch");
  assert.equal(wizard.draftAccess(draft, levels(linux)), "sandboxed");
  assert.equal(wizard.draftAccess({ ...draft, access: "full" }, levels(null)), "full");
  assert.equal(wizard.draftAccess({ ...draft, access: "full" }, levels(linux)), "sandboxed");
  assert.equal(wizard.draftAccess({ ...draft, access: undefined }, levels(linux)), "sandboxed", "a draft kept from before the setting follows the same rule");
  assert.equal(wizard.draftAccess(draft, undefined), "own_branch", "an engine that names no levels keeps the preselect");
  assert.equal(wizard.levelUnavailable(levels(linux), "own_branch"), linux);
  assert.equal(wizard.levelUnavailable(levels(linux), "sandboxed"), null);
  assert.equal(wizard.levelUnavailable(undefined, "full"), null);
  const page = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.match(page, /const access = draftAccess\(draft, catalogValue\?\.agent_access\);/, "create saves the level the form shows");
  assert.match(page, /<AccessLevels name="np-access" value=\{draftAccess\(draft, catalogValue\?\.agent_access\)\} unavailable=\{\(level\) => levelUnavailable\(catalogValue\?\.agent_access, level\)\}/);
  assert.ok(!page.includes("unavailable={() => null}"));
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/project_setup.rs", import.meta.url), "utf8");
  assert.match(engine, /pub agent_access: Vec<LevelView>,/, "the project catalog carries the levels this computer can run");
});

test("every engine refusal for agent access has its own sentence", () => {
  const missing = new Set();
  for (const file of ["sandbox/policy.rs", "project_setup.rs", "storage.rs"]) {
    const engine = readFileSync(new URL(`../../../crates/crazyagent/src/${file}`, import.meta.url), "utf8");
    for (const code of engine.matchAll(/"(agent_(?:access|policy|network|toolchain|project)_[a-z_]+)"/g)) {
      if (!api.KNOWN_ERROR_CODES.includes(code[1])) missing.add(code[1]);
    }
  }
  assert.deepEqual([...missing], [], "every access, policy, network, toolchain and project refusal has a sentence");
});

test("a stop for lowered access claims only what is true at every level", () => {
  assert.equal(
    api.knownError("agent_access_lowered"),
    "This project's agent access was lowered below what the agent started with, so it was stopped. What it changed in its own folder is still in Changes; an agent that ran at Full access may also have changed files outside it.",
  );
});

test("Settings says beside the level choice that lowering it stops agents running above it, in both UIs", () => {
  assert.equal(access.LOWERING_STOPS_AGENTS, "Lowering the level stops agents running above it; they restart at the new level. It does not reach processes a Full access agent started outside its terminal.");
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<AccessLevels name="ps-access"[^\n]*\/>}\n\s*{view && <p className="pj-help">{LOWERING_STOPS_AGENTS}<\/p>}/);
  assert.ok(!page.includes("keep the access they started with"));
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.LOWERING_STOPS_AGENTS}"`));
});

test("every Save of agent access says a saved change restarts the project's IDE, in both UIs", () => {
  assert.equal(access.POLICY_RESTARTS_IDE, "Changing the level, the IDE hooks switch or the agent folders choice below Full access, or the network choice at Own branch restarts this project's IDE at once. Its terminals end, and it reopens within a minute.");
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /{view && <p className="pj-help">{LOWERING_STOPS_AGENTS}<\/p>}\n\s*{view && <p className="pj-help">{POLICY_RESTARTS_IDE}<\/p>}/);
  const team = readFileSync(new URL("../src/screens/team/setup-rows.tsx", import.meta.url), "utf8");
  assert.match(team, /{LOWERING_STOPS_AGENTS} {POLICY_RESTARTS_IDE}<\/span>/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.POLICY_RESTARTS_IDE}"`));
});

test("agent access saves at the version the page read, and a stale save reads the saved access again", () => {
  assert.match(settings.ACCESS_STALE, /Agent access changed since you opened it\. The page now shows the saved access/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const shared = page.slice(page.indexOf("export async function saveAccess("));
  assert.match(shared, /saveAgentPolicy\(projectId, policy, view\.version, nextGeneration\(\)\)/);
  assert.match(shared, /if \(savedStale\(result\)\) \{\n    const fresh = await loadAgentPolicy\(projectId\);/, "the stale save reads the access and its version again");
  assert.match(shared, /fresh\.kind === "ok" \? \{ note: ACCESS_STALE, fresh: fresh\.value \}/);
  assert.ok(shared.indexOf("savedStale(result)") >= 0 && shared.indexOf("savedStale(result)") < shared.indexOf("failureCopy(result)"), "before the plain refusal copy");
  const save = page.slice(page.indexOf("  async function save() {\n    if (!view) return;"));
  assert.match(save, /saveAccess\(data\.id, view, policyOf\(view, access, network, allowed, ideGitHooks, ideAgentFiles, toolCache, sharedFolders\)\)/);
  assert.match(save, /if \(saved\.fresh\) seed\(saved\.fresh\);\n    setNote\(saved\.note\);/);
  const rows = readFileSync(new URL("../src/screens/team/setup-rows.tsx", import.meta.url), "utf8");
  assert.match(rows, /saveAccess\(projectId, view, policyOf\(view, access, view\.network, view\.toolchains\.map\(\(row\) => row\.allowed\), view\.ide_git_hooks, view\.ide_agent_files\)\)/,
    "Agent settings saves access through the same save, keeping the saved network and toolchains");
  const client = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(client, /export interface PolicyView \{[\s\S]*?\n  version: number;\n\}/);
  assert.match(client, /call<IntentReceipt>\("desktop_set_project_agent_policy", \{ id, version, policy, generation \}\)/);
  const created = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.match(created, /const read = await loadAgentPolicy\(id\);\n\s*const saved = read\.kind === "ok" \? await saveAgentPolicy\(id, \{ access, network: "open", toolchains: \[\], ide_git_hooks: false, ide_agent_files: "allow_all" \}, read\.value\.version, nextGeneration\(\)\) : read;/,
    "a new project's chosen access is saved at the version the engine holds");
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(lib, /async fn desktop_set_project_agent_policy\([^)]*version: u64/);
});

test("the IDE hooks switch is saved with the policy and reads the same in both UIs, Off for a new project", () => {
  assert.deepEqual(access.policyOf(view, "sandboxed", "open", [true, false], true, "allow_all"), { access: "sandboxed", network: "open", toolchains: [], ide_git_hooks: true, tool_cache: "shared", ide_agent_files: "allow_all", shared_folders: SHARED });
  assert.equal(access.IDE_GIT_HOOKS, "IDE git runs the project's hooks");
  assert.match(access.IDE_GIT_HOOKS_LINE, /never signed/);
  assert.doesNotMatch(access.IDE_GIT_HOOKS_LINE, /Sandboxed terminal/);
  assert.match(access.IDE_GIT_HOOKS_LINE, /Applies when the IDE next starts\.$/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.IDE_GIT_HOOKS}"`));
  assert.ok(tui.includes(`"${access.IDE_GIT_HOOKS_LINE}"`));
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.ok(page.includes("policyOf(view, access, network, allowed, ideGitHooks, ideAgentFiles, toolCache, sharedFolders)"));
  assert.ok(page.includes("ideGitHooks !== view.ide_git_hooks"));
  assert.ok(page.includes("setIdeGitHooks(next.ide_git_hooks)"));
  assert.match(page, /<IdeGitHooksSwitch prefix="ps" checked={ideGitHooks} onChange={setIdeGitHooks} \/>/);
  const created = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.ok(created.includes('{ access, network: "open", toolchains: [], ide_git_hooks: false, ide_agent_files: "allow_all" }'));
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/policy.rs", import.meta.url), "utf8");
  assert.match(engine, /#\[serde\(default\)\]\n    pub ide_git_hooks: bool,/);
});

test("the tool cache choice is saved with the policy, the saved one unless the owner picks another, with the engine's labels and what each means", () => {
  assert.deepEqual(access.policyOf(view, "sandboxed", "open", [true, false], false, "allow_all", "per_agent"), { access: "sandboxed", network: "open", toolchains: [], ide_git_hooks: false, tool_cache: "per_agent", ide_agent_files: "allow_all", shared_folders: SHARED });
  assert.equal(access.policyOf({ ...view, tool_cache: "per_agent" }, "sandboxed", "open", [true, false], false, "allow_all").tool_cache, "per_agent", "a save that shows no choice keeps the saved one");
  assert.deepEqual(access.TOOL_CACHE_CHOICES.map((choice) => [choice.scope, choice.label]), view.tool_caches.map((row) => [row.scope, row.label]));
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/policy.rs", import.meta.url), "utf8");
  for (const choice of access.TOOL_CACHE_CHOICES) assert.ok(engine.includes(`"${choice.label}"`), `${choice.label} is the engine's label`);
  assert.match(access.TOOL_CACHE_CLEAN, /^uv cache clean fails inside an agent's sandbox\./);
  assert.match(access.TOOL_CACHE_CHOICES.find((choice) => choice.scope === "shared").help, /One agent can change what the other agents at its level install\./, "Shared says one agent can change what the others install");
  assert.match(access.TOOL_CACHE_IGNORED_UNDER_FULL, /^Full access runs agents without a sandbox/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.ok(page.includes("setToolCache(next.tool_cache)"));
  assert.ok(page.includes("toolCache !== view.tool_cache"));
  assert.match(page, /<ToolCacheChoices name="ps-tool-cache" value={toolCache} access={access} onChange={setToolCache} \/>/);
  const rows = readFileSync(new URL("../src/screens/team/setup-rows.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(rows, /tool_cache|toolCache/, "Agent settings' level row keeps the saved choice through policyOf");
});

test("the IDE agent folders choice is saved with the policy, restarts the IDE and reads the same in both UIs, Allow all changes by default", () => {
  for (const choice of ["allow_all", "no_delete_or_rename", "read_only"]) {
    assert.equal(access.policyOf(view, "own_branch", "open", [true, false], false, choice).ide_agent_files, choice);
  }
  assert.deepEqual(access.IDE_AGENT_FILES_CHOICES.map((choice) => [choice.choice, choice.label]), [
    ["allow_all", "Allow all changes"],
    ["no_delete_or_rename", "No delete or rename"],
    ["read_only", "Read only"],
  ]);
  assert.match(access.IDE_AGENT_FILES_CHOICES[1].help, /Edit anyway included, but not deleted or renamed/);
  assert.match(access.IDE_AGENT_FILES_CHOICES[2].help, /Edit anyway included\.$/);
  assert.equal(access.AGENT_FILES_IGNORED_UNDER_FULL, "Full access runs the IDE without a sandbox, so this choice does not apply.");
  assert.match(access.POLICY_RESTARTS_IDE, /the agent folders choice below Full access/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/policy.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.IDE_AGENT_FILES}"`));
  assert.ok(tui.includes(`"${access.AGENT_FILES_IGNORED_UNDER_FULL}"`));
  for (const choice of access.IDE_AGENT_FILES_CHOICES) {
    assert.ok(tui.includes(`"${choice.help}"`), choice.choice);
    assert.ok(engine.includes(`=> "${choice.label}"`), choice.choice);
  }
  assert.match(engine, /#\[serde\(default\)\]\n    pub ide_agent_files: IdeAgentFiles,/);
  const card = readFileSync(new URL("../src/screens/project/access.tsx", import.meta.url), "utf8");
  assert.match(card, /access === "full"[^\n]*AGENT_FILES_IGNORED_UNDER_FULL/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.ok(page.includes("ideAgentFiles !== view.ide_agent_files"));
  assert.ok(page.includes("setIdeAgentFiles(next.ide_agent_files)"));
  assert.match(page, /<IdeGitHooksSwitch prefix="ps" checked={ideGitHooks} onChange={setIdeGitHooks} \/>}\n\s*{view && <IdeAgentFilesChoices name="ps-agent-files" value={ideAgentFiles} access={access} onChange={setIdeAgentFiles} \/>}/);
});

test("None says it limits agents to their model's own hosts through the proxy or the local model's port, and Full access annotates the network choice as moot", () => {
  const none = access.NETWORK_CHOICES.find((choice) => choice.network === "none");
  assert.equal(none.help, "None limits agents to their model: a hosted model's own hosts, through the engine's proxy, or only the local model's port. The plugin check reaches nothing.");
  for (const choice of access.NETWORK_CHOICES) assert.doesNotMatch(choice.help, /only the model|port 443 \(HTTPS to any host\)|does not yet limit/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${none.help}"`), "the TUI shows the same None line");
  assert.equal(access.NETWORK_IGNORED_UNDER_FULL, "Full access runs agents without a sandbox, so this choice does not apply.");
  const card = readFileSync(new URL("../src/screens/project/access.tsx", import.meta.url), "utf8");
  assert.match(card, /access === "full"[^\n]*NETWORK_IGNORED_UNDER_FULL/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<NetworkChoices[^>]*access=\{access\}/);
});

test("Hiding a folder takes an absolute path, rejects duplicates and caps at the engine's limit", () => {
  assert.deepEqual(settings.addHiddenPath([], "/Users/you/project/secrets"), { paths: ["/Users/you/project/secrets"] });
  assert.ok("error" in settings.addHiddenPath([], "secrets"));
  assert.ok("error" in settings.addHiddenPath([], "/Users/you/a\"b"));
  assert.ok("error" in settings.addHiddenPath([], "/Users/you/a\\b"));
  assert.ok("error" in settings.addHiddenPath([], "/Users/you/a'b"));
  assert.ok("error" in settings.addHiddenPath(["/a"], "/a"));
  const full = Array.from({ length: settings.HIDDEN_PATHS_MAX }, (_, index) => `/dir${index}`);
  assert.ok("error" in settings.addHiddenPath(full, "/one-more"));
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<HiddenSection data=\{data\} \/>/);
  for (const code of ["hidden_paths_too_many", "hidden_paths_invalid", "hidden_path_not_absolute", "hidden_path_not_plain", "hidden_path_missing", "hidden_path_not_folder", "hidden_path_outside_project", "hidden_path_tracked", "hidden_path_unchecked"]) {
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
});

test("Settings says beside the toolchain switches that turning one off stops agents, research jobs and master turns running with it, in both UIs", () => {
  assert.equal(access.TOOLCHAIN_OFF_STOPS_AGENTS, "Turning a toolchain off stops agents, research jobs and master turns running with it; their next start goes without it.");
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<ToolchainSwitches prefix="ps"[^\n]*\/>}\n\s*{view && <p className="pj-help">{TOOLCHAIN_OFF_STOPS_AGENTS}<\/p>}/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.TOOLCHAIN_OFF_STOPS_AGENTS}"`));
});

test("Full access says what it means for the IDE wherever it asks, and the hooks line says it has no effect there", () => {
  assert.equal(access.FULL_ACCESS_IDE, "The IDE runs with your user's permissions. The project's network choice does not limit it. The AI lock and extension approvals are written at each IDE start, and anything inside the IDE can change them until the next one. Workspace trust is on. Until you trust the folder, IDE git and extensions that need trust stay off.");
  const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
  assert.match(read("../src/screens/project/access.tsx"), /{value === "full" && <p className="pj-note warn" role="alert">{FULL_ACCESS_WARNING} {FULL_ACCESS_IDE}<\/p>}/);
  for (const page of ["../src/screens/project/settings.tsx", "../src/screens/team/setup-rows.tsx"]) {
    assert.match(read(page), /{FULL_ACCESS_WARNING} {FULL_ACCESS_IDE}\n\s*<\/ConfirmDialog>/, page);
  }
  assert.ok(read("../../../crates/crazyagent/src/tui/screens/project_access.rs").includes(`"${access.FULL_ACCESS_IDE}"`));
  assert.ok(access.IDE_GIT_HOOKS_LINE.includes("No effect at Full access: git runs as yours."), access.IDE_GIT_HOOKS_LINE);
  assert.match(access.IDE_GIT_HOOKS_LINE, /Applies when the IDE next starts\.$/);
});

test("the shared folders choice is saved with the policy, the saved one unless the owner picks another, with the engine's labels in both UIs", () => {
  for (const scope of ["all", "chosen", "none"]) {
    for (const choice of ["read_only", "read_write"]) {
      const shared = { scope, access: choice, chosen: ["program"], secrets: [".env"] };
      assert.deepEqual(access.policyOf(view, "sandboxed", "open", [true, false], false, "allow_all", "shared", shared).shared_folders, shared, `${scope} ${choice}`);
    }
  }
  const saved = { scope: "none", access: "read_write", chosen: [], secrets: [] };
  assert.deepEqual(access.policyOf({ ...view, shared_folders: saved }, "sandboxed", "open", [true, false], false, "allow_all").shared_folders, saved, "a save that shows no choice keeps the saved one");
  assert.deepEqual(access.SHARED_SCOPE_CHOICES.map((choice) => [choice.scope, choice.label]), [["all", "All local folders"], ["chosen", "Chosen folders"], ["none", "None"]]);
  assert.deepEqual(access.SHARED_ACCESS_CHOICES.map((choice) => [choice.access, choice.label]), [["read_only", "Read-only"], ["read_write", "Read-write"]]);
  assert.deepEqual(access.DEFAULT_SHARED_FOLDERS, SHARED, "All with Read-only by default");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/shared_folders.rs", import.meta.url), "utf8");
  for (const choice of [...access.SHARED_SCOPE_CHOICES, ...access.SHARED_ACCESS_CHOICES]) {
    assert.ok(engine.includes(`=> "${choice.label}"`), `${choice.label} is the engine's label`);
    assert.ok(engine.includes(`"${choice.help}"`), `${choice.label} says what the engine's help says`);
  }
  assert.ok(engine.includes(`pub const OFF_MACOS_LINE: &str = "${access.SHARED_FOLDERS_OFF_MACOS}";`), "the Linux and Windows gap reads as the engine states it");
  assert.ok(engine.includes(`pub const CHOSEN_MAX: usize = ${access.SHARED_CHOSEN_MAX};`));
  assert.ok(engine.includes(`pub const SECRETS_MAX: usize = ${access.SHARED_SECRETS_MAX};`));
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  for (const line of [access.SHARED_FOLDERS, access.SHARED_SECRETS_LINE, access.SHARED_FOLDERS_NEXT_START, access.SHARED_FOLDERS_IGNORED_UNDER_FULL]) {
    assert.ok(tui.includes(`"${line}"`), `the TUI shows ${line}`);
  }
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.ok(page.includes("setShared(next.shared_folders)"));
  assert.ok(page.includes("!sameShared(sharedFolders, view.shared_folders)"));
  assert.match(page, /<IdeAgentFilesChoices [^\n]*\/>}\n\s*{view && <SharedFoldersChoices prefix="ps" value={shared} secretsText={secretsText} local={view.local_folders} access={access} onChange={setShared} onSecretsText={setSecretsText} \/>}/);
  assert.match(page, /disabled={!changed \|\| !sharedValid \|\| busy}/, "Save waits while too many secret paths are typed");
  const rows = readFileSync(new URL("../src/screens/team/setup-rows.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(rows, /shared_folders|sharedFolders/, "Agent settings' level row keeps the saved choice through policyOf");
  const card = readFileSync(new URL("../src/screens/project/access.tsx", import.meta.url), "utf8");
  assert.match(card, /access === "full"[^\n]*SHARED_FOLDERS_IGNORED_UNDER_FULL/);
});

test("secret paths are read one per line, the picker keeps a picked folder that is gone, and the choice stays within the engine's bounds", () => {
  assert.deepEqual(access.sharedSecretsOf("  .env\n\nkeys/server.pem \n.env\n"), [".env", "keys/server.pem"]);
  assert.deepEqual(access.sharedSecretsOf(""), []);
  assert.deepEqual(access.pickableFolders(["program", "build"], ["old", "build"]), ["build", "old", "program"]);
  assert.equal(access.sameShared(SHARED, { ...SHARED }), true);
  assert.equal(access.sameShared(SHARED, { ...SHARED, chosen: ["program"] }), false);
  assert.equal(access.sameShared(SHARED, { ...SHARED, access: "read_write" }), false);
  const many = Array.from({ length: access.SHARED_CHOSEN_MAX + 1 }, (_, index) => `f${index}`);
  assert.throws(() => access.policyOf(view, "sandboxed", "open", [true, false], false, "allow_all", "shared", { ...SHARED, scope: "chosen", chosen: many }), /within the engine's bounds/);
  const secrets = Array.from({ length: access.SHARED_SECRETS_MAX + 1 }, (_, index) => `s${index}`);
  assert.throws(() => access.policyOf(view, "sandboxed", "open", [true, false], false, "allow_all", "shared", { ...SHARED, secrets }), /within the engine's bounds/);
});
