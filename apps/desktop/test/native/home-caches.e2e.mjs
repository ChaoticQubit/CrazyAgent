import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { TASK, handOff, onEngine } from "./codex-task-engine.mjs";

/** Every agent home caches choice the owner can save (DEC-173 add.1), with the wakes its row
 * runs after the first hand-off: the level the project is saved at before each wake and
 * whether the caches planted in the agent's home stay through it. The project starts
 * Sandboxed, so Own branch is a raise. */
const OPTIONS = [
  { choice: "keep", label: "Keep", wakes: [{ access: "own_branch", kept: true }] },
  { choice: "clear_when_raised", label: "Clear when access is raised", wakes: [{ access: "sandboxed", kept: true }, { access: "own_branch", kept: false }] },
  { choice: "clear_every_launch", label: "Clear every launch", wakes: [{ access: "sandboxed", kept: false }] },
];

/** The files a tool run would leave in an agent home's two download caches. */
const CACHED = [".cache/e2e/x", ".npm/_cacache/index-v5/x"];

/** Reads the team agent that ran the owner's task and its status. Inputs: the engine root
 * and the project. Output: { agent, status }, or null before the task has an agent. */
function taskAgent(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const row = store.prepare("SELECT a.id AS agent, a.status AS status FROM team_tasks t JOIN team_agents a ON a.id=t.agent WHERE t.project=? AND t.prompt=?").get(project, TASK);
    return row ? { agent: Number(row.agent), status: row.status } : null;
  } finally {
    store.close();
  }
}

/** Names where the engine records the level of an agent home's last launch: the SHA-256 of
 * the home's real path in the engine root's agent-home-levels folder, never in the home.
 * Inputs: the engine root and the home. Output: the record's path. */
function levelRecord(runtime, home) {
  return join(runtime, "agent-home-levels", createHash("sha256").update(realpathSync(home)).digest("hex"));
}

/** Writes a file into both download caches of an agent home, as a tool run there would.
 * Input: the home. */
function plant(home) {
  for (const file of CACHED) {
    mkdirSync(dirname(join(home, file)), { recursive: true, mode: 0o700 });
    writeFileSync(join(home, file), "cached\n", { mode: 0o600 });
  }
}

/** Tells which planted cache files are still in an agent home. Input: the home. Output: one
 * flag per file in CACHED. */
function cached(home) {
  return CACHED.map((file) => existsSync(join(home, file)));
}

/** Saves the project's level through the engine, keeping every other saved choice, the
 * caches choice included. Inputs: the bridge, the project, the level and the write
 * generation. Output: none. */
async function saveLevel(bridge, project, access, generation) {
  const view = await value(bridge, "desktop_project_agent_policy", { id: project });
  if (view.access === access) return;
  const policy = {
    access,
    network: view.network,
    toolchains: [],
    ide_git_hooks: view.ide_git_hooks,
    tool_cache: view.tool_cache,
    home_caches: view.home_caches,
    ide_agent_files: view.ide_agent_files,
    apply_changes: view.apply_changes,
    grants: (view.grants ?? []).map((row) => row.grant),
    shared_folders: view.shared_folders,
  };
  const saved = await value(bridge, "desktop_set_project_agent_policy", { id: project, version: view.version, policy, generation });
  assert.equal(saved.outcome, "applied", `the project runs at ${access}: ${JSON.stringify(saved)}`);
  const kept = await value(bridge, "desktop_project_agent_policy", { id: project });
  assert.equal(kept.access, access);
  assert.equal(kept.home_caches, view.home_caches, "a level save keeps the caches choice");
}

/** Stops the agent and waits until the engine has it stopped. Inputs: the bridge, the engine
 * root, the project, the agent and the write generation. Output: none. */
