import { act, createElement, useState } from "react";
import { AccountSetting, SubHarnessesSetting } from "../../src/screens/team/setup-rows.tsx";
import { mountTab, settle } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const entryOf = (id, name) => ({ id, name, status: { kind: "ready" }, status_label: "Ready", executable: null, headless: "", tools: { kind: "switchable" }, model_selection: false, account_kinds: [], team: { kind: "available" }, aliases: [] });
const HARNESSES = [entryOf("claude", "Claude Code"), entryOf("codex", "Codex")];
const POOLS = [{ pool: { id: "pool-a", name: "Pool A", members: [] }, members: [] }, { pool: { id: "pool-b", name: "Pool B", members: [] }, members: [] }];
const SETUP = {
  project_id: "chores", name: "Chores", about: "", tech_stack: [], platforms: [], research: [], pool: "pool-a", revision: 4,
  team: { master: "codex", master_tools: true, sub_harnesses: [{ harness: "codex", agents: 2, tools: true }, { harness: "claude", agents: 3, tools: false }], min_agents: 1, max_agents: 6 },
};
const APPLIED = { pending: false, outcome: "applied", rejection: null };
const REFUSED = { pending: false, outcome: "rejected", rejection: "stale_revision" };
let setData = null;
let assigned = [];

const project = () => ({
  id: "chores", name: "Chores", now: 0, setup: { kind: "ok", value: SETUP }, harnesses: { kind: "ok", value: HARNESSES }, pools: { kind: "ok", value: POOLS },
  research: undefined, assigned: POOLS[0], team: undefined, navigate() {}, notify() {}, reload() {}, reloadPools() {}, reloadTeam() {},
});

function Host() {
  const [data, set] = useState(project);
  setData = set;
  return createElement("div", null, createElement(SubHarnessesSetting, { data, prefix: "team" }), createElement(AccountSetting, { data, id: "account" }));
}

/** Answers one engine command: a setup save, and each step of a pool assignment, with the pool
 * records showing the last pool assigned. Inputs: the scene, the command and its arguments.
 * Output: the answer. */
function answer(scene, command, args) {
  if (command === "desktop_update_project_setup") return scene.refused ? REFUSED : APPLIED;
  if (command === "desktop_open_project") return { fresh: true, project_id: "chores" };
  if (command === "desktop_model_access_page") return { writable: true, ticket: "ticket", has_more: false, items: POOLS.map(({ pool }) => ({ kind: "pool", pool: { id: pool.id, projects: assigned.at(-1) === pool.id ? ["chores"] : [] } })) };
  if (command === "desktop_edit_model_access") {
    assigned.push(args.edit.pool);
    return APPLIED;
  }
  throw `Command ${command} not found`;
}

/** The pools the engine was told to give the project, in order. Output: their ids, null for
 * no pool. */
export function assignedPools() {
  return [...assigned];
}

/** Mounts the team rows with project data that only changes when `landRead` is called, as a
 * reload that has not landed yet. Inputs: the scene, saying whether saves are refused, and the
 * theme. Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  assigned = [];
  await mountTab(createElement(Host), (command, args) => answer(scene, command, args), theme);
}

/** Lands the project read the save asked for, one revision on. Output: nothing. */
export async function landRead() {
  await act(async () => {
    setData((old) => ({ ...old, setup: { kind: "ok", value: { ...old.setup.value, revision: old.setup.value.revision + 1 } } }));
  });
  await settle(40);
}

/** Lands a read at the same revision. Output: nothing. */
export async function landSameRevision() {
  await act(async () => {
    setData((old) => ({ ...old, setup: { kind: "ok", value: { ...old.setup.value } } }));
  });
  await settle(40);
}

/** Changes a select as the owner's pick does. Inputs: its id and the value. Output: nothing. */
export async function choose(id, value) {
  await act(async () => {
    const select = document.getElementById(id);
    const set = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set;
    set.call(select, value);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle(80);
}
