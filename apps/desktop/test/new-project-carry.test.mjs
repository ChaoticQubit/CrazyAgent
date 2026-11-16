import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const carry = await import("../src/screens/new-project/carry.ts");
const intake = await import("../src/screens/intake/intake.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const NAME = "Notes/2026 plans";
const doneJob = (id) => ({ id, kind: "setup", project_id: "notes", source: NAME, mode: "move", overrides: [], stopping: false, state: { state: "done" } });
const started = (extra = {}) => ({
  id: "setup-1", project: "notes", draft: {}, name: "Notes", pool: "pool-a", poolLabel: "Team pool", access: "own_branch", research: 2, ...extra,
});
const VIEW = {
  version: 4,
  access: "sandboxed",
  network: "open",
  levels: [],
  networks: [],
  toolchains: [{ toolchain: "rust", label: "Rust", installed: true, used: true, allowed: true }],
  ide_git_hooks: false,
  tool_cache: "shared",
  tool_caches: [],
  home_caches: "clear_when_raised",
  home_cache_choices: [],
  ide_agent_files: "allow_all",
  shared_folders: { scope: "all", access: "read_only", chosen: [], secrets: [] },
  local_folders: [],
};

function api({ assigned = "applied", gate } = {}) {
  const calls = { assign: [], read: [], save: [] };
  const deps = {
    assign: async (project, pool, generation) => {
      calls.assign.push([project, pool, generation]);
      if (gate) await gate;
      return { kind: "ok", value: assigned === "applied" ? { kind: "applied" } : { kind: "rejected", code: assigned } };
    },
    read: async (project) => {
      calls.read.push(project);
      return { kind: "ok", value: VIEW };
    },
    save: async (project, policy, version, generation) => {
      calls.save.push([project, policy.access, version, generation]);
      return { kind: "ok", value: { pending: false, outcome: "applied", rejection: null } };
    },
    generation: () => 7,
  };
  return { calls, deps };
}

test("a New Project done with its page closed assigns the pool once and saves a non-Sandboxed agent access once (DEC-177 add.2 Q1 B)", async () => {
  const { calls, deps } = api();
  carry.keepStarted(started());
  const carried = carry.carryFollowUps("setup-1", "notes", deps);
  assert.ok(carried, "the job started here carries");
  assert.equal(await carried.outcome, "Notes created. 2 research agents are starting.");
  assert.deepEqual(calls.assign, [["notes", "pool-a", 7]]);
  assert.deepEqual(calls.read, ["notes"]);
  assert.deepEqual(calls.save, [["notes", "own_branch", 4, 7]]);
});

test("a Sandboxed New Project saves no agent access", async () => {
  const { calls, deps } = api();
  carry.keepStarted(started({ access: "sandboxed" }));
  const carried = carry.carryFollowUps("setup-1", "notes", deps);
  assert.ok(carried);
  await carried.outcome;
  assert.equal(calls.assign.length, 1);
  assert.deepEqual(calls.read, []);
  assert.deepEqual(calls.save, []);
});

test("a failed pool assign answers the Settings sentence, and Home's Done line keeps it", async () => {
  const { deps } = api({ assigned: "pool_missing" });
  carry.keepStarted(started({ access: "sandboxed" }));
  const carried = carry.carryFollowUps("setup-1", "notes", deps);
  assert.ok(carried);
  const line = "Notes created. Couldn't assign Team pool. Choose it again in the project's Settings, Pool.";
  assert.equal(await carried.outcome, line);
  assert.equal(intake.jobLine(doneJob("setup-1")), line);
});

test("a job not started here carries nothing and Home's Done line still names Settings", async () => {
  const { calls, deps } = api();
  carry.keepStarted(started({ id: "setup-2" }));
  assert.equal(carry.carryFollowUps("setup-9", "other", deps), null);
  assert.ok(carry.carryFollowUps("setup-2", "notes", deps), "the job started here carries");
  carry.dropStarted();
  assert.equal(carry.carryFollowUps("setup-3", "notes", deps), null);
  assert.equal(calls.assign.length, 1);
  assert.equal(intake.jobLine(doneJob("setup-9")), `${NAME} is set up. Choose its pool and agent access in its Settings.`);
});

test("a second poll of the same end runs nothing again and answers the same sentence", async () => {
  const { calls, deps } = api();
  carry.keepStarted(started());
  const first = carry.carryFollowUps("setup-1", "notes", deps);
  const second = carry.carryFollowUps("setup-1", "notes", deps);
  assert.ok(first && second);
  assert.equal(second.outcome, first.outcome, "both callers share one run");
  await second.outcome;
  assert.equal(calls.assign.length, 1);
  assert.equal(calls.save.length, 1);
  assert.equal(intake.jobLine(doneJob("setup-1")), "Notes created. 2 research agents are starting.", "carried names no Settings");
});

test("the page letting go after the pool assign starts still saves the agent access", async () => {
  let open;
  const gate = new Promise((resolve) => { open = resolve; });
  const { calls, deps } = api({ gate });
  carry.keepStarted(started());
  const carried = carry.carryFollowUps("setup-1", "notes", deps);
  assert.ok(carried);
  carry.dropStarted();
  assert.equal(carry.startedJob(), null);
  open();
  await carried.outcome;
  assert.equal(calls.save.length, 1, "the access is saved with no page open");
  const wizard = read("src/screens/new-project/index.tsx");
  assert.ok(!/&& mounted\.current\)/.test(wizard), "no follow-up step waits on the page staying open");
  assert.ok(wizard.includes("carryFollowUps(following, follow.project)"), "the page's Done runs the shared follow-ups");
  const state = read("src/state.ts");
  assert.match(state, /carryFollowUps\(result\.value\.id, result\.value\.project_id\)/, "Home's poll runs them for a page closed");
});
