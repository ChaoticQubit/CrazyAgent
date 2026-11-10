import { act, createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { startPoolWith } from "../../src/screens/settings/pool-draft.ts";
import { mountTab, sceneRead, settle, wordsOf } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const APPLIED = { pending: false, outcome: "applied", rejection: null };
const SAVED_LINE_MS = 6000;
let timers = [];
let usageSince;
let current = {};
let schedule = null;
let saves = [];
let gated = null;
let gatedAnswers = [];
let savedLineTimers = [];
let livePools = [];
let poolEditsSent = [];
let readiedSent = [];
let removedSent = [];
let unmountPage = () => {};
const say = (node) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
const member = (index, kind = "chatgpt") => ({ id: `acct-${index}`, kind, provider: "chatgpt", status: "signed_in", status_label: "Signed in" });
const account = (index) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT", method: "subscription", method_label: "Subscription", url: null, model: null, status: "signed_in", status_label: "Signed in" });
const MAIN = { id: "pool-main", name: "Main pool", summary: "3 ChatGPT", members: [member(0, "api_key"), member(1), member(2)], project_count: 2 };
const SPARE = { id: "pool-spare", name: "Spare", summary: "1 ChatGPT", members: [member(3)], project_count: 0 };
const FULL = { ...MAIN, members: Array.from({ length: 15 }, (_, index) => member(index)) };
const LATE = { status: "not_signed_in", status_label: "Not signed in" };
const WAITING = { ...MAIN, members: [member(0, "api_key"), { ...member(1), ...LATE }, member(2)] };
const KEYED_SPARE = { ...SPARE, members: [member(3, "api_key")] };
const POOLS = { ok: [MAIN, SPARE], keyed: [MAIN, KEYED_SPARE], full: [FULL], waiting: [WAITING], long: [{ ...SPARE, name: "W".repeat(60) }], hollow: [{ ...SPARE, members: [] }], empty: [] };
const SIGNED_IN = Array.from({ length: 18 }, (_, index) => account(index));
const KEYED = { method: "api_key", method_label: "API key", status: "key_saved", status_label: "Key saved" };
const CLAUDE = { provider: "claude", provider_name: "Claude" };
const ACCOUNTS = {
  ok: SIGNED_IN, waiting: SIGNED_IN.map((item, index) => (index === 1 ? { ...item, ...LATE } : item)), empty: [],
  keyed: SIGNED_IN.map((item, index) => (index === 5 ? { ...item, ...KEYED } : item)),
  mixed: SIGNED_IN.map((item, index) => (index >= 16 ? { ...item, ...CLAUDE } : item)),
};
const poolOf = (id, name, indexes, project_count) => ({ id, name, summary: "", members: indexes.map((index) => member(index)), project_count });
const upTo = (count) => Array.from({ length: count }, (_, index) => index);
const LIVE_POOLS = {
  live: () => [MAIN, SPARE],
  "live-full": () => [poolOf("pool-main", "Main pool", upTo(15), 2)],
  "live-14": () => [poolOf("pool-main", "Main pool", upTo(14), 2)],
  "live-one": () => [poolOf("pool-main", "Main pool", [3], 0)],
  "live-new": () => [MAIN, { ...SPARE, name: "New pool" }, { ...SPARE, id: "pool-two", name: "New pool 2" }],
  "live-empty": () => [],
};
const next = (why) => ({ kind: "next", weekly_resets_ms: null, weekly_left_percent: null, use_last: false, why });
const ranked = (index, standing) => ({ connection: `acct-${index}`, standing });
const order = (accounts) => ({ pool: "pool-main", strategy: "use_it_or_lose_it", accounts, own_order_projects: 0 });
const SPARE_ORDER = { pool: "pool-spare", strategy: "balanced", accounts: [ranked(3, next({ kind: "tied_order" }))], own_order_projects: 1 };
const READY_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(0, next({ kind: "least_used" })), ranked(1, next({ kind: "more_used" }))];
const HELD_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(1, { kind: "not_signed_in" }), ranked(0, { kind: "never_use" })];
const NONE_ROWS = [ranked(1, { kind: "not_signed_in" }), ranked(0, { kind: "never_use" }), ranked(2, { kind: "five_hour_cap", until_ms: null })];
const UNCONFIRMED_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(1, { kind: "sign_in_unconfirmed" })];
const FULL_ROWS = Array.from({ length: 15 }, (_, index) => ranked(index, next({ kind: "tied_order" })));
const RANKINGS = { ok: [order(READY_ROWS), SPARE_ORDER], held: [order(HELD_ROWS), SPARE_ORDER], unconfirmed: [order(UNCONFIRMED_ROWS), SPARE_ORDER], none: [order(NONE_ROWS), SPARE_ORDER], full: [order(FULL_ROWS)], bare: [] };
const SCHEDULE = { strategy: "use_it_or_lose_it", pools: [{ id: "pool-spare", strategy: "balanced" }], projects: [], accounts: [{ connection: "acct-1", plan: { rank: 2, reserve_percent: 10, pin: "use_last" } }], version: 4 };
const reading = (used_percent) => (used_percent === null ? null : { used_percent, resets_at_ms: null });
const limit = (index, weekly, fiveHour = null) => ({ account: `acct-${index}`, plan: null, weekly: reading(weekly), five_hour: reading(fiveHour), five_hour_reset_override_ms: null, weekly_reset_override_ms: null, source: "poll", observed_ms: 0, checked_ms: 0, failure: null });
const LIMITS = { ok: [limit(2, 62, 18), limit(0, 30)], near: [limit(2, 80), limit(0, 79), limit(1, 100)], mixed: [limit(2, 80), limit(1, 100)] };

