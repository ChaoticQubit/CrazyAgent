import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { AgentSettingsView } from "../../src/screens/process.tsx";
import { AccountChoiceView, ProjectAccountOrderView } from "../../src/screens/settings/account-choice.tsx";
import { MemoryView } from "../../src/screens/settings/memory.tsx";
import { UpdatesView } from "../../src/screens/settings/updates.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const SCHEDULE = { strategy: "manual_rank", pools: [{ id: "pool-main", strategy: "balanced" }], projects: [{ id: "project-a", strategy: "use_it_or_lose_it" }], accounts: [], version: 3 };
const POOL = { id: "pool-main", name: "Main", summary: "1 Claude", members: [], project_count: 1 };
const nothing = () => undefined;
const MEMORY = { saved: { kind: "ok", value: { items: [], next: null, agent_writes: "save" } }, suggested: { kind: "ok", value: [] }, scope: "", projects: [], search: "", failure: "", busy: null,
  onSearch: nothing, onScope: nothing, onAdd: nothing, onSave: nothing, onSaveEdited: nothing, onDismiss: nothing, onDelete: nothing, onMore: nothing, onRetry: nothing, onImport: nothing };
const DELIVERY = { kind: "ok", value: { settings: { mode: "both", transcripts: true, edits: "next_message" }, version: 1 } };
const UPDATES = { settings: { kind: "ok", value: { current_version: "0.2.0", channel: "dev" } }, phase: { kind: "idle" }, channelBusy: false, channelProblem: "", cli: { kind: "idle" },
  onCheck: nothing, onInstall: nothing, onRestart: nothing, onLinkCli: nothing, onRetry: nothing, engine: { kind: "ok", value: { engine: "0.2.0", app: "0.2.0", message: null } }, engineRestart: { kind: "idle" }, onRestartEngine: nothing };
let root = null;
let asked = 0;
let waiting = [];
const pause = (ms) => new Promise((done) => setTimeout(done, ms));

/** Mounts one app settings view whose read never comes back after a save, so what stays on
 * screen is the pick. Inputs: "schedule", "project", "memory", "delivery" or "updates", whether
 * a save is refused and whether its answers wait for `answer`. Output: nothing. */
export async function mount(view, refused, manual = false) {
  root?.unmount();
  asked = 0;
  waiting = [];
  document.body.innerHTML = '<div id="mount"></div>';
  root = createRoot(document.getElementById("mount"));
  const reply = (value) => {
    asked += 1;
    return manual ? new Promise((done) => waiting.push(() => done(value))) : Promise.resolve(value);
  };
  const answer = () => reply(refused ? { kind: "error", code: "refused", message: "Refused." } : { kind: "ok", value: undefined });
  const note = () => reply(refused ? "Refused." : "");
  const views = {
    schedule: [AccountChoiceView, { schedule: { kind: "ok", value: SCHEDULE }, pools: { kind: "ok", value: [POOL] }, names: new Map(), onChange: answer, onRetry: nothing }],
    project: [ProjectAccountOrderView, { schedule: { kind: "ok", value: SCHEDULE }, project: "project-a", pool: "pool-main", onChange: answer }],
    memory: [MemoryView, { ...MEMORY, onWrites: note }],
    delivery: [AgentSettingsView, { settings: DELIVERY, busy: false, failure: "", onSave: note }],
    updates: [UpdatesView, { ...UPDATES, onChannel: note }],
  };
  const [component, props] = views[view];
  await act(async () => { root.render(createElement(component, props)); });
}

/** Answers one save that waits. Input: its place in the order the saves were asked. Output:
 * nothing. */
export async function answer(index) {
  await act(async () => {
    waiting[index]();
    await pause(20);
  });
}

/** Picks a value in one select as the owner does and lets its save run. Inputs: the select's id
 * and the value. Output: the value before, the value shown after, how many saves were asked and
 * the help line under the select. */
export async function pick(id, value) {
  const select = document.getElementById(id);
  const before = select.value;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await pause(20);
  });
  const now = document.getElementById(id);
  return { before, shown: now.value, asked, helper: now.closest(".field")?.querySelector(".helper")?.textContent ?? null };
}

function checkedIn(label) {
  return document.querySelector(`[role="radiogroup"][aria-label="${label}"] [aria-checked="true"]`).textContent.trim();
}

/** Clicks the first unchecked option of a segmented row and lets its save run. Input: the
 * group's label. Output: the option before, the one picked, the one shown after and how many
 * saves were asked. */
export async function pickSegment(label) {
  const before = checkedIn(label);
  const other = [...document.querySelectorAll(`[role="radiogroup"][aria-label="${label}"] [role="radio"]`)].find((radio) => radio.getAttribute("aria-checked") !== "true");
  const picked = other.textContent.trim();
  await act(async () => {
    other.click();
    await pause(20);
  });
  return { before, picked, shown: checkedIn(label), asked };
}

/** Clicks a switch and lets its save run. Input: its id. Output: the state before, the state
 * shown after and how many saves were asked. */
export async function flip(id) {
  const before = document.getElementById(id).getAttribute("aria-checked");
  await act(async () => {
    document.getElementById(id).click();
    await pause(20);
  });
  return { before, shown: document.getElementById(id).getAttribute("aria-checked"), asked };
}
