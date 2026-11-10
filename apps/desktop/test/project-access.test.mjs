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
};

test("Save sends the level, the network choice and only the toolchain switches that differ from the project's own use", () => {
  assert.deepEqual(access.policyOf(view, "own_branch", "open", [true, false], false), { access: "own_branch", network: "open", toolchains: [], ide_git_hooks: false });
  assert.deepEqual(access.policyOf(view, "sandboxed", "https", [false, true], false), {
    access: "sandboxed",
    network: "https",
    toolchains: [{ toolchain: "rust", allowed: false }, { toolchain: "node", allowed: true }],
    ide_git_hooks: false,
  });
  assert.throws(() => access.policyOf(view, "full", "none", [true], false), /one switch per toolchain/);
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

test("agent access saves at the version the page read, and a stale save reads the saved access again", () => {
  assert.match(settings.ACCESS_STALE, /Agent access changed since you opened it\. The page now shows the saved access/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const shared = page.slice(page.indexOf("export async function saveAccess("));
  assert.match(shared, /saveAgentPolicy\(projectId, policy, view\.version, nextGeneration\(\)\)/);
  assert.match(shared, /if \(savedStale\(result\)\) \{\n    const fresh = await loadAgentPolicy\(projectId\);/, "the stale save reads the access and its version again");
  assert.match(shared, /fresh\.kind === "ok" \? \{ note: ACCESS_STALE, fresh: fresh\.value \}/);
  assert.ok(shared.indexOf("savedStale(result)") >= 0 && shared.indexOf("savedStale(result)") < shared.indexOf("failureCopy(result)"), "before the plain refusal copy");
  const save = page.slice(page.indexOf("  async function save() {\n    if (!view) return;"));
  assert.match(save, /saveAccess\(data\.id, view, policyOf\(view, access, network, allowed, ideGitHooks\)\)/);
  assert.match(save, /if \(saved\.fresh\) seed\(saved\.fresh\);\n    setNote\(saved\.note\);/);
  const rows = readFileSync(new URL("../src/screens/team/setup-rows.tsx", import.meta.url), "utf8");
  assert.match(rows, /saveAccess\(projectId, view, policyOf\(view, access, view\.network, view\.toolchains\.map\(\(row\) => row\.allowed\), view\.ide_git_hooks\)\)/,
    "Agent settings saves access through the same save, keeping the saved network and toolchains");
  const client = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(client, /export interface PolicyView \{[\s\S]*?\n  version: number;\n\}/);
  assert.match(client, /call<IntentReceipt>\("desktop_set_project_agent_policy", \{ id, version, policy, generation \}\)/);
  const created = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.match(created, /const read = await loadAgentPolicy\(id\);\n\s*const saved = read\.kind === "ok" \? await saveAgentPolicy\(id, \{ access, network: "open", toolchains: \[\], ide_git_hooks: false \}, read\.value\.version, nextGeneration\(\)\) : read;/,
    "a new project's chosen access is saved at the version the engine holds");
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(lib, /async fn desktop_set_project_agent_policy\([^)]*version: u64/);
});

test("the IDE hooks switch is saved with the policy and reads the same in both UIs, Off for a new project", () => {
  assert.deepEqual(access.policyOf(view, "sandboxed", "open", [true, false], true), { access: "sandboxed", network: "open", toolchains: [], ide_git_hooks: true });
  assert.equal(access.IDE_GIT_HOOKS, "IDE git runs the project's hooks");
  assert.match(access.IDE_GIT_HOOKS_LINE, /never signed/);
  assert.doesNotMatch(access.IDE_GIT_HOOKS_LINE, /Sandboxed terminal/);
  assert.match(access.IDE_GIT_HOOKS_LINE, /Applies when the IDE next starts\.$/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.IDE_GIT_HOOKS}"`));
  assert.ok(tui.includes(`"${access.IDE_GIT_HOOKS_LINE}"`));
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.ok(page.includes("policyOf(view, access, network, allowed, ideGitHooks)"));
  assert.ok(page.includes("ideGitHooks !== view.ide_git_hooks"));
  assert.ok(page.includes("setIdeGitHooks(next.ide_git_hooks)"));
  assert.match(page, /<IdeGitHooksSwitch prefix="ps" checked={ideGitHooks} onChange={setIdeGitHooks} \/>/);
  const created = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.ok(created.includes('{ access, network: "open", toolchains: [], ide_git_hooks: false }'));
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/policy.rs", import.meta.url), "utf8");
  assert.match(engine, /#\[serde\(default\)\]\n    pub ide_git_hooks: bool,/);
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
