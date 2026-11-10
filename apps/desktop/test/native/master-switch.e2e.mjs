import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { shownView } from "./agents-cards.mjs";
import { TURN_S } from "./master-switch.wdio.conf.mjs";
import { click, shown, toasts, unfold } from "./settings-steps.mjs";
import { go, paste, pick, reopen, run, segment, tab, until, waitFor } from "./trial-kit.mjs";
import { masterRows, sendToMaster, startIfPaused, teamNodes, terminalLive } from "./trial4/n-d8-master-reply.mjs";

const root = process.env.MS_ROOT;
const GIT = "/usr/bin/git";
const POOL = "local-pool";
const UI_MS = 30000;
const SETUP_MS = 120000;
const SWITCH_MS = (TURN_S + 60) * 1000;
const HOLD_MS = 40000;
const MASTER = "#ps-master-harness";
const APPLY = "#ps-apply-switch";
const TITLE = ".pj-master-live .pj-console-title";
const WAITS = "[data-testid=master-switch-waits]";
const DIVIDER = "[data-testid=harness-switch]";
const STOP = "//dialog[@open]//button[normalize-space(.)=\"Stop all\"]";
const INPUT_KEY = "crazyagent.terminal-input";
const STORE = join(root, "home", "Library", "WebKit", basename(process.env.WD_APP));
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };
let generation = 0;

/** Calls one desktop command through the app's own bridge, as the window does. Inputs: the
 * command and its arguments. Output: its answer; throws with the failure the app returned. */
async function invoke(command, args) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: JSON.stringify(failure) }));
  }, command, args);
  if (answer.failed !== undefined) throw new Error(`${command} failed: ${answer.failed}`);
  return answer.value;
}

/** Proves this window's web store is the fixture's own before any row can write to it: WebKit
 * made its folder under the fixture home when the page loaded and no Terminal input is saved
 * there. Fails when the store stayed in the real user Library. Output: none. */
async function storeIsTheFixtures() {
  await until(() => existsSync(join(STORE, "WebsiteData")), UI_MS, `the window's web store never appeared at ${STORE}`);
  assert.equal(await browser.execute((key) => window.localStorage.getItem(key), INPUT_KEY), null, "a fixture's web store starts with no Terminal input");
}

/** Reads whether the fixture's own web store holds a saved key, from WebKit's files and never
 * through the window. Input: the key. Output: a boolean. */
function fixtureStoreHolds(key) {
  return readdirSync(STORE, { recursive: true }).filter((path) => basename(path) === "localstorage.sqlite3").some((path) => {
    const store = new DatabaseSync(join(STORE, path), { readOnly: true });
    try {
      return store.prepare("SELECT 1 FROM ItemTable WHERE key=?").get(key) !== undefined;
    } finally {
      store.close();
    }
  });
}

/** Makes an owner repository with one commit, run with no system or user git config. Input:
 * its name. Output: its absolute path. */
function ownerRepo(name) {
  const repo = join(root, "repos", name);
  mkdirSync(repo, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), `# ${name}\n`);
  const env = { PATH: "/usr/bin:/bin", HOME: repo, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" };
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "Start"]]) {
    const result = spawnSync(GIT, args, { cwd: repo, encoding: "utf8", env });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
  return repo;
}

/** Adds an owner repository as a project on the pool that holds a Codex and a Claude Code
 * account, waits for onboarding and opens it. Input: the repository name. Output: the project
 * id, also set as the kit's project. */