/** Applies one change to the fake schedule and moves its version on, as the engine does.
 * Inputs: the schedule and the change. Output: the new schedule. */
function applied(current, change) {
  const version = current.version + 1;
  if (change.kind === "app_strategy") return { ...current, strategy: change.strategy, version };
  if (change.kind === "pool_strategy") {
    const pools = current.pools.filter((item) => item.id !== change.pool);
    return { ...current, pools: change.strategy === null ? pools : [...pools, { id: change.pool, strategy: change.strategy }], version };
  }
  return { ...current, accounts: [...current.accounts.filter((item) => item.connection !== change.connection), { connection: change.connection, plan: change.plan }], version };
}

/** Answers one save as the engine does: a change naming an old version is refused. Inputs: the
 * scene, whose `refuse` names a refusal for every save or only for save number `refuseAt`, and
 * the save's arguments. Output: the receipt. */
function saveChange(scene, args) {
  saves.push({ change: args.change, version: args.version });
  if (scene.refuse && saves.length === (scene.refuseAt ?? saves.length)) return { pending: false, outcome: "rejected", rejection: scene.refuse };
  if (args.version !== schedule.version) return { pending: false, outcome: "rejected", rejection: "stale_revision" };
  schedule = applied(schedule, args.change);
  return APPLIED;
}

/** Ranks the main pool from the fake schedule: Never use is held, Use last goes after the rest
 * and under Manual rank the ranks set the order. Output: the rankings. */
function liveRanking() {
  const plan = (index) => schedule.accounts.find((item) => item.connection === `acct-${index}`)?.plan ?? { rank: null, reserve_percent: 0, pin: null };
  const strategy = schedule.pools.find((item) => item.id === "pool-main")?.strategy ?? schedule.strategy;
  const ready = [2, 0, 1].filter((index) => plan(index).pin !== "never_use");
  if (strategy === "manual_rank") ready.sort((a, b) => (plan(a).rank ?? 1000) - (plan(b).rank ?? 1000));
  const last = (index) => (plan(index).pin === "use_last" ? 1 : 0);
  ready.sort((a, b) => last(a) - last(b));
  const held = [2, 0, 1].filter((index) => plan(index).pin === "never_use");
  return [{ ...order([...ready.map((index) => ranked(index, next({ kind: "tied_order" }))), ...held.map((index) => ranked(index, { kind: "never_use" }))]), strategy }, SPARE_ORDER];
}

/** Ranks every live pool in its member order, so a row added or removed shows at once. Output:
 * the rankings. */
