import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { invoke, newProject, prepareWindow } from "./master-settings-window.mjs";
import { click, settingsTab, shown, toasts } from "./settings-steps.mjs";
import { go, paste, pick, reopen, run, segment, tab, until, waitFor } from "./trial-kit.mjs";
import { masterRows, startIfPaused, teamNodes, terminalLive } from "./trial4/n-d8-master-reply.mjs";

const UI_MS = 30000;
const START_MS = 150000;
const MOVE_MS = 180000;
const HOLD_MS = 50000;
const POOL = "local-pool";
const FIRST = "local-codex";
const SECOND = "Second Codex endpoint";
const SETTING = "Live master account";
const KEEP = "Keep start account";
const MOVE = "Move when idle";
const KEEP_HINT = "A live master stays on the account it started on until it restarts.";
const MOVE_HINT = "An idle live master moves to its pool's top account and starts again there.";
const MOVED = "[data-testid=master-moved]";
const MOVED_LINE = "The live master moved to the account that is now first in this pool's order and started again there (Live master account: Move when idle).";
const MASTER = "#ps-master-harness";
const READY = "Codex stand-in ready";
const USE_LAST = { rank: null, reserve_percent: 0, pin: "use_last" };
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };
let generation = 2000000;
let second = null;

/** Adds a second account a Codex master can run on, as Add account does for a URL with no key,
 * and puts it in the fixture's pool behind the first. The URL is never called. Output: its id. */
