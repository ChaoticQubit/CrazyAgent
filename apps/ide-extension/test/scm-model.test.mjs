import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const model = await import("../src/scm-model.ts");

const NOW = Date.UTC(2026, 9, 27, 14, 0);
const ID = "a".repeat(32);
const file = (extra = {}) => ({ path: "README.md", old_path: null, status: "modified", kind: "text", marks: [], ...extra });
const set = (extra = {}) => ({
  id: ID, agent: "Ada (@builder)", role: "sub", title: "Name the shop for gardens", state: "pending", waiting: false,
  head: "b".repeat(40), message: "Name the shop for gardens\n", worktree: "/work/agents/builder", files: [file()],
  restorable_until: null, problem: null, ...extra,
});

test("a pending change set is one Source Control group named for its agent and title, with Accept and Discard", () => {
  const [group, ...rest] = model.scmGroups([set()], NOW);
  assert.equal(rest.length, 0);
  assert.equal(group.set, ID);
  assert.equal(group.label, "Ada (@builder): Name the shop for gardens");
  assert.equal(group.id, `crazyagent.${ID}.open`);
  assert.deepEqual([group.accept, group.discard, group.restore], [true, true, false]);
  assert.deepEqual(group.resources, [{
    path: "README.md", file: "/work/agents/builder/README.md", base: `/${ID}/README.md`, config: null, head: `/${ID}/README.md`, open: "diff",
    letter: "M", tooltip: "Modified", strikeThrough: false,
  }]);
});

