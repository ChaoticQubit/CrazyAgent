import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const model = await import("../src/trust-model.ts");
const scm = await import("../src/scm-model.ts");

const file = (path, extra = {}) => ({ path, status: "added", marks: [], ...extra });
const set = (extra = {}) => ({
  id: "a".repeat(32), agent: "Ada (@builder)", state: "pending", head: "b".repeat(40), worktree: "/work/agents/builder/",
  files: [
    file(".vscode/settings.json", { status: "modified", marks: ["ide_config"] }),
    file("shop.code-workspace", { marks: ["ide_config"] }),
    file(".vscode/old.json", { status: "deleted", marks: ["ide_config"] }),
    file("README.md", { status: "modified" }),
  ],
  ...extra,
});

test("settings files of open change sets are decorated in the agent's worktree, deleted ones are not", () => {
  for (const state of ["pending", "conflict", "updating"]) {
    assert.deepEqual(
      [...model.ideConfigFiles([set({ state })])],
      [["/work/agents/builder/.vscode/settings.json", "Ada (@builder)"], ["/work/agents/builder/shop.code-workspace", "Ada (@builder)"]],
      state,
    );
  }
  assert.deepEqual([...model.ideConfigFiles([set({ state: "accepted" }), set({ state: "discarded" })])], []);
  assert.deepEqual([...model.ideConfigFiles([set({ files: [file("README.md")] })])], []);
});

test("one warning names the agent and at most three settings files, once per head", () => {
  assert.equal(
    model.ideConfigWarning(set()),
    "Ada (@builder) changed IDE settings: .vscode/settings.json, shop.code-workspace, .vscode/old.json. Check them before you accept: they change how the IDE behaves in this project.",
  );
  const many = set({ files: ["a", "b", "c", "d", "e"].map((name) => file(`.vscode/${name}.json`, { marks: ["ide_config"] })) });
  assert.match(model.ideConfigWarning(many), /\.vscode\/c\.json and 2 more\. /);
  assert.equal(model.ideConfigWarning(set({ files: [file("README.md")] })), null);
  assert.equal(model.ideConfigWarning(set({ state: "accepted" })), null);
  assert.equal(model.ideConfigWarning(set({ agent: "Ada‮\n" })).startsWith("Ada�� changed"), true);
  assert.notEqual(model.warningKey(set()), model.warningKey(set({ head: "c".repeat(40) })));
});

test("the decoration words match the desktop's and the engine's mark name", () => {
  const desktop = readFileSync(new URL("../../desktop/src/screens/changes/model.ts", import.meta.url), "utf8");
  assert.ok(desktop.includes(`  ${model.IDE_CONFIG}: "${model.IDE_CONFIG_NOTE}",`));
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/changes.rs", import.meta.url), "utf8");
  assert.ok(tui.includes(`FileMark::IdeConfig => "${model.IDE_CONFIG_NOTE}",`));
  assert.equal(model.IDE_CONFIG, scm.IDE_CONFIG);
});
