import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS, value } from "./e2e-harness.mjs";

const LEASE_MS = 180000;
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const LEASE_BRANCH = /^crazyagent\/(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const PROMPT = "Say hello.";

/** Reads, read-only, the worktree leases the engine keeps for one project. Inputs: the
 * engine root and the project. Output: the leases as the engine stored them. */
function leases(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    return store.prepare("SELECT body FROM worktree_leases").all()
      .map((row) => JSON.parse(row.body).lease)
      .filter((lease) => lease.project_id === project);
  } finally {
    store.close();
  }
}

/** Lets the project's master use the test pool, as the owner does on the Models page: the
 * pool serves the project and the project leader role. Inputs: the bridge, the project and
 * the next generation. Output: none. */
async function poolForMaster(bridge, project, next) {
  const basis = await value(bridge, "desktop_inspect_model_access", { collection: "pools", id: "local-pool" });
  const now = basis.record.pool;
  const pool = {
    ...now,
    projects: now.projects.includes(project) ? now.projects : [...now.projects, project],
    roles: now.roles.includes("project_leader") ? now.roles : [...now.roles, "project_leader"],
  };
  if (JSON.stringify(pool) === JSON.stringify(now)) return;
  const saved = await value(bridge, "desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: next() });
  assert.equal(saved.outcome, "applied", `the pool serves the project's master: ${JSON.stringify(saved)}`);
}

/** Has the engine lease and check out the project's master worktree the way it does for
 * every master turn: opens the master, sends one prompt, waits with a deadline for the
 * engine's Ready lease and for the turn to end (it fails at the test's closed model port),
 * then checks the lease is still the engine's. Inputs: the bridge, the engine root, the
 * project and the next generation. Output: the worktree, its git folder and its branch. */
export async function engineMasterWorktree(bridge, runtime, project, next) {
  await poolForMaster(bridge, project, next);
  const { ticket } = await value(bridge, "desktop_open_project", { id: project });
  let master = await value(bridge, "desktop_master_state", { ticket });
  if (!master.session || master.session.phase !== "configured") {
    const opened = await value(bridge, "desktop_open_master", { ticket, generation: next() });
    assert.equal(opened.outcome, "applied", `the master opens: ${JSON.stringify(opened)}`);
    master = await value(bridge, "desktop_master_state", { ticket });
  }
  assert.ok(master.session, `the master has a session: ${JSON.stringify(master)}`);
  const sessionId = master.session.session_id;
  const sent = await value(bridge, "desktop_submit_master", { ticket, sessionId, text: PROMPT, generation: next() });
  assert.equal(sent.outcome, "applied", `the prompt reaches the master: ${JSON.stringify(sent)}`);
  let lease;
  for (const deadline = Date.now() + LEASE_MS; ; await pause(POLL_MS)) {
    lease = leases(runtime, project).find((item) => item.state === "Ready");
    if (lease) break;
    assert.ok(Date.now() < deadline, `the engine never made a Ready master lease: ${JSON.stringify(leases(runtime, project))}`);
  }
  for (const deadline = Date.now() + LEASE_MS; ; await pause(POLL_MS)) {
    const turn = (await value(bridge, "desktop_master_turns", { ticket, sessionId, cursor: null })).page.turns.find((item) => item.id === sent.operation_id);
    if (turn && !ACTIVE_TURN.has(turn.phase)) break;
    assert.ok(Date.now() < deadline, `the master turn never ended: ${JSON.stringify(turn ?? null)}`);
  }
  const kept = leases(runtime, project).find((item) => item.path === lease.path);
  assert.ok(kept?.state === "Ready", `the failed master turn removed or changed the engine's lease: ${JSON.stringify(kept ?? null)}`);
  assert.match(kept.branch, LEASE_BRANCH, "the engine named the master's branch");
  assert.equal(kept.task_id, lease.task_id, "the lease is the one the engine made for this master");
  const link = readFileSync(join(kept.path, ".git"), "utf8").match(/^gitdir: (.+)\n$/);
  assert.ok(link, "the master worktree is a linked worktree");
  assert.equal(readFileSync(join(link[1], "HEAD"), "utf8"), `ref: refs/heads/${kept.branch}\n`, "the master worktree is on the engine's branch");
  return { worktree: kept.path, gitDir: link[1], branch: kept.branch };
}
