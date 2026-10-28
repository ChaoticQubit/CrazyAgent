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
  levels: [
    { access: "sandboxed", label: "Sandboxed", unavailable: null },
    { access: "own_branch", label: "Own branch", unavailable: null },
    { access: "full", label: "Full access", unavailable: null },
  ],
  toolchains: [
    { toolchain: "rust", label: "Rust", installed: true, used: true, allowed: true },
    { toolchain: "node", label: "Node", installed: true, used: false, allowed: false },
  ],
};

test("Save sends the level and only the toolchain switches that differ from the project's own use", () => {
  assert.deepEqual(access.policyOf(view, "own_branch", [true, false]), { access: "own_branch", toolchains: [] });
  assert.deepEqual(access.policyOf(view, "sandboxed", [false, true]), {
    access: "sandboxed",
    toolchains: [{ toolchain: "rust", allowed: false }, { toolchain: "node", allowed: true }],
  });
  assert.throws(() => access.policyOf(view, "full", [true]), /one switch per toolchain/);
});

test("Full access asks for the plain warning first, and the project page names the level and toolchains", () => {
  assert.equal(access.confirmFirst("sandboxed", "full"), true);
  assert.equal(access.confirmFirst(null, "full"), true);
  assert.equal(access.confirmFirst("full", "full"), false);
  assert.equal(access.confirmFirst("full", "own_branch"), false);
  assert.equal(access.FULL_ACCESS_WARNING, "Agents run with your user's permissions: they can read your keys and change any file.");
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
  for (const file of ["sandbox/policy.rs", "project_setup.rs", "storage.rs"]) {
    const engine = readFileSync(new URL(`../../../crates/crazyagent/src/${file}`, import.meta.url), "utf8");
    for (const code of engine.matchAll(/"(agent_(?:access|policy)_[a-z_]+)"/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
  }
});

test("Settings says beside the level choice that lowering it stops agents running above it, in both UIs", () => {
  assert.equal(access.LOWERING_STOPS_AGENTS, "Lowering the level stops agents running above it; they restart at the new level.");
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<AccessLevels name="ps-access"[^\n]*\/>}\n\s*{view && <p className="pj-help">{LOWERING_STOPS_AGENTS}<\/p>}/);
  assert.ok(!page.includes("keep the access they started with"));
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${access.LOWERING_STOPS_AGENTS}"`));
});

test("agent access saves at the version the page read, and a stale save reads the saved access again", () => {
  assert.match(settings.ACCESS_STALE, /Agent access changed since you opened it\. The page now shows the saved access/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const save = page.slice(page.indexOf("  async function save() {\n    if (!view) return;"));
  assert.match(save, /saveAgentPolicy\(data\.id, policyOf\(view, access, allowed\), view\.version, nextGeneration\(\)\)/);
  assert.match(save, /if \(savedStale\(result\)\) \{\n      const fresh = await loadAgentPolicy\(data\.id\);/, "the stale save reads the access and its version again");
  assert.match(save, /seed\(fresh\.value\);\n      return setNote\(ACCESS_STALE\);/);
  assert.ok(save.indexOf("savedStale(result)") >= 0 && save.indexOf("savedStale(result)") < save.indexOf("failureCopy(result)"), "before the plain refusal copy");
  const client = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(client, /export interface PolicyView \{[\s\S]*?\n  version: number;\n\}/);
  assert.match(client, /call<IntentReceipt>\("desktop_set_project_agent_policy", \{ id, version, policy, generation \}\)/);
  const created = readFileSync(new URL("../src/screens/new-project/index.tsx", import.meta.url), "utf8");
  assert.match(created, /const read = await loadAgentPolicy\(id\);\n\s*const saved = read\.kind === "ok" \? await saveAgentPolicy\(id, \{ access, toolchains: \[\] \}, read\.value\.version, nextGeneration\(\)\) : read;/,
    "a new project's chosen access is saved at the version the engine holds");
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(lib, /async fn desktop_set_project_agent_policy\([^)]*version: u64/);
});