async function stopAgent(bridge, runtime, project, agent, generation) {
  const receipt = await value(bridge, "desktop_team_stop_agent", { id: project, agent, generation });
  assert.equal(receipt.outcome, "applied", `the owner stops the agent: ${JSON.stringify(receipt)}`);
  for (const deadline = Date.now() + STEP_MS; taskAgent(runtime, project)?.status !== "stopped"; await pause(POLL_MS * 5)) {
    assert.ok(Date.now() < deadline, `the agent never stopped: ${JSON.stringify(taskAgent(runtime, project))}`);
  }
}

/** Wakes the stopped agent and waits until its launch rewrote its home's level record, which
 * the launch writes after the reset. Inputs: the bridge, the project, the agent, the record,
 * the level the launch runs at and the write generation. Output: none. */
async function wakeAgent(bridge, project, agent, record, access, generation) {
  const before = statSync(record).ino;
  const receipt = await value(bridge, "desktop_team_wake_agent", { id: project, agent, generation });
  assert.equal(receipt.outcome, "applied", `the owner wakes the agent: ${JSON.stringify(receipt)}`);
  for (const deadline = Date.now() + STEP_MS * 4; ; await pause(POLL_MS * 5)) {
    if (existsSync(record) && statSync(record).ino !== before) break;
    assert.ok(Date.now() < deadline, "the woken agent's launch never recorded its level");
  }
  assert.equal(readFileSync(record, "utf8"), access, "the launch recorded the level it ran at");
}

for (const option of OPTIONS) {
  test(`the agent home caches choice ${option.label} saves from Agent access and decides what a woken agent's home keeps`, { timeout: 900000 }, (t) =>
    onEngine(t, `ca-home-caches-${option.choice}-`, async ({ project, runtime, me, page, site, bridge }) => {
      let generation = 100;
      const before = await value(bridge, "desktop_project_agent_policy", { id: project });
      assert.equal(before.home_caches, "clear_when_raised", "a project starts on Clear when access is raised");
      assert.deepEqual(before.home_cache_choices.map((row) => [row.choice, row.label]), OPTIONS.map((row) => [row.choice, row.label]), "the engine lists every choice");
      await saveLevel(bridge, project, "sandboxed", ++generation);
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=access` });
      await me.waitFor("the agent home caches choice in Agent access", (text) => text.includes("Tool caches in agent homes") && text.includes(option.label));
      if (option.choice !== before.home_caches) {
        await me.click(option.label, "label.pj-radio-card strong");
        await me.click("Save agent access", "button");
      }
      for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
        const saved = await value(bridge, "desktop_project_agent_policy", { id: project });
        if (saved.home_caches === option.choice) {
          assert.equal(saved.access, "sandboxed", "the save from Agent access keeps the level");
          break;
        }
        assert.ok(Date.now() < deadline, `the engine never kept ${option.choice}: ${JSON.stringify(saved)}`);
      }
      await me.capture(`home-caches-${option.choice}`);
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.unfold("Agents and tasks");
      await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
      assert.equal(await handOff(me, page, project, runtime), "done", "the Codex sub-agent finished the task");
      const ran = taskAgent(runtime, project);
      assert.ok(ran, "the task names the agent that ran it");
      const home = join(runtime, "team", "agents", `agent-${ran.agent}`, "home");
      assert.ok(existsSync(home), `the agent ran in its own home: ${home}`);
      const record = levelRecord(runtime, home);
      assert.equal(readFileSync(record, "utf8"), "sandboxed", "the first launch recorded Sandboxed outside the home");
      assert.ok(!record.startsWith(home), "the level record is not in the home");
      for (const wake of option.wakes) {
        await stopAgent(bridge, runtime, project, ran.agent, ++generation);
        plant(home);
        await saveLevel(bridge, project, wake.access, ++generation);
        await wakeAgent(bridge, project, ran.agent, record, wake.access, ++generation);
        assert.deepEqual(cached(home), [wake.kept, wake.kept], `${option.label}, woken at ${wake.access}: both caches ${wake.kept ? "stay" : "go"}`);
      }
    }));
}
