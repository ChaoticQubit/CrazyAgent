import assert from "node:assert/strict";
import test from "node:test";

const model = await import("../src/sidebar/model.ts");

const NOW = Date.UTC(2026, 9, 27, 14, 0);
const memory = (id, state, extra = {}) => ({ id, revision: 1, text: `fact ${id}`, state, reviewed: false, ...extra });
const agent = (id, role, status, extra = {}) => ({
  id, label: role === "master" ? "Master" : `Agent ${id}`, role, harness: "codex", status, status_label: status,
  worktree: null, message: role === "sub" && status !== "stopped" && status !== "failed", ...extra,
});
const change = (id, state, extra = {}) => ({
  id: id.repeat(32), agent: "Ada (@builder)", role: "sub", title: `Change ${id}`, state, waiting: false,
  files: [{ path: "README.md" }], restorable_until: null, ...extra,
});
const labels = (rows) => rows.map((row) => [row.label, row.description, row.children.map((child) => child.label)]);

test("memories group as waiting, approved and set aside in the engine's order, and empty groups are left out", () => {
  const rows = model.memoryGroups([
    memory("a", "approved", { revision: 2, reviewed: true }),
    memory("b", "pending"),
    memory("c", "rejected", { revision: 2, reviewed: true }),
    memory("d", "pending"),
    memory("e", "retracted"),
    memory("f", "removed"),
  ]);
  assert.deepEqual(labels(rows), [
    ["Waiting for you", "2", ["fact b", "fact d"]],
    ["Approved", "1", ["fact a"]],
    ["Set aside", "3", ["fact c", "fact e", "fact f"]],
  ]);
  assert.deepEqual(rows[0].children.map((row) => [row.kind, row.description, row.memory.id]), [
    ["memory.pending", "revision 1", "b"],
    ["memory.pending", "revision 1", "d"],
  ]);
  assert.equal(rows[1].children[0].kind, "memory.approved");
  assert.deepEqual(labels(model.memoryGroups([memory("a", "approved")])), [["Approved", "1", ["fact a"]]]);
  assert.deepEqual(model.memoryGroups([]), []);
});

test("a memory shows on one plain line, cut to the label limit, with no control or direction characters", () => {
  const long = "word ".repeat(40);
  const [group] = model.memoryGroups([
    memory("a", "pending", { text: "first\nsecond\tthird‮\u0007 end" }),
    memory("b", "pending", { text: long }),
  ]);
  assert.equal(group.children[0].label, "first second third end");
  const cut = group.children[1].label;
  assert.equal([...cut].length, model.LABEL_CHARS_MAX);
  assert.ok(cut.endsWith("..."));
  assert.equal(group.children[1].memory.text, long, "the memory itself is not changed");
});

test("agents group as master, running and stopped, and each kind names the actions the agent offers", () => {
  const rows = model.agentGroups([
    agent(1, "master", "idle", { worktree: "/work/project" }),
    agent(2, "sub", "working", { worktree: "/work/agents/2" }),
    agent(3, "sub", "stopped", { worktree: "/work/agents/3" }),
    agent(4, "sub", "starting"),
    agent(5, "sub", "failed"),
  ]);
  assert.deepEqual(labels(rows), [
    ["Master", "1", ["Master"]],
    ["Running", "2", ["Agent 2", "Agent 4"]],
    ["Stopped", "2", ["Agent 3", "Agent 5"]],
  ]);
  const kinds = rows.flatMap((row) => row.children.map((child) => [child.agent.id, child.kind, child.description]));
  assert.deepEqual(kinds, [
    [1, "agent agent.worktree", "idle"],
    [2, "agent agent.message agent.worktree", "working"],
    [4, "agent agent.message", "starting"],
    [3, "agent agent.worktree", "stopped"],
    [5, "agent", "failed"],
  ]);
  assert.deepEqual(model.agentGroups([agent(2, "sub", "idle", { message: false })]).map((row) => row.children[0].kind), ["agent"]);
});

test("change sets show only the ones with a Source Control group, named the way that group is", () => {
  const rows = model.changeGroups([
    change("a", "pending", { files: [{ path: "a" }, { path: "b" }] }),
    change("b", "accepted"),
    change("c", "conflict"),
    change("d", "discarded", { restorable_until: NOW + 60_000 }),
    change("e", "discarded", { restorable_until: NOW - 1 }),
    change("f", "updating", { title: "  " }),
    change("g", "discarded"),
  ], NOW);
  assert.deepEqual(labels(rows), [
    ["Open", "3", ["Ada (@builder): Change a", "Ada (@builder): Change c", "Ada (@builder): Changes"]],
    ["Can restore", "1", ["Ada (@builder): Change d"]],
  ]);
  assert.deepEqual(rows[0].children.map((row) => [row.kind, row.description, row.change.id]), [
    ["change", "2 files", "a".repeat(32)],
    ["change", "1 file", "c".repeat(32)],
    ["change", "1 file", "f".repeat(32)],
  ]);
  assert.deepEqual(model.changeGroups([change("b", "accepted")], NOW), []);
});

test("a long list shows the first rows of a group and says how many there are", () => {
  const many = Array.from({ length: model.ROWS_PER_GROUP_MAX + 5 }, (_, index) => memory(`m${index}`, "pending"));
  const [group] = model.memoryGroups(many);
  assert.equal(group.children.length, model.ROWS_PER_GROUP_MAX);
  assert.equal(group.description, `${model.ROWS_PER_GROUP_MAX} of ${model.ROWS_PER_GROUP_MAX + 5}`);
});

test("context links are the brief, then documents and research by name, inside the context folder only", () => {
  const rows = model.contextLinks(
    "/work/project/.crazyagent/context/",
    ["spec.md", ".hidden", "notes.txt", "a/b", "bad\nname", ""],
    ["market.md"],
  );
  assert.deepEqual(labels(rows), [
    ["Brief", "brief.md", []],
    ["Documents", "2", ["notes.txt", "spec.md"]],
    ["Research", "1", ["market.md"]],
  ]);
  assert.deepEqual(rows.flatMap((row) => [row.path, ...row.children.map((child) => child.path)]).filter(Boolean), [
    "/work/project/.crazyagent/context/brief.md",
    "/work/project/.crazyagent/context/documents/notes.txt",
    "/work/project/.crazyagent/context/documents/spec.md",
    "/work/project/.crazyagent/context/research/market.md",
  ]);
  assert.deepEqual(labels(model.contextLinks("/work/project/context", null, [])), [["Brief", "brief.md", []]]);
});

test("the context folder is the project's .crazyagent folder when it is a real folder, else the project folder", () => {
  assert.equal(model.contextFolder("/work/project", { directory: true, symlink: false }), "/work/project/.crazyagent/context");
  assert.equal(model.contextFolder("/work/project/", { directory: true, symlink: true }), "/work/project/context");
  assert.equal(model.contextFolder("/work/project", { directory: false, symlink: false }), "/work/project/context");
  assert.equal(model.contextFolder("/work/project", null), "/work/project/context");
});

test("memory pages continue after the last id shown until a page comes back empty or the page limit", () => {
  assert.equal(model.nextPage([memory("a", "pending"), memory("b", "pending")], 1), "b");
  assert.equal(model.nextPage([], 1), null);
  assert.equal(model.nextPage([memory("a", "pending")], model.MEMORY_PAGES_MAX), null);
});