function poolsRanking() {
  return livePools.map((pool) => ({ pool: pool.id, strategy: "use_it_or_lose_it", own_order_projects: 0, accounts: pool.members.map((item) => ({ connection: item.id, standing: next({ kind: "tied_order" }) })) }));
}

/** Answers one pool edit as the engine does: a pool with no account is refused. Inputs: the scene
 * and the edit's arguments. Output: the receipt. */
function editPool(scene, args) {
  const { edit } = args;
  if (edit.kind === "remove_pool") {
    removedSent.push(edit.id);
    livePools = livePools.filter((pool) => pool.id !== edit.id);
    return APPLIED;
  }
  poolEditsSent.push(edit);
  if (scene.refuseEdit) return { pending: false, outcome: "rejected", rejection: scene.refuseEdit };
  if (edit.pool.members.length === 0) return { pending: false, outcome: "rejected", rejection: "pool_needs_account" };
  const members = edit.pool.members.map((id) => member(Number(id.slice("acct-".length))));
  if (edit.kind === "create_pool") livePools = [...livePools, { id: edit.pool.id, name: edit.pool.name, summary: "", members, project_count: 0 }];
  else livePools = livePools.map((pool) => (pool.id === edit.pool.id ? { ...pool, name: edit.pool.name, members } : pool));
  return APPLIED;
}

/** Answers the commands of a pool edit and a set-up for agents. Inputs: the scene, the command
 * and its arguments. Output: the answer, or undefined when the command is another one. */
function liveEdit(scene, command, args) {
  if (command === "desktop_model_access_page") return { ticket: "ticket" };
  if (command === "desktop_edit_model_access") return editPool(scene, args);
  if (command === "desktop_configure_account_profile") {
    if (scene.ready === "refuse") throw { code: "engine_failed" };
    readiedSent.push(args.ticket.slice("connection:".length));
    return { pending: false, reply: { outcome: "configured" } };
  }
  if (command !== "desktop_inspect_model_access") return undefined;
  if (args.collection === "connections") return { ticket: `connection:${args.id}`, record: { kind: "connection", connection: { id: args.id } } };
  const pool = livePools.find((item) => item.id === args.id);
  if (!pool) throw { code: "pool_not_found" };
  return { ticket: "ticket", record: { kind: "pool", pool: { id: pool.id, name: pool.name, members: pool.members.map((item) => item.id) } } };
}

/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, a failure thrown, or a read that never lands. */
function answer(scene, command, args) {
  const failing = (state, list) => sceneRead(state, list, FAILURE, command);
  if (scene.hold === command) return new Promise(() => {});
  if (LIVE_POOLS[scene.pools]) {
    if (command === "desktop_pool_summaries") return livePools;
    const edited = liveEdit(scene, command, args);
    if (edited !== undefined) return edited;
  }
  if (command === "desktop_pool_summaries") return failing(scene.pools ?? "ok", POOLS);
  if (command === "desktop_list_accounts") {
    const accounts = failing(scene.accounts ?? "ok", ACCOUNTS);
    return accounts instanceof Promise ? accounts : { accounts, next: null };
  }
  if (command === "desktop_account_schedule") return failing(scene.schedule ?? "ok", { ok: schedule });
  if (command === "desktop_account_ranking") return scene.ranking === "pools" ? poolsRanking() : scene.ranking === "live" ? liveRanking() : failing(scene.ranking ?? "ok", RANKINGS);
  if (command === "desktop_set_account_schedule") return saveChange(scene, args);
  if (command === "desktop_usage_report") {
    usageSince = args.sinceMs;
    const limits = failing(scene.limits ?? "ok", LIMITS);
    return limits instanceof Promise ? limits : { limits };
  }
  if (scene.edits === "ok" && command === "desktop_inspect_model_access") return { ticket: "ticket", record: { kind: "pool", pool: { id: "pool-spare", name: "Spare", members: ["acct-3"] } } };
  if (scene.edits === "ok" && command === "desktop_edit_model_access") return APPLIED;
  if (command === "desktop_inspect_model_access" || command === "desktop_model_access_page") throw FAILURE;
  throw `Command ${command} not found`;
}

