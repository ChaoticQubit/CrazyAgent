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
};

test("Save sends the level, the network choice and only the toolchain switches that differ from the project's own use", () => {
  assert.deepEqual(access.policyOf(view, "own_branch", "open", [true, false]), { access: "own_branch", network: "open", toolchains: [] });
  assert.deepEqual(access.policyOf(view, "sandboxed", "https", [false, true]), {
    access: "sandboxed",
    network: "https",
    toolchains: [{ toolchain: "rust", allowed: false }, { toolchain: "node", allowed: true }],
  });
  assert.throws(() => access.policyOf(view, "full", "none", [true]), /one switch per toolchain/);
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

test("None says it limits agents to their model's port, not yet the model host, and Full access annotates the network choice as moot", () => {
  const none = access.NETWORK_CHOICES.find((choice) => choice.network === "none");
  assert.equal(none.help, "None limits agents to their model's port: port 443 to any host for a hosted model, or only the local model's port. It does not yet limit them to the model host. The plugin check reaches nothing.");
  for (const choice of access.NETWORK_CHOICES) assert.doesNotMatch(choice.help, /only the model|port 443 \(HTTPS to any host\)/);
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/project_access.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`"${none.help}"`), "the TUI shows the same None line");
  assert.equal(access.NETWORK_IGNORED_UNDER_FULL, "Full access runs agents without a sandbox, so this choice does not apply.");
  const card = readFileSync(new URL("../src/screens/project/access.tsx", import.meta.url), "utf8");
  assert.match(card, /access === "full"[^\n]*NETWORK_IGNORED_UNDER_FULL/);
  const page = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(page, /<NetworkChoices[^>]*access=\{access\}/);
});
