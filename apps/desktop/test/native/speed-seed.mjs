import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS, STEP_MS, bridgeCall, closedPort, start, stop, value } from "./e2e-harness.mjs";
import { poolForMaster, readyForAgents } from "./master-lease.mjs";

const SOCKET_PATH_MAX = 100;

/** Master sessions the big-project row opens the project with (DEC-169 Q5). */
export const SESSIONS = 50;
/** Calls per command in one round-trip reading (row T5). */
export const ROUND_TRIPS = 100;
/** The project the speed rows walk. */
export const PROJECT = "Speed Project";
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const SESSIONS_MAX = 200;

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
  const made = await value(bridge, "desktop_setup_project", {
    spec: { name: PROJECT, repositories: [] },
    setup: { about: "The project the speed rows open", tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
    generation: next(),
  });
  applied("the project is made", made);
  const project = made.project_id;
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

/** The reads row T5 times: the project and master reads the project page makes on open. Inputs:
 * the seeded side and the project ticket. Output: the calls. */
export function speedReads(seeded, ticket) {
  assert.match(seeded.project, /\S/, "a seeded project");
  assert.match(ticket, /\S/, "a project ticket");
  return [
    { cmd: "desktop_open_project", args: { id: seeded.project } },
    { cmd: "desktop_master_state", args: { ticket } },
    { cmd: "desktop_master_sessions", args: { ticket } },
  ];
}

/** The speed suite's side, from the speed driver's environment: `SPEED_SIDE`, a folder
 * build-speed-base.sh made, and `SPEED_STATE`, the side's kept engine state, made here with
 * its settings on first use. A state with an engine but no seeded.json is a broken seeding
 * and is refused. Input: the app run.mjs was given. Output: the side's paths. */
export async function speedSide(app) {
  const folder = process.env.SPEED_SIDE;
  const state = process.env.SPEED_STATE;
  assert.ok(folder && isAbsolute(folder) && existsSync(join(folder, "bin")), "SPEED_SIDE names a speed artifact folder");
  assert.ok(state && isAbsolute(state), "SPEED_STATE names the side's kept state folder");
  assert.equal(app, join(folder, "bin", "crazyagent-desktop"), "the app is the side's own build");
  const side = { folder, state, engine: join(folder, "bin", "crazyagent"), bridge: join(folder, "bin", "desktop-e2e-bridge"),
    runtime: join(state, "engine"), projects: join(state, "projects"), seeded: join(state, "seeded.json") };
  assert.ok(join(side.runtime, "engine.sock").length < SOCKET_PATH_MAX, `the engine socket path fits a socket address: ${side.runtime}`);
  const fresh = !existsSync(side.runtime) || readdirSync(side.runtime).length === 0;
  assert.ok(fresh || existsSync(side.seeded), `${state} holds an engine that was never seeded; remove it and run again`);
  for (const folderMade of [side.runtime, side.projects]) mkdirSync(folderMade, { recursive: true, mode: 0o700 });
  if (fresh) writeFileSync(join(side.runtime, "global.toml"), speedSettings(await closedPort()), { flag: "wx", mode: 0o600 });
  return side;
}

/** Readies a speed side once its engine runs: seeds it on first use, then times the project
 * page's reads (row T5) through the side's own bridge, which stops before the app starts.
 * Inputs: the side, the run environment and the evidence folder. Output: the seeded project,
 * its sessions and the round-trip samples. */
export async function prepareSpeedSide(side, env, evidence) {
  const started = await start(side.bridge, [join(side.runtime, "engine.sock")], { ...env }, evidence, "speed-bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  try {
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    if (!existsSync(side.seeded)) writeFileSync(side.seeded, JSON.stringify(await seedSide(bridge, side.projects)), { flag: "wx", mode: 0o600 });
    const seeded = JSON.parse(readFileSync(side.seeded, "utf8"));
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects");
    const { ticket } = await value(bridge, "desktop_open_project", { id: seeded.project });
    return { seeded, round_trip_ms: await roundTrips(bridge, speedReads(seeded, ticket)) };
  } finally {
    await stop(started.child);
  }
}