async function newProject(name) {
  const receipt = await invoke("desktop_start_onboarding", { path: ownerRepo(name), pool: POOL, generation: ++generation });
  assert.equal(receipt.outcome, "applied", `${name} becomes a project: ${JSON.stringify(receipt)}`);
  const id = receipt.project_id;
  for (const deadline = Date.now() + SETUP_MS; ; await pause(1000)) {
    const view = await invoke("desktop_onboarding", { id });
    if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
    assert.ok(Date.now() < deadline, `onboarding of ${name} still running: ${JSON.stringify(view)}`);
  }
  await invoke("desktop_open_project", { id });
  const pools = await invoke("desktop_model_access_page", { collection: "pools", next: false });
  const assigned = await invoke("desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project: id, pool: POOL }, generation: ++generation });
  assert.equal(assigned.outcome, "applied", `${name} gets the pool: ${JSON.stringify(assigned)}`);
  run.project = id;
  return id;
}

/** Opens the project's Team settings and waits until its master select shows. Output: the
 * saved master harness. */
async function teamSettings() {
  await go(`#/projects/${encodeURIComponent(run.project)}/settings?section=team`);
  await until(() => shown(MASTER), UI_MS, "the Team settings never showed the master harness");
  return $(MASTER).getValue();
}

/** Saves a master harness on the Team settings as the owner does and reads it back. Input:
 * the harness id. */
async function saveMaster(harness) {
  if ((await teamSettings()) === harness) return;
  await pick(MASTER, harness);
  await click("button=Save team");
  await until(async () => (await toasts()).some((line) => line.includes("Team saved.")), UI_MS, `saving ${harness} showed no Team saved.`);
  await tab("overview");
  await until(async () => (await teamSettings()) === harness, UI_MS, `the master harness did not read ${harness} after saving`);
}

/** Chooses how a saved master harness change reaches the running master and reads it back.
 * Input: when_idle or next_start. */
async function applyChange(mode) {
  await teamSettings();
  await until(() => shown(APPLY), UI_MS, "the Team settings never showed Apply a master harness change");
  await pick(APPLY, mode);
  await until(async () => { await reopen("settings?section=team"); return (await shown(APPLY)) && (await $(APPLY).getValue()) === mode; }, UI_MS, `Apply a master harness change did not read ${mode}`);
}

/** Reads the master area as the owner sees it. Output: { title, waits, divider, rows }. */
async function area() {
  const seen = await browser.execute((title, waits, divider) => ({
    title: document.querySelector(title)?.textContent ?? "",
    waits: document.querySelector(waits)?.textContent ?? "",
    divider: [...document.querySelectorAll(divider)].map((node) => node.textContent).join(" | "),
  }), TITLE, WAITS, DIVIDER);
  return { ...seen, rows: (await masterRows()).join("\n") };
}

/** Checks the running Master tab in the real window against the Terminal input this app has
 * saved, reading the store and never writing it: no text area beside the terminal with
 * nothing saved or Terminal only, exactly one with a box choice. Logs what the store held.
 * Output: { saved, boxes }. */
async function masterFollowsSavedInput() {
  const seen = await browser.execute(() => ({
    saved: window.localStorage.getItem("crazyagent.terminal-input"),
    boxes: [...document.querySelectorAll("textarea:not(.xterm-helper-textarea)")].filter((item) => item.getClientRects().length > 0).length,
  }));
  console.log(`Master tab, saved Terminal input and text areas: ${JSON.stringify(seen)}`);
  assert.equal(seen.boxes, seen.saved === "box" || seen.saved === "both" ? 1 : 0, JSON.stringify(seen));
  return seen;
}

/** Saves a Terminal input option on Settings, Profile, as the owner does, and reads it back after
 * leaving the page and opening it again. Input: the option's label. */
async function saveInput(option) {
  const choice = segment("Terminal input", option);
  await go("#/settings/profile");
  await until(() => shown(choice), UI_MS, "Settings, Profile never showed Terminal input");
  await click(choice);
  await go("#/settings/process");
  await go("#/settings/profile");
  await until(async () => (await shown(choice)) && (await $(choice).getAttribute("aria-checked")) === "true", UI_MS, `Terminal input did not read ${option} after opening Settings again`);
}

/** Starts one sub-agent through the app's own command, opens its terminal from its card and
 * reads what the owner is offered there. Input: the project id. Output: { label, boxes }. */
async function subAgentOffers(id) {
  const receipt = await invoke("desktop_team_delegate", { id, harness: "claude-code", prompt: "Stand by.", generation: ++generation });
  assert.equal(receipt.outcome, "applied", `the sub-agent is asked for: ${JSON.stringify(receipt)}`);
  let agent;
  await waitFor(async () => {
    const nodes = await teamNodes(id);
    agent = nodes.find((node) => node.kind !== "master" && node.terminal);
    return { done: agent !== undefined, seen: JSON.stringify(nodes).slice(0, 400) };
  }, SWITCH_MS, "no sub-agent terminal ever started");
  await tab("agents");
  await shownView(kit, agent.label);
  const boxes = await browser.execute((label) => {
    const panel = [...document.querySelectorAll("section.tg-terminal")].find((node) => node.getAttribute("aria-label") === `${label} terminal` && node.getClientRects().length);
    return [...panel.querySelectorAll("textarea.tg-composer-input")].filter((item) => item.getClientRects().length > 0).length;
  }, agent.label);
  return { label: agent.label, boxes };
}

/** Opens the master area with the live master running and its terminal showing a harness's
 * ready line. Input: the harness name the stand-in prints. Output: the area. */
async function liveMaster(name) {
  await tab("conversation");
  await startIfPaused(kit, run.project);
  await terminalLive(kit, SWITCH_MS);
  await waitFor(async () => {
    const seen = await area();
    return { done: seen.rows.includes(`${name} stand-in ready`), seen: JSON.stringify(seen).slice(0, 400) };
  }, SWITCH_MS, `the master terminal never showed the ${name} stand-in`);
  return area();
}

/** Sends one owner line to the live master and waits until its turn shows on the terminal.
 * Inputs: the line and the harness name the stand-in prints. Output: none. */
async function ownerTurn(line, name) {
  const shownTurns = async () => (await masterRows()).filter((row) => row.includes(`${name} stand-in turn`)).length;
  const turns = await shownTurns();
  await sendToMaster(kit, line, async () => (await shownTurns()) > turns);
  await waitFor(async () => {
    const now = await shownTurns();
    return { done: now > turns, seen: `${now} ${name} turns on the terminal` };
  }, UI_MS, `${name} never started a turn for "${line}"`);
}

/** Reads the owner turns the engine counted from the live terminal in the project's newest
 * master session. Input: the project id. Output: { turns, last } or null before any. */
function ownerTurns(id) {
  const store = new DatabaseSync(join(root, "engine", "engine.db"), { readOnly: true });
  try {
    return store.prepare("SELECT turns, last_line AS last FROM session_owner_turns WHERE project=? ORDER BY rowid DESC LIMIT 1").get(id) ?? null;
  } finally {
    store.close();
  }
}

/** Reads the switches the engine wrote for a project. Input: the project id. Output: the
 * harness of each, oldest first. */
function switches(id) {
  const store = new DatabaseSync(join(root, "engine", "engine.db"), { readOnly: true });
  try {
    return store.prepare("SELECT harness FROM team_session_switches WHERE project=? ORDER BY created_ms").all(id).map((row) => row.harness);
  } finally {
    store.close();
  }
}

/** Waits until the master area shows the switch to a harness: the divider names it, the area
 * is headed by it with no waiting note, and the terminal shows its stand-in. Input: the
 * harness name. Output: the area. */
async function switchedTo(name) {
  let seen = null;
  await waitFor(async () => {
    seen = await area();
    const done = seen.divider.includes(name) && seen.title.includes(name) && seen.waits === "" && seen.rows.includes(`${name} stand-in ready`);
    return { done, seen: JSON.stringify(seen).slice(0, 400) };
  }, SWITCH_MS, `the master area never switched to ${name}`);
  return seen;
}

describe("A saved master harness change reaches the live master as the owner's Apply a master harness change says", () => {
  before(async () => {
    await until(() => browser.execute(() => Boolean(window.__TAURI_INTERNALS__)), 120000, "the app window never loaded");
    await storeIsTheFixtures();
    await until(async () => (await invoke("desktop_connect").catch(() => ({}))).connected === true, 60000, "the app never connected to the test engine");
    assert.equal((await invoke("desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: ++generation })).outcome, "applied");
    mkdirSync(join(root, "projects"), { mode: 0o700 });
    const homed = await invoke("desktop_set_projects_home", { path: join(root, "projects"), generation: ++generation });
    assert.equal(homed.outcome, "applied", `the projects home is set: ${JSON.stringify(homed)}`);
    await browser.execute(() => location.reload());
    await until(() => browser.execute(() => document.readyState === "complete" && Boolean(window.__TAURI_INTERNALS__)), 60000, "the app window never loaded again");
  });

  it("When the master is idle (default): Save while the master is in a turn, the turn ends, then the divider and a Codex turn", async () => {
    const id = await newProject("idle-shop");
    await teamSettings();
    await until(() => shown(APPLY), UI_MS, "the Team settings never showed Apply a master harness change");
    assert.equal(await $(APPLY).getValue(), "when_idle", "When the master is idle is the default");
    await saveMaster("claude-code");
    await liveMaster("Claude");
    await masterFollowsSavedInput();
    await ownerTurn("Start a long turn.", "Claude");
    const saved = Date.now();
    await saveMaster("codex");
    await tab("conversation");
    await until(async () => (await area()).waits === "Codex from the next start", UI_MS, "the header never said the Codex change waits");
    const during = await area();
    assert.ok(Date.now() - saved < TURN_S * 1000, `the Save took longer than the ${TURN_S} s turn, so the turn could have ended`);
    assert.match(during.title, /Claude Code/, "the master in its turn keeps its harness");
    assert.equal(during.waits, "Codex from the next start", "the header says the change waits");
    assert.equal(during.divider, "", "no switch while the master is in its turn");
    assert.deepEqual(switches(id), []);
    const after = await switchedTo("Codex");
    assert.deepEqual(switches(id), ["codex"], "the engine wrote one switch row, to Codex");
    const counted = ownerTurns(id)?.turns ?? 0;
    await ownerTurn("Now on Codex.", "Codex");
    await waitFor(async () => {
      const now = ownerTurns(id);
      return { done: (now?.turns ?? 0) > counted, seen: `${JSON.stringify(now)}, ${counted} turns before the Codex turn` };
    }, SWITCH_MS, "the engine never counted the Codex turn in the live session");
    assert.ok(after.divider.includes("Codex"));
  });

  it("At the next start: Save keeps the master on Claude Code with a header note until Stop all and Start all, then the divider", async () => {
    const id = await newProject("next-shop");
    await applyChange("next_start");
    await saveMaster("claude-code");
    await liveMaster("Claude");
    await ownerTurn("A short turn on Claude.", "Claude");
    await saveMaster("codex");
    await tab("conversation");
    await until(async () => (await area()).waits === "Codex from the next start", UI_MS, "the header never said the Codex change waits for the next start");
    for (const ends = Date.now() + HOLD_MS; Date.now() < ends; await pause(2000)) {
      const seen = await area();
      assert.match(seen.title, /Claude Code/, `the master switched before its next start: ${JSON.stringify(seen).slice(0, 300)}`);
      assert.equal(seen.waits, "Codex from the next start");
      assert.equal(seen.divider, "");
    }
    assert.deepEqual(switches(id), []);
    await tab("agents");
    await unfold("Agents and tasks");
    await click("button=Stop all");
    await click(STOP);
    await until(async () => !(await shown("dialog[open]")), 60000, "the Stop all dialog stayed open");
    await tab("conversation");
    assert.equal(await startIfPaused(kit, id), true, "Stop all stopped the team, so the master area offers Start all");
    await switchedTo("Codex");
    assert.deepEqual(switches(id), ["codex"], "the engine wrote one switch row, to Codex");
  });

  for (const [option, saved, name] of [["Message box only", "box", "box-shop"], ["Both", "both", "both-shop"]]) {
    it(`${option}: saved in Settings, the Master tab and a sub-agent offer the box and one message goes through the real engine`, async () => {
      const id = await newProject(name);
      await saveMaster("claude-code");
      await saveInput(option);
      await until(() => fixtureStoreHolds(INPUT_KEY), UI_MS, `Terminal input never reached the fixture's own web store at ${STORE}`);
      await liveMaster("Claude");
      assert.equal((await masterFollowsSavedInput()).saved, saved);
      await ownerTurn(`Through ${option}.`, "Claude");
      const offered = await subAgentOffers(id);
      assert.equal(offered.boxes, 1, `${offered.label} offers one message box`);
    });
  }
});
