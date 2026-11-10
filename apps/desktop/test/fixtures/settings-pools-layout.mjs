import { act, createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { mountTab, sceneRead, settle, wordsOf } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const APPLIED = { pending: false, outcome: "applied", rejection: null };
let timers = [];
let usageSince;
let current = {};
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
const ACCOUNTS = { ok: SIGNED_IN, waiting: SIGNED_IN.map((item, index) => (index === 1 ? { ...item, ...LATE } : item)), empty: [] };
const next = (why) => ({ kind: "next", weekly_resets_ms: null, weekly_left_percent: null, use_last: false, why });
const ranked = (index, standing) => ({ connection: `acct-${index}`, standing });
const order = (accounts) => ({ pool: "pool-main", strategy: "use_it_or_lose_it", accounts, own_order_projects: 0 });
const SPARE_ORDER = { pool: "pool-spare", strategy: "balanced", accounts: [ranked(3, next({ kind: "tied_order" }))], own_order_projects: 1 };
const READY_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(0, next({ kind: "least_used" })), ranked(1, next({ kind: "more_used" }))];
const HELD_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(1, { kind: "not_signed_in" }), ranked(0, { kind: "never_use" })];
const NONE_ROWS = [ranked(1, { kind: "not_signed_in" }), ranked(0, { kind: "never_use" }), ranked(2, { kind: "five_hour_cap", until_ms: null })];
const UNCONFIRMED_ROWS = [ranked(2, next({ kind: "ranked", rank: 1 })), ranked(1, { kind: "sign_in_unconfirmed" })];
const RANKINGS = { ok: [order(READY_ROWS), SPARE_ORDER], held: [order(HELD_ROWS), SPARE_ORDER], unconfirmed: [order(UNCONFIRMED_ROWS), SPARE_ORDER], none: [order(NONE_ROWS), SPARE_ORDER], bare: [] };
const SCHEDULE = { strategy: "use_it_or_lose_it", pools: [{ id: "pool-spare", strategy: "balanced" }], projects: [], accounts: [{ connection: "acct-1", plan: { rank: 2, reserve_percent: 10, pin: "use_last" } }], version: 4 };
const reading = (used_percent) => (used_percent === null ? null : { used_percent, resets_at_ms: null });
const limit = (index, weekly, fiveHour = null) => ({ account: `acct-${index}`, plan: null, weekly: reading(weekly), five_hour: reading(fiveHour), five_hour_reset_override_ms: null, weekly_reset_override_ms: null, source: "poll", observed_ms: 0, checked_ms: 0, failure: null });
const LIMITS = { ok: [limit(2, 62, 18), limit(0, 30)], near: [limit(2, 80), limit(0, 79), limit(1, 100)], mixed: [limit(2, 80), limit(1, 100)] };

/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, a failure thrown, or a read that never lands. */
function answer(scene, command, args) {
  const failing = (state, list) => sceneRead(state, list, FAILURE, command);
  if (scene.hold === command) return new Promise(() => {});
  if (command === "desktop_pool_summaries") return failing(scene.pools ?? "ok", POOLS);
  if (command === "desktop_list_accounts") {
    const accounts = failing(scene.accounts ?? "ok", ACCOUNTS);
    return accounts instanceof Promise ? accounts : { accounts, next: null };
  }
  if (command === "desktop_account_schedule") return failing(scene.schedule ?? "ok", { ok: SCHEDULE });
  if (command === "desktop_account_ranking") return failing(scene.ranking ?? "ok", RANKINGS);
  if (command === "desktop_set_account_schedule") return APPLIED;
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
 * pools and accounts reads as ok, full, waiting, long, empty, error, missing or pending, and
 * the theme. Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  current = scene;
  timers = [];
  const start = window.setInterval.bind(window);
  window.setInterval = (run, ms, ...rest) => { timers.push({ run, ms }); return start(run, ms, ...rest); };
  await mountTab(createElement(Settings, { tab: "pools" }), (command, args) => answer(scene, command, args), theme);
}

/** Reads the first account line. Output: its words and the edges of its name and checkbox. */
export function pick() {
  const row = document.querySelector(".pick");
  return { words: wordsOf(".pick"), nameRight: row.querySelector("span").getBoundingClientRect().right, boxLeft: row.querySelector("input").getBoundingClientRect().left };
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
