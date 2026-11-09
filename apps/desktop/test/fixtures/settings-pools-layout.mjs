import { createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { mountTab, sceneRead, wordsOf } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const member = (index, kind = "chatgpt") => ({ id: `acct-${index}`, kind, provider: "chatgpt", status: "signed_in", status_label: "Signed in" });
const account = (index) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT", method: "subscription", method_label: "Subscription", url: null, model: null, status: "signed_in", status_label: "Signed in" });
const MAIN = { id: "pool-main", name: "Main pool", summary: "3 ChatGPT", members: [member(0, "api_key"), member(1), member(2)], project_count: 2 };
const SPARE = { id: "pool-spare", name: "Spare", summary: "1 ChatGPT", members: [member(3)], project_count: 0 };
const FULL = { ...MAIN, members: Array.from({ length: 15 }, (_, index) => member(index)) };
const LATE = { status: "not_signed_in", status_label: "Not signed in" };
const WAITING = { ...MAIN, members: [member(0, "api_key"), { ...member(1), ...LATE }, member(2)] };
const POOLS = { ok: [MAIN, SPARE], full: [FULL], waiting: [WAITING], long: [{ ...SPARE, name: "W".repeat(60) }], empty: [] };
const SIGNED_IN = Array.from({ length: 18 }, (_, index) => account(index));
const ACCOUNTS = { ok: SIGNED_IN, waiting: SIGNED_IN.map((item, index) => (index === 1 ? { ...item, ...LATE } : item)), empty: [] };
/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, a failure thrown, or a read that never lands. */
function answer(scene, command) {
  const failing = (state, list) => sceneRead(state, list, FAILURE, command);
  if (command === "desktop_pool_summaries") return failing(scene.pools ?? "ok", POOLS);
  if (command === "desktop_list_accounts") {
    const accounts = failing(scene.accounts ?? "ok", ACCOUNTS);
    return accounts instanceof Promise ? accounts : { accounts, next: null };
  }
  if (command === "desktop_inspect_model_access" || command === "desktop_model_access_page") throw FAILURE;
  throw `Command ${command} not found`;
}

/** Mounts the real Settings page on Pools against a fake engine. Inputs: the scene, naming the
 * pools and accounts reads as ok, full, waiting, long, empty, error, missing or pending, and
 * the theme. Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  await mountTab(createElement(Settings, { tab: "pools" }), (command) => answer(scene, command), theme);
}

/** Reads the first account line. Output: its words and the edges of its name and checkbox. */
export function pick() {
  const row = document.querySelector(".pick");
  return { words: wordsOf(".pick"), nameRight: row.querySelector("span").getBoundingClientRect().right, boxLeft: row.querySelector("input").getBoundingClientRect().left };
}
