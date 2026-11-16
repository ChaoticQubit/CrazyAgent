import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { until } from "./settings-steps.mjs";

const ROWS_MAX = 64;
const MANIFESTS = new URL("../../../../crates/crazyagent/harness-manifests/", import.meta.url);

/** Reads one quoted top-level field of a harness manifest. Inputs: the manifest text and the
 * key. Output: the value, or null when the manifest does not set it. */
function field(text, key) {
  return new RegExp(`^${key}:\\s*"([^"]*)"`, "mu").exec(text)?.[1] ?? null;
}

/** Every harness the bundled manifests name, the catalog the engine serves: its id, its name and
 * the Limited control reasons its manifest gives. Input: none. Output: the harnesses. */
export function bundledHarnesses() {
  const harnesses = readdirSync(MANIFESTS).filter((file) => file.endsWith(".yaml")).map((file) => {
    const text = readFileSync(new URL(file, MANIFESTS), "utf8");
    const [id, name, tools, start] = ["id", "name", "tools_limited", "cannot_start"].map((key) => field(text, key));
    assert.ok(id && name, `${file} names its harness`);
    return { id, name, reasons: [start, tools].filter((reason) => reason !== null) };
  });
  assert.ok(harnesses.length > 0 && harnesses.length <= ROWS_MAX, "a bounded catalog");
  return harnesses;
}

/** Gives each row read off the window the catalog id of the harness it names, so a row can
 * name a harness the way the engine does. Input: the rows. Output: the rows with their id, null
 * for a name no bundled manifest gives. */
function withIds(rows) {
  const ids = new Map(bundledHarnesses().map((harness) => [harness.name, harness.id]));
  return rows.map((row) => ({ id: ids.get(row.name) ?? null, ...row }));
}

/** Reads the harness lines of Settings, Harnesses as the owner sees them: each line's id and
 * name, its status badge word, the accounts it can use, what it says of its tools and every
 * help line. Input: none. Output: the rows, in page order. */
export async function settingsRows() {
  const rows = await browser.execute(() => [...document.querySelectorAll(".set-row")]
    .filter((row) => row.querySelector("button.set-label[aria-expanded]"))
    .map((row) => {
      const lines = [...row.querySelectorAll(".set-help > span")].map((span) => span.textContent.trim());
      const after = (start) => (lines.find((line) => line.startsWith(start)) ?? "").slice(start.length);
      return {
        name: row.getAttribute("data-setting"),
        status: row.querySelector(".set-ctl .badge")?.textContent.trim() ?? "",
        accounts: after("Accounts it can use: "),
        tools: after("Tools: "),
        lines,
      };
    }));
  assert.ok(Array.isArray(rows) && rows.length <= ROWS_MAX, "the Harnesses page lists a bounded number of lines");
  return withIds(rows);
}

/** Reads the harness rows of first run's Harnesses step: each harness's id and name, whether it
 * is picked and whether the owner can change that. Input: none. Output: the rows, in page order. */
export async function firstRunRows() {
  const rows = await browser.execute(() => [...document.querySelectorAll("main input[type=checkbox][aria-label]")]
    .map((box) => ({ name: box.getAttribute("aria-label"), picked: box.checked, locked: box.disabled })));
  assert.ok(Array.isArray(rows) && rows.length <= ROWS_MAX, "the Harnesses step lists a bounded number of rows");
  return withIds(rows);
}

/** Reads one harness picker, the master or a team picker, as the owner sees it: each option's
 * harness id and name, the status group it sits under and whether the owner can pick it.
 * Input: the picker's CSS selector. Output: the options, in picker order. */
export async function pickerRows(selector) {
  const rows = await browser.execute((picker) => [...document.querySelectorAll(`${picker} option`)]
    .map((option) => ({
      id: option.value,
      name: option.textContent.trim(),
      status: option.closest("optgroup")?.getAttribute("label") ?? "",
      disabled: option.disabled,
    })), selector);
  assert.ok(Array.isArray(rows) && rows.length <= ROWS_MAX, `${selector} lists a bounded number of harnesses`);
  return rows;
}

/** Waits until one harness line of Settings, Harnesses reaches a state. Inputs: the harness
 * name, the test on its row, what the wait is for and the time to wait. Output: the row. */
export async function untilSettingsRow(name, state, what, timeout = 30000) {
  let row;
  await until(async () => {
    row = (await settingsRows()).find((item) => item.name === name);
    return row !== undefined && state(row);
  }, timeout, `${name} did not show ${what}`);
  return row;
}
