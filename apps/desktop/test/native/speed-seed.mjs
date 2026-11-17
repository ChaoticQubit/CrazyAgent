import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS, STEP_MS, bridgeCall, closedPort, setUpProject, start, stop, value } from "./e2e-harness.mjs";
import { poolForMaster, readyForAgents } from "./master-lease.mjs";
import { privateFolder } from "./speed-budget.mjs";

const SOCKET_PATH_MAX = 100;

/** Master sessions the big-project row opens the project with (DEC-169 Q5). */
export const SESSIONS = 50;
/** Calls per command in one round-trip reading (row T5). */
export const ROUND_TRIPS = 100;
/** The project the speed rows walk. */
export const PROJECT = "Speed Project";
/** Leftover project ids the engine start row seeds: each has a note and no live project or
 * removal record, as a build without the removal record leaves them (DEC-178, leader 01:11). */
export const LEFTOVER_IDS = 1000;
const LEFTOVER_PREFIX = "speed-left-";
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const SESSIONS_MAX = 200;
const BUSY_MS = 5000;

/** Readies a kept engine state, while its engine is stopped, so the next start runs the held-id
 * backfill over `LEFTOVER_IDS` leftover ids: on a build with the removal record it clears the
 * backfill's done mark and the seeded ids the last start held, and keeps one note per seeded
 * id. A build without the record (the base) is left as it is. Input: the engine folder.
 * Output: how many leftover ids it seeded, 0 on a build without the record. */
function seedLeftovers(runtime) {
  const path = join(runtime, "engine.db");
  assert.ok(existsSync(path), `a seeded engine state has its database: ${path}`);
  const store = new DatabaseSync(path);
  try {
    store.exec(`PRAGMA busy_timeout = ${BUSY_MS}`);
    const marked = store.prepare("SELECT 1 AS one FROM sqlite_schema WHERE type='table' AND name='projects_removed_backfill'").get();
    if (!marked) return 0;
    const like = `${LEFTOVER_PREFIX}%`;
    store.exec("BEGIN IMMEDIATE");
    try {
      store.exec("DELETE FROM projects_removed_backfill");
      store.prepare("DELETE FROM projects_removed WHERE project LIKE ?").run(like);
      store.prepare(`WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?)
        INSERT OR IGNORE INTO memory_library(id,project,origin,text)
        SELECT ?||i||'-note', ?||i, '{"kind":"written"}', 'A note a removed project left.' FROM n`)
        .run(LEFTOVER_IDS, LEFTOVER_PREFIX, LEFTOVER_PREFIX);
      store.exec("COMMIT");
    } catch (error) {
      store.exec("ROLLBACK");
      throw error;
    }
    const ids = Number(store.prepare("SELECT count(DISTINCT project) AS n FROM memory_library WHERE project LIKE ?").get(like).n);
    assert.equal(ids, LEFTOVER_IDS, "every seeded leftover id has its note");
    assert.equal(Number(store.prepare("SELECT count(*) AS n FROM projects_removed WHERE project LIKE ?").get(like).n), 0,
      "no seeded id is held before the start");
    store.exec("PRAGMA wal_checkpoint(TRUNCATE)");
    return ids;
  } finally {
    store.close();
  }
}

/** How many seeded leftover ids the running engine's backfill held at this start, read from
 * its database without writing. Input: the engine folder. Output: the count. */
function heldLeftovers(runtime) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec(`PRAGMA busy_timeout = ${BUSY_MS}`);
    return Number(store.prepare("SELECT count(*) AS n FROM projects_removed WHERE project LIKE ?").get(`${LEFTOVER_PREFIX}%`).n);
  } finally {
    store.close();
  }
}

/** The engine's global settings for a speed side: one key-less RCLI connection to a local
 * port that refuses, in one pool, so a master opens and its turns end without a model.
 * Input: the closed port. Output: the TOML text. */
export function speedSettings(port) {
  assert.ok(Number.isInteger(port) && port > 0 && port < 65536, `a port: ${port}`);
  return `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${port}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`;
}

/** Waits, bounded, until a master turn is no longer active. Inputs: the bridge, the ticket,
 * the session and the turn id. Output: none. */
async function turnEnded(bridge, ticket, sessionId, turnId) {
  assert.ok(ticket && sessionId && turnId, "a turn to wait on");
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const turns = (await value(bridge, "desktop_master_turns", { ticket, sessionId, cursor: null })).page.turns;
    const turn = turns.find((item) => item.id === turnId);
    if (turn && !ACTIVE_TURN.has(turn.phase)) return;
    assert.ok(Date.now() < deadline, `the master turn never ended: ${JSON.stringify(turn ?? null)}`);
  }
}