async function secondAccount() {
  const draft = { provider: "openai_compatible", method: "url", name: SECOND, url: "http://127.0.0.1:9/v1", model: "author/model" };
  const account = await invoke("desktop_add_account", { draft, key: null });
  assert.equal(account.name, SECOND);
  const basis = await invoke("desktop_inspect_model_access", { collection: "pools", id: POOL });
  const now = basis.record.pool;
  assert.ok(now.members.includes(FIRST) && !now.members.includes(account.id), JSON.stringify(now.members));
  const pool = { ...now, members: [...now.members, account.id] };
  const saved = await invoke("desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: ++generation });
  assert.equal(saved.outcome, "applied", `${SECOND} joins the pool: ${JSON.stringify(saved)}`);
  return account.id;
}

/** Saves Codex as the project's master harness on the Team settings, as the owner does, so both
 * endpoint accounts of the pool can run the master. Output: none. */
async function codexMaster() {
  await go(`#/projects/${encodeURIComponent(run.project)}/settings?section=team`);
  await until(() => shown(MASTER), UI_MS, "the Team settings never showed the master harness");
  if ((await $(MASTER).getValue()) !== "codex") {
    await pick(MASTER, "codex");
    await click("button=Save team");
    await until(async () => (await toasts()).some((line) => line.includes("Team saved.")), UI_MS, "saving Codex showed no Team saved.");
  }
  await reopen("settings?section=team");
  await until(async () => (await shown(MASTER)) && (await $(MASTER).getValue()) === "codex", UI_MS, "the master harness did not read codex after saving");
}

/** Reads the running live master from the engine's team graph. Output: its node, or null while
 * none runs on Codex with a terminal. */
async function master() {
  const node = (await teamNodes(run.project)).find((item) => item.kind === "master");
  return node && node.terminal && node.harness === "codex" && ["idle", "working", "starting"].includes(node.status) ? node : null;
}

/** Opens the master area and waits for the live Codex master and its terminal. Output: the
 * account the master runs on, one of the pool's two endpoint accounts. */
async function liveMaster() {
  await tab("conversation");
  await startIfPaused(kit, run.project);
  await terminalLive(kit, START_MS);
  let account = null;
  await waitFor(async () => {
    const node = await master();
    account = node?.account ?? null;
    const ready = (await masterRows()).some((row) => row.includes(READY));
    return { done: ready && [FIRST, second].includes(account), seen: `master ${JSON.stringify(node)}, ready line ${ready}` };
  }, START_MS, "no live Codex master ever ran on an account of the pool");
  return account;
}

/** Reads the engine's account schedule through the app's own command. Output: the schedule. */
function schedule() {
  return invoke("desktop_account_schedule", {});
}

/** Pins an account Use last through the app's own command, as its Pin control does, so the
 * pool's order puts the other account first. Input: the connection id. Output: none. */
async function useLast(connection) {
  const now = await schedule();
  if (now.accounts.find((row) => row.connection === connection)?.plan.pin === "use_last") return;
  const saved = await invoke("desktop_set_account_schedule", { change: { kind: "account", connection, plan: USE_LAST }, version: now.version, generation: ++generation });
  assert.equal(saved.outcome, "applied", `${connection} is pinned Use last: ${JSON.stringify(saved)}`);
  await until(async () => (await schedule()).accounts.find((row) => row.connection === connection)?.plan.pin === "use_last", UI_MS, `the engine never kept Use last for ${connection}`);
}

/** Opens Settings, Pools and waits for the Live master account card. Output: none. */
async function poolsPage() {
  await go("#/settings/profile");
  await settingsTab("Pools");
  await until(() => shown(segment(SETTING, KEEP)), UI_MS, "Settings, Pools never showed Live master account");
}

/** Reads the Live master account card as the owner sees it. Output: the picked option's words
 * and the hint under it. */
function card() {
  return browser.execute((label) => {
    const section = [...document.querySelectorAll("section.pool-app-order")].find((item) => item.getAttribute("aria-label") === label);
    return {
      picked: section?.querySelector('[role="radiogroup"] [aria-checked="true"]')?.textContent ?? null,
      hint: section?.querySelector(".pool-app-hint")?.textContent ?? null,
    };
  }, SETTING);
}

/** Picks a Live master account option on Settings, Pools as the owner does, and reads it back
 * from the engine and from the page opened again. Inputs: the option's words, its engine code
 * and its hint. Output: none. */
async function choose(option, code, hint) {
  await poolsPage();
  if ((await card()).picked !== option) await click(segment(SETTING, option));
  await until(async () => (await schedule()).live_master === code, UI_MS, `the engine never kept ${code}`);
  await poolsPage();
  await until(async () => (await card()).picked === option, UI_MS, `Live master account did not read ${option} after opening Pools again`);
  assert.deepEqual(await card(), { picked: option, hint });
}

/** Reads the move notes the engine wrote for the project, through the app's own team read.
 * Output: the text of each, oldest first. */
async function moveNotes() {
  const team = await invoke("desktop_team_status", { id: run.project });
  assert.equal(team.project_id, run.project);
  return team.messages.filter((message) => message.author === "engine" && message.state === "note" && message.text === MOVED_LINE).map((message) => message.text);
}

/** Reads the move line of the master area. Output: its text, or null when it is not shown. */
function movedLine() {
  return browser.execute((selector) => document.querySelector(selector)?.textContent ?? null, MOVED);
}

describe("Live master account, in the real window on a pool with two accounts a Codex master can use", () => {
  before(async () => {
    await prepareWindow();
    second = await secondAccount();
    await newProject("live-account-shop");
    await codexMaster();
  });

  it("Keep start account (default): the order puts the other account first and the idle live master stays on its account, with no move line", async () => {
    assert.equal((await schedule()).live_master, "keep", "Keep start account is the default");
    await poolsPage();
    assert.deepEqual(await card(), { picked: KEEP, hint: KEEP_HINT });
    const held = await liveMaster();
    await useLast(held);
    await pause(HOLD_MS);
    await tab("conversation");
    await terminalLive(kit, UI_MS);
    assert.equal((await master())?.account, held, "the live master keeps the account it started on");
    assert.deepEqual(await moveNotes(), [], "the engine wrote no move note");
    assert.equal(await movedLine(), null, "the master area shows no move line");
    assert.equal((await schedule()).live_master, "keep");
  });

  it("Move when idle: the idle live master starts again on the account now first, once, and the master area says so in one line", async () => {
    const held = await liveMaster();
    const top = held === FIRST ? second : FIRST;
    await useLast(held);
    await choose(MOVE, "move_when_idle", MOVE_HINT);
    await tab("conversation");
    await waitFor(async () => {
      const node = await master();
      return { done: node?.account === top, seen: `master ${JSON.stringify(node)}, wanted account ${top}` };
    }, MOVE_MS, "the idle live master never moved to the account now first");
    await terminalLive(kit, START_MS);
    await waitFor(async () => {
      const line = await movedLine();
      return { done: line === MOVED_LINE, seen: `move line ${JSON.stringify(line)}` };
    }, UI_MS, "the master area never showed the move line");
    assert.deepEqual(await moveNotes(), [MOVED_LINE], "the engine wrote one move note");
    await pause(HOLD_MS);
    assert.equal((await master())?.account, top, "the master stays on the account it moved to");
    assert.deepEqual(await moveNotes(), [MOVED_LINE], "the master moved once");
    await choose(KEEP, "keep", KEEP_HINT);
  });
});
