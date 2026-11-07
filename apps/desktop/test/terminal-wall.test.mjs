import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { wallPages } = await import("../src/screens/terminals/pages.ts");

/** A graph node for the wall's page math. Inputs: the id, the kind and whether it has a
 * terminal. Output: the node. */
function node(id, kind = "sub", terminal = true) {
  return { id, kind, parent: kind === "master" ? null : "master", label: id, harness: "codex", status: null, status_label: null, account: null, terminal: terminal ? `team-${id}` : null };
}

/** A team in graph order with the master third, as a graph may list it. Input: how many
 * agents besides the master. Output: the nodes. */
function team(agents) {
  const subs = Array.from({ length: agents }, (_, index) => node(`a${index + 1}`));
  return [...subs.slice(0, 2), node("m", "master"), ...subs.slice(2)];
}

const ids = (pages) => pages.map((page) => page.map((item) => item.id));

test("13 terminals at 6 per window take three windows of 6, 6 and 1, the master first and the agents in graph order", () => {
  const pages = wallPages(team(12), 6);
  assert.deepEqual(pages.map((page) => page.length), [6, 6, 1]);
  assert.deepEqual(ids(pages).flat(), ["m", ...Array.from({ length: 12 }, (_, index) => `a${index + 1}`)]);
});

test("a 14th terminal fills the last window, and a 19th opens a fourth", () => {
  assert.deepEqual(wallPages(team(13), 6).map((page) => page.length), [6, 6, 2]);
  assert.deepEqual(wallPages(team(18), 6).map((page) => page.length), [6, 6, 6, 1]);
});

test("each choice fills its windows to 4, 6 or 9", () => {
  assert.deepEqual([4, 6, 9].map((per) => wallPages(team(7), per).map((page) => page.length)), [[4, 4], [6, 2], [8]]);
  assert.deepEqual(wallPages(team(15), 9).map((page) => page.length), [9, 7]);
});

test("agents without a terminal take no tile, and a team with no terminal opens no window", () => {
  const nodes = [node("m", "master"), node("a1", "sub", false), node("a2"), node("lead", "lead", false)];
  assert.deepEqual(ids(wallPages(nodes, 4)), [["m", "a2"]]);
  assert.deepEqual(wallPages([node("m", "master", false)], 6), []);
  assert.deepEqual(wallPages([], 6), []);
});