/** Mounts the real Settings page on Pools against a fake engine. Inputs: the scene, naming the
 * pools and accounts reads as ok, full, waiting, long, empty, error, missing or pending, with
 * `start` for what the schedule starts as, and the theme. Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  current = scene;
  timers = [];
  saves = [];
  gated = null;
  gatedAnswers = [];
  savedLineTimers = [];
  livePools = LIVE_POOLS[scene.pools]?.() ?? [];
  poolEditsSent = [];
  readiedSent = [];
  removedSent = [];
  if (scene.drafted) startPoolWith(scene.drafted);
  schedule = { ...SCHEDULE, ...scene.start };
  const start = window.setInterval.bind(window);
  window.setInterval = (run, ms, ...rest) => { timers.push({ run, ms }); return start(run, ms, ...rest); };
  const wait = window.setTimeout.bind(window);
  window.setTimeout = (run, ms, ...rest) => {
    if (ms !== SAVED_LINE_MS) return wait(run, ms, ...rest);
    savedLineTimers.push(run);
    return 0;
  };
  unmountPage = await mountTab(createElement(Settings, { tab: "pools", action: scene.action }), (command, args) => (command === gated ? new Promise((done) => gatedAnswers.push(() => done(answer(scene, command, args)))) : answer(scene, command, args)), theme);
}

/** Lists every change the page asked the engine to save. Output: each change and its version. */
export function savedChanges() {
  return saves;
}

/** Makes one engine command wait for `openGate`. Input: the command. Output: nothing. */
export function gate(command) {
  gated = command;
}

/** Answers every command that waits at the gate and stops holding new ones. Output: nothing. */
export async function openGate() {
  gated = null;
  gatedAnswers.splice(0).forEach((go) => go());
  await settle(80);
}

/** Answers the oldest command waiting at the gate and keeps holding new ones. Output: nothing. */
export async function passGate() {
  gatedAnswers.shift()?.();
  await settle(80);
}

/** Lists the pool edits the page sent, without pool removals. Output: each edit. */
export function poolEdits() {
  return poolEditsSent;
}

/** Lists the pools the page removed. Output: their ids. */
export function removedPools() {
  return removedSent;
}

/** Lists the accounts the page set up for agents. Output: their ids. */
export function readied() {
  return readiedSent;
}

/** Reads the names in the Add account panel. Output: the names in list order. */
export function addRows() {
  return [...document.querySelectorAll(".pool-add-row .ellipsis:first-child")].map(say);
}

/** Picks an option of a native select as the owner's choice does. Inputs: the select's selector
 * and the option's value. Output: nothing. */
export async function choose(selector, value) {
  const node = document.querySelector(selector);
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(node, value);
  await act(async () => { node.dispatchEvent(new Event("change", { bubbles: true })); });
  await settle(40);
}

/** Takes the page away, as going to another page does. Output: nothing. */
export async function unmount() {
  await act(async () => { unmountPage(); });
  await settle(40);
}

/** Reads the option a choice group shows. Input: the group's label. Output: its words. */
export function checked(label) {
  return say(document.querySelector(`[role="radiogroup"][aria-label="${label}"] [aria-checked="true"]`));
}

/** Reads the options of a choice group. Input: the group's label. Output: their words. */
export function optionsOf(label) {
  return [...document.querySelectorAll(`[role="radiogroup"][aria-label="${label}"] [role="radio"]`)].map(say);
}

/** Says where the focus is. Output: the focused element's tag, words and id, and the label of
 * the choice group around it. */
export function focusPlace() {
  const node = document.activeElement;
  return { tag: node?.tagName ?? "", words: say(node), id: node?.id ?? "", group: node?.closest('[role="radiogroup"]')?.getAttribute("aria-label") ?? "" };
}

/** Ends the wait of the saved line, as its six seconds passing does. Output: nothing; throws
 * when no saved line waits. */
export async function fireSavedLine() {
  if (savedLineTimers.length === 0) throw new Error("no saved line is waiting to clear");
  await act(async () => { savedLineTimers.at(-1)(); });
  await settle(20);
}