/** Makes a speed side's owner state through the real engine, as the owner's app would: the
 * profile, the projects folder, one project whose master is the key-less RCLI connection, and
 * `count` master sessions, each with one message, made with New session. Inputs: the bridge,
 * the projects folder and the session count. Output: the project id and its session count. */
export async function seedSide(bridge, projects, count = SESSIONS) {
  assert.ok(Number.isInteger(count) && count >= 1 && count <= SESSIONS_MAX, `1 to ${SESSIONS_MAX} sessions`);
  let generation = 0;
  const next = () => { generation += 1; return generation; };
  const applied = (what, receipt) => assert.equal(receipt?.outcome, "applied", `${what}: ${JSON.stringify(receipt)}`);
  assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects");
  applied("the profile is saved", await value(bridge, "desktop_save_owner_profile", { profile: { name: "Speed Owner" }, generation: next() }));
  applied("the projects folder is set", await value(bridge, "desktop_set_projects_home", { path: projects, generation: next() }));
  const project = await setUpProject(bridge, { name: PROJECT, repositories: [] }, {
    about: "The project the speed rows open", tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [],
  });
  await poolForMaster(bridge, project, next);
  await readyForAgents(bridge, next);
  const { ticket } = await value(bridge, "desktop_open_project", { id: project });
  if ((await value(bridge, "desktop_master_state", { ticket })).session?.phase !== "configured") {
    applied("the master opens", await value(bridge, "desktop_open_master", { ticket, generation: next() }));
  }
  for (let index = 0; index < count; index += 1) {
    const live = (await value(bridge, "desktop_master_state", { ticket })).session;
    assert.equal(live?.phase, "configured", `session ${index + 1} is live: ${JSON.stringify(live)}`);
    const sent = await value(bridge, "desktop_submit_master", { ticket, sessionId: live.session_id, text: `Speed note ${index + 1}`, generation: next() });
    applied(`message ${index + 1} reaches the master`, sent);
    await turnEnded(bridge, ticket, live.session_id, sent.operation_id);
    if (index + 1 < count) applied(`session ${index + 2} opens`, await value(bridge, "desktop_new_master_session", { ticket, cancelRunning: false, generation: next() }));
  }
  const listed = (await value(bridge, "desktop_master_sessions", { ticket })).sessions.length;
  assert.ok(listed >= count, `the engine lists the ${count} sessions: ${listed}`);
  return { project, sessions: listed };
}

/** Times bridge round trips of read commands, one call at a time. Inputs: the bridge and the
 * calls `[{ cmd, args }]`. Output: `{ cmd: [ms, ...] }` with `ROUND_TRIPS` samples each. */
export async function roundTrips(bridge, calls) {
  assert.ok(calls.length >= 1 && calls.length <= 8, "1 to 8 commands");
  const times = {};
  for (const { cmd, args } of calls) {
    times[cmd] = [];
    for (let index = 0; index < ROUND_TRIPS; index += 1) {
      const asked = performance.now();
      const answer = await bridgeCall(bridge, cmd, args);
      times[cmd].push(performance.now() - asked);
      assert.ok("value" in answer, `${cmd} answered ${JSON.stringify(answer).slice(0, 300)}`);
    }
    assert.equal(times[cmd].length, ROUND_TRIPS);
  }
  return times;
}

/** The reads row T5 times: the project and master reads the project page makes on open, the
 * master reads on the ticket first and the project open last, as every open renews the
 * selection and leaves the ticket stale for a read after it. Inputs: the seeded side and the
 * project ticket. Output: the calls, in the order they are timed. */
export function speedReads(seeded, ticket) {
  assert.match(seeded.project, /\S/, "a seeded project");
  assert.match(ticket, /\S/, "a project ticket");
  return [
    { cmd: "desktop_master_state", args: { ticket } },
    { cmd: "desktop_master_sessions", args: { ticket } },
    { cmd: "desktop_open_project", args: { id: seeded.project } },
  ];
}

/** The speed suite's side, from the speed driver's environment: `SPEED_SIDE`, a folder
 * build-speed-base.sh made, and `SPEED_STATE`, the side's kept engine state, made here with
 * its settings on first use and, on every later use, readied with leftover ids for the engine
 * start row. A state with an engine but no seeded.json is a broken seeding and is refused, as
 * is a state, engine or projects folder that is not this user's private folder
 * ([`privateFolder`]). Input: the app run.mjs was given. Output: the side's paths. */
