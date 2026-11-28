import { createElement } from "react";
import { RestoreSettings } from "../../src/screens/settings/restore.tsx";
import { mountTab, settle } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const APPLIED = { pending: false, outcome: "applied", rejection: null };
let first = true;
let reads = [];
let saves = [];

/** Answers one engine command: a quiet restore status, the compaction switch read Off first and
 * every later read held until `landRead` or `failRead`, and an applied save. Inputs: the command
 * and its arguments. Output: the answer. */
function answer(command, args) {
  if (command === "desktop_restore_status") return { until_ms: null, last: null };
  if (command === "desktop_log_compaction") {
    if (first) {
      first = false;
      return { choice: "off", dev_build: true };
    }
    return new Promise((resolve, reject) => reads.push({ resolve, reject }));
  }
  if (command === "desktop_set_log_compaction") {
    saves.push(args.choice);
    return APPLIED;
  }
  throw `Command ${command} not found`;
}

/** Mounts the real Restore 2.4 page against the fake engine. Inputs: the scene, unused, and the
 * theme. Output: nothing. */
export async function mount(_scene = {}, theme = "light") {
  first = true;
  reads = [];
  saves = [];
  await mountTab(createElement(RestoreSettings), answer, theme);
}

/** Lands every held switch read with a choice. Input: the choice. Output: nothing. */
export async function landRead(choice) {
  for (const read of reads.splice(0)) read.resolve({ choice, dev_build: true });
  await settle(60);
}

/** Fails every held switch read. Output: nothing. */
export async function failRead() {
  for (const read of reads.splice(0)) read.reject("engine_unavailable");
  await settle(60);
}

/** The choices the page saved, in order, and how many switch reads wait. Output: both. */
export function sent() {
  return { saves: [...saves], waiting: reads.length };
}