/** Says whether every row's controls stay inside it. Output: a boolean, false with no rows. */
export function controlsFit() {
  const rows = [...document.querySelectorAll(".pool-row")];
  return rows.length > 0 && rows.every((row) => [...row.querySelectorAll(".pool-ctl, .pool-sched")].every((part) => part.getBoundingClientRect().right <= row.getBoundingClientRect().right));
}

/** Says whether the alert is inside the window. Output: a boolean, false with no alert. */
export function alertInView() {
  const box = document.querySelector('#mount [role="alert"]')?.getBoundingClientRect();
  return Boolean(box && box.top >= 0 && box.bottom <= window.innerHeight);
}

/** Reads the left list. Output: each pool button's name, count, line, whether it is the selected
 * one and whether its words spill out of it. */
export function poolItems() {
  return [...document.querySelectorAll(".pool-item")].map((item) => ({
    name: say(item.querySelector(".pool-item-label")), count: say(item.querySelector(".pool-item-count")), about: say(item.querySelector(".pool-item-about")),
    current: item.getAttribute("aria-current"), spills: item.scrollWidth > item.clientWidth,
  }));
}

/** Reads the selected pool's order. Output: each row's number, spoken place, name, badges, plan
 * line, why, Sign in link, usage lines with their tones, no-usage line and width of its words. */
export function orderRows() {
  return [...document.querySelectorAll(".pool-row")].map((row) => {
    const link = row.querySelector(".pool-why a");
    return {
      held: row.classList.contains("held"), number: say(row.querySelector('.pool-pos [aria-hidden="true"]')), spoken: say(row.querySelector(".pool-pos .visually-hidden")),
      name: say(row.querySelector(".pool-name")), badges: [...row.querySelectorAll(".badge")].map(say), plan: say(row.querySelector(".pool-plan")), why: say(row.querySelector(".pool-why")),
      link: link && { href: link.getAttribute("href"), label: link.getAttribute("aria-label") },
      limits: [...row.querySelectorAll(".pool-limit")].map((line) => [say(line), line.classList.contains("near") ? "near" : line.classList.contains("full") ? "full" : "ok"]),
      none: say(row.querySelector(".pool-limit-none")), whoWidth: row.querySelector(".pool-who").getBoundingClientRect().width,
    };
  });
}

/** Says where the held line stands. Output: its words and whether it sits under the last ready
 * row and over the first held one. */
export function heldHead() {
  const head = document.querySelector(".pool-held-head");
  const lastReady = [...document.querySelectorAll(".pool-row:not(.held)")].at(-1);
  const firstHeld = document.querySelector(".pool-row.held");
  return { words: say(head), between: Boolean(head && firstHeld && (!lastReady || lastReady.getBoundingClientRect().bottom <= head.getBoundingClientRect().top) && head.getBoundingClientRect().bottom <= firstHeld.getBoundingClientRect().top) };
}

/** Reads the two panes and the page's width. Output: each pane's edges and whether the page or
 * the main area scrolls sideways. */
export function panes() {
  const main = document.getElementById("main");
  const [list, pool] = [...document.querySelectorAll(".pool-screen > *")].map((pane) => pane.getBoundingClientRect());
  return { list: { left: list.left, right: list.right, top: list.top }, pool: { left: pool.left, right: pool.right, top: pool.top }, sideways: document.documentElement.scrollWidth > window.innerWidth || main.scrollWidth > main.clientWidth };
}

/** Changes what the fake engine answers from now on. Input: the parts of the scene that change.
 * Output: nothing. */
export function changeScene(parts) {
  Object.assign(current, parts);
}

/** Lists the delays of the repeating timers the page started. Output: the delays in ms. */
export function timerDelays() {
  return timers.map((timer) => timer.ms);
}

/** Runs the page's repeating timer of one delay once. Input: the delay in ms. Output: nothing;
 * throws unless exactly one timer has that delay. */
export async function fireTimer(ms) {
  const found = timers.filter((timer) => timer.ms === ms);
  if (found.length !== 1) throw new Error(`${found.length} timers repeat every ${ms} ms`);
  await act(async () => { found[0].run(); });
  await settle(60);
}

/** Reads the range start of the last usage read. Output: it in ms, or null for all of time. */
export function lastUsageSince() {
  return usageSince;
}