export async function speedSide(app) {
  const folder = process.env.SPEED_SIDE;
  const state = process.env.SPEED_STATE;
  assert.ok(folder && isAbsolute(folder) && existsSync(join(folder, "bin")), "SPEED_SIDE names a speed artifact folder");
  assert.ok(state && isAbsolute(state), "SPEED_STATE names the side's kept state folder");
  privateFolder(state);
  assert.equal(app, join(folder, "bin", "crazyagent-desktop"), "the app is the side's own build");
  const side = { folder, state, engine: join(folder, "bin", "crazyagent"), bridge: join(folder, "bin", "desktop-e2e-bridge"),
    runtime: join(state, "engine"), projects: join(state, "projects"), seeded: join(state, "seeded.json") };
  assert.ok(join(side.runtime, "engine.sock").length < SOCKET_PATH_MAX, `the engine socket path fits a socket address: ${side.runtime}`);
  const fresh = !existsSync(side.runtime) || readdirSync(side.runtime).length === 0;
  assert.ok(fresh || existsSync(side.seeded), `${state} holds an engine that was never seeded; remove it and run again`);
  for (const folderMade of [side.runtime, side.projects]) {
    mkdirSync(folderMade, { recursive: true, mode: 0o700 });
    privateFolder(folderMade);
  }
  if (fresh) writeFileSync(join(side.runtime, "global.toml"), speedSettings(await closedPort()), { flag: "wx", mode: 0o600 });
  side.leftovers = fresh ? 0 : seedLeftovers(side.runtime);
  return side;
}

/** Times one onboarding start of a new folder in the projects folder, the step that picks the
 * folder's project id past held ids and leftover rows (rv-code4 r2 F2), then removes that
 * project from the dashboard and deletes its folder, so the app's rows read the same projects
 * and found folders every run. Inputs: the bridge, the projects folder and the first client
 * generation free. Output: the ms from asking to the engine's receipt. */
async function onboardStart(bridge, projects, generation) {
  assert.ok(Number.isSafeInteger(generation) && generation >= 1, "a client generation");
  const name = `speed-onboard-${Date.now()}`;
  const folder = join(projects, name);
  mkdirSync(folder, { mode: 0o700 });
  const asked = performance.now();
  const started = await value(bridge, "desktop_start_onboarding", { path: folder, pool: "local-pool", generation });
  const ms = performance.now() - asked;
  assert.equal(started?.outcome, "applied", `onboarding starts: ${JSON.stringify(started)}`);
  assert.equal(started.project_id, name, "the new folder gets its own name's id");
  const removed = await value(bridge, "desktop_remove_project", { id: started.project_id, generation: generation + 1 });
  assert.equal(removed?.outcome, "applied", `the timed project leaves the dashboard: ${JSON.stringify(removed)}`);
  rmSync(folder, { recursive: true, force: true });
  assert.ok(!existsSync(folder), "the timed folder is gone");
  assert.ok(Number.isFinite(ms) && ms >= 0, "an onboarding start time");
  return ms;
}

/** Readies a speed side once its engine runs: checks that a start over seeded leftover ids
 * held every one, seeds the side on first use, times the project page's reads (row T5)
 * through the side's own bridge, which stops before the app starts, and, on a run that did not
 * seed, one onboarding start. Inputs: the side, the run environment and the evidence folder.
 * Output: the seeded project, its sessions, the leftover ids seeded and held at this start,
 * the round-trip samples and the onboarding start time, or null on the seeding run. */
export async function prepareSpeedSide(side, env, evidence) {
  assert.ok(Number.isInteger(side.leftovers) && side.leftovers >= 0, "speedSide counted the leftover ids");
  const leftovers = { seeded: side.leftovers, held: side.leftovers ? heldLeftovers(side.runtime) : 0 };
  assert.equal(leftovers.held, leftovers.seeded, "this engine start's backfill held every seeded leftover id");
  const started = await start(side.bridge, [join(side.runtime, "engine.sock")], { ...env }, evidence, "speed-bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  try {
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    const seeding = !existsSync(side.seeded);
    if (seeding) writeFileSync(side.seeded, JSON.stringify(await seedSide(bridge, side.projects)), { flag: "wx", mode: 0o600 });
    const seeded = JSON.parse(readFileSync(side.seeded, "utf8"));
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects");
    const { ticket } = await value(bridge, "desktop_open_project", { id: seeded.project });
    const round_trip_ms = await roundTrips(bridge, speedReads(seeded, ticket));
    const onboard_start_ms = seeding ? null : await onboardStart(bridge, side.projects, 1);
    return { seeded, leftovers, round_trip_ms, onboard_start_ms };
  } finally {
    await stop(started.child);
  }
}