test("a text diff's agent side is the text at the recorded head, never the agent's worktree file", () => {
  const files = [file(), file({ path: "old.txt", status: "deleted" }), file({ path: "logo.png", kind: "binary" })];
  const [group] = model.scmGroups([set({ files })], NOW);
  const shown = Object.fromEntries(group.resources.map((resource) => [resource.path, resource]));
  assert.equal(model.HEAD_SCHEME, "crazyagent-head");
  assert.notEqual(model.HEAD_SCHEME, model.BASE_SCHEME);
  assert.equal(shown["README.md"].head, `/${ID}/README.md`);
  assert.notEqual(shown["README.md"].head, shown["README.md"].file);
  assert.deepEqual(model.parseBasePath(shown["README.md"].head), { id: ID, path: "README.md" });
  assert.equal(shown["old.txt"].head, null);
  assert.equal(shown["logo.png"].head, null);
  const scm = readFileSync(new URL("../src/scm.ts", import.meta.url), "utf8");
  assert.match(scm, /const right = resource\.config === null \? head : /);
  assert.match(scm, /arguments: \[base, right, /);
  const content = readFileSync(new URL("../src/base-content.ts", import.meta.url), "utf8");
  assert.match(content, /\[HEAD_SCHEME, "ide\.head_blob"\]/);
  assert.match(content, /\{ id: target\.id, head: uri\.query, path: target\.path, page \}/);
});

test("each file opens VS Code's diff from its base text, a deleted one shows its base, and other kinds open as files", () => {
  const files = [
    file({ path: "notes/new.md", status: "added" }),
    file({ path: "old.txt", status: "deleted" }),
    file({ path: "docs/guide.md", old_path: "guide.md", status: "renamed" }),
    file({ path: "logo.png", status: "added", kind: "binary" }),
    file({ path: "latest", status: "added", kind: "symlink", marks: ["symlink"] }),
    file({ path: "bin/grow.sh", status: "added", marks: ["executable"] }),
    file({ path: ".gitmodules", marks: ["modules"] }),
    file({ path: "gone.bin", status: "deleted", kind: "binary" }),
  ];
  const [group] = model.scmGroups([set({ files })], NOW);
  const shown = Object.fromEntries(group.resources.map((resource) => [resource.path, resource]));
  assert.deepEqual([shown["notes/new.md"].open, shown["notes/new.md"].letter, shown["notes/new.md"].base], ["diff", "A", `/${ID}/notes/new.md`]);
  assert.deepEqual([shown["old.txt"].open, shown["old.txt"].letter, shown["old.txt"].strikeThrough], ["base", "D", true]);
  assert.deepEqual([shown["docs/guide.md"].letter, shown["docs/guide.md"].tooltip, shown["docs/guide.md"].base], ["R", "Renamed from guide.md", `/${ID}/docs/guide.md`]);
  assert.deepEqual([shown["logo.png"].open, shown["logo.png"].base, shown["logo.png"].tooltip], ["file", null, `Added, Binary file, no text diff, ${model.LIVE_FILE}`]);
  assert.equal(shown.latest.tooltip, `Added, Symbolic link: points to another path, ${model.LIVE_FILE}`);
  assert.equal(shown["bin/grow.sh"].tooltip, "Added, Can run as a program");
  assert.equal(shown[".gitmodules"].tooltip, "Modified, .gitmodules: changes the project's submodules");
  assert.deepEqual([shown["gone.bin"].open, shown["gone.bin"].base], ["none", null]);
});

test("a row that opens the agent's own file says Accept lands the version the set recorded", () => {
  assert.equal(model.LIVE_FILE, "Opens the agent's current file: Accept lands the version it recorded");
  const files = [
    file(),
    file({ path: "logo.png", kind: "binary" }),
    file({ path: "core", status: "added", kind: "gitlink" }),
    file({ path: "old.txt", status: "deleted" }),
    file({ path: "gone.bin", status: "deleted", kind: "binary" }),
  ];
  const [group] = model.scmGroups([set({ files })], NOW);
  for (const resource of group.resources) {
    const live = resource.open === "file";
    assert.equal(resource.tooltip.endsWith(`, ${model.LIVE_FILE}`), live, resource.path);
    assert.equal(resource.tooltip.split(model.LIVE_FILE).length - 1, live ? 1 : 0, resource.path);
  }
  assert.deepEqual(group.resources.filter((resource) => resource.open === "file").map((resource) => resource.path), ["logo.png", "core"]);
});

test("an Accepted set has no group, a Discarded one keeps Restore only inside its window, and the other states say what they are", () => {
  assert.deepEqual(model.scmGroups([set({ state: "accepted" })], NOW), []);
  const [restorable] = model.scmGroups([set({ state: "discarded", restorable_until: NOW + 1 })], NOW);
  assert.equal(restorable.id, `crazyagent.${ID}.restorable`);
  assert.equal(restorable.label, "Ada (@builder): Name the shop for gardens (discarded, can be restored)");
  assert.deepEqual([restorable.accept, restorable.discard, restorable.restore], [false, false, true]);
  assert.deepEqual(model.scmGroups([set({ state: "discarded", restorable_until: NOW })], NOW), []);
  assert.deepEqual(model.scmGroups([set({ state: "discarded", restorable_until: null })], NOW), []);

  const [conflict] = model.scmGroups([set({ state: "conflict" })], NOW);
  assert.equal(conflict.label, "Ada (@builder): Name the shop for gardens (conflicts with your branch)");
  assert.deepEqual([conflict.accept, conflict.discard, conflict.restore], [true, true, false]);
  const [waiting] = model.scmGroups([set({ state: "updating", waiting: true })], NOW);
  assert.equal(waiting.id, `crazyagent.${ID}.waiting`);
  assert.deepEqual([waiting.accept, waiting.discard, waiting.restore], [false, true, false]);
  const [resolving] = model.scmGroups([set({ state: "updating" })], NOW);
  assert.equal(resolving.id, `crazyagent.${ID}.resolving`);
  assert.equal(resolving.label, "Ada (@builder): Name the shop for gardens (the agent is updating it)");
  assert.deepEqual([resolving.accept, resolving.discard, resolving.restore], [false, false, false]);

  const [unread] = model.scmGroups([set({ files: [], problem: "changes_git_failed" })], NOW);
  assert.equal(unread.label, "Ada (@builder): Name the shop for gardens (CrazyAgent couldn't read these changes)");
  assert.deepEqual(unread.resources, []);
  assert.equal(unread.id, `crazyagent.${ID}.unread`);
  assert.deepEqual([unread.accept, unread.discard, unread.restore], [false, true, false]);
  const [large] = model.scmGroups([set({ files: [], problem: "changes_too_large" })], NOW);
  assert.equal(large.accept, false);
  assert.equal(large.label, "Ada (@builder): Name the shop for gardens (too large to show here)");
  assert.equal(model.scmGroups([set({ title: "" })], NOW)[0].label, "Ada (@builder): Changes");
});

test("groups keep the engine's order and a hostile title or path shows as plain text", () => {
  const second = "c".repeat(32);
  const groups = model.scmGroups([set(), set({ id: second, agent: "Bo (@tester)", title: "Fix\u202e the \u0007tests" })], NOW);
  assert.deepEqual(groups.map((group) => group.set), [ID, second]);
  assert.equal(groups[1].label, "Bo (@tester): Fix\ufffd the \ufffdtests");
  const [odd] = model.scmGroups([set({ files: [file({ path: "a\nb.txt" })] })], NOW);
  assert.equal(odd.resources[0].tooltip, "Modified");
  assert.equal(odd.resources[0].file, "/work/agents/builder/a\nb.txt");
});

test("the base side's address names the change set and path and refuses anything else", () => {
  assert.equal(model.basePath(ID, "src/app.ts"), `/${ID}/src/app.ts`);
  assert.deepEqual(model.parseBasePath(`/${ID}/src/app.ts`), { id: ID, path: "src/app.ts" });
  for (const bad of [`/${ID}`, `/${ID}/`, "/short/src/app.ts", `/${"A".repeat(32)}/x`, `${ID}/x`, `/${ID}/../x`, `/${ID}/a/../../x`, `/${ID}//x`]) {
    assert.equal(model.parseBasePath(bad), null, bad);
  }
});

test("an Accept, Discard or Restore answer reads as the desktop's own sentence", () => {
  assert.equal(model.acceptedNote({ kind: "landed", commit: "1234567890abcdef1234567890abcdef12345678", branch: "main" }), "Accepted as 1234567 on main.");
  assert.equal(model.acceptedNote({ kind: "conflict", files: ["a", "b", "c", "d", "e", "f", "g"] }), "These files also changed on your branch: a, b, c, d, e and 2 more. Ask the agent to update, or discard.");
  assert.equal(model.refusal("made_up_code"), "Something went wrong. Try again. (made_up_code)");
  const desktop = readFileSync(new URL("../../desktop/src/api-changes.ts", import.meta.url), "utf8");
  const sentences = Object.fromEntries([...desktop.matchAll(/^ {2}(changes_[a-z_]+): "([^"]+)",$/gm)].map((match) => [match[1], match[2]]));
  assert.ok(Object.keys(sentences).length >= 20, "the desktop sentences were read");
  for (const code of model.REFUSAL_CODES) {
    assert.equal(model.refusal(code), sentences[code], code);
  }
  for (const code of ["changes_stale", "changes_owner_dirty", "changes_restore_expired", "changes_owner_identity_missing", "changes_not_found"]) {
    assert.ok(model.REFUSAL_CODES.includes(code), code);
  }
});

test("an agent IDE settings file opens as the engine's read-only copy, never the worktree file", () => {
  const files = [
    file({ path: ".vscode/settings.json", status: "added", marks: ["ide_config"] }),
    file({ path: ".vscode/tasks.json", marks: ["ide_config"] }),
    file({ path: ".vscode/old.json", status: "deleted", marks: ["ide_config"] }),
    file({ path: "app/.vscode", status: "added", kind: "symlink", marks: ["symlink", "ide_config"] }),
    file({ path: "shop.code-workspace", status: "added", kind: "binary", marks: ["ide_config"] }),
  ];
  const [group] = model.scmGroups([set({ files })], NOW);
  const shown = Object.fromEntries(group.resources.map((resource) => [resource.path, resource]));
  for (const path of [".vscode/settings.json", ".vscode/tasks.json"]) {
    assert.deepEqual([shown[path].open, shown[path].base, shown[path].config], ["diff", `/${ID}/${path}`, `/${ID}/${path}`], path);
    assert.equal(shown[path].tooltip.endsWith("IDE settings: changes how the IDE behaves in this project"), true, path);
  }
  assert.deepEqual([shown[".vscode/old.json"].open, shown[".vscode/old.json"].config], ["base", null]);
  assert.deepEqual([shown["app/.vscode"].open, shown["app/.vscode"].config], ["none", null]);
  assert.deepEqual([shown["shop.code-workspace"].open, shown["shop.code-workspace"].config], ["none", null]);
  assert.equal(model.CONFIG_SCHEME, "crazyagent-config");
  assert.deepEqual(model.parseBasePath(shown[".vscode/settings.json"].config), { id: ID, path: ".vscode/settings.json" });
  const desktop = readFileSync(new URL("../../desktop/src/screens/changes/model.ts", import.meta.url), "utf8");
  assert.ok(desktop.includes(`  ${model.IDE_CONFIG}: "IDE settings: changes how the IDE behaves in this project",`));
});
