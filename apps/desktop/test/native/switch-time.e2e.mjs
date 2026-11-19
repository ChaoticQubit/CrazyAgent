import assert from "node:assert/strict";
import { isAbsolute } from "node:path";
import { invoke, newProject, prepareWindow } from "./master-settings-window.mjs";
import { measureRow, nativeViewShowing, onScreen } from "./switch-time.mjs";
import { go, paste, pick, reopen, run, tab, until, waitFor } from "./trial-kit.mjs";
import { startIfPaused, terminalLive } from "./trial4/n-d8-master-reply.mjs";

const evidence = process.env.MS_EVIDENCE;
const START_MS = 180000;
const UI_MS = 30000;
const CONVERSATION = "a#tab-conversation";
const HOME = "#/home";
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };

/** The stand-in harness's ready line: the marker a switch back must put on screen again. */
const MARKER = "stand-in ready";

/** The live master's webview terminal with the marker on it. */
const MASTER_TERMINAL = { scope: ".pj-master-live .tg-terminal-screen", rows: ".tg-terminal-host", text: MARKER };

/** The live master's terminal the rows time: the webview one, or on a build that draws
 * terminals natively (DEC-203) its native view, `{ view, text }`, found once it shows. */
let target = MASTER_TERMINAL;

/** Waits until the master's terminal shows the marker and sets `target`: on a native build
 * the page's own embedded panel goes live and one native view shows it; else the webview
 * terminal. Prints the kind line a run is void without. Output: none. */
async function masterShown() {
  if (!(await invoke("desktop_native_terminal_enabled", {}))) {
    await terminalLive(kit, START_MS);
    await until(() => onScreen(MASTER_TERMINAL), START_MS, "the master terminal never showed the stand-in's ready line");
    console.log("SWITCH-TIME terminal kind web");
    return;
  }
  await until(() => browser.execute(() => document.querySelector(".pj-master-live section.tg-terminal.embedded .tg-terminal-note") !== null), START_MS,
    "the page's master panel never went live");
  let found = null;
  await until(async () => (found = await nativeViewShowing(MARKER)) !== null, START_MS, "no native view showed the stand-in's ready line");
  target = { view: found.view, text: MARKER };
  console.log(`SWITCH-TIME terminal kind native, view ${found.view}, direct ${found.direct}`);
}

describe("Switching back to a live terminal in the real window (DEC-187)", () => {
  before(async () => {
    assert.ok(isAbsolute(evidence ?? ""), "MS_EVIDENCE names the evidence folder");
    await prepareWindow();
    await newProject("switch-time-shop");
    await tab("conversation");
    await startIfPaused(kit, run.project);
    await masterShown();
  });

  it("tab switch: Overview and back to Conversation shows the terminal within the limit", async () => {
    const result = await measureRow({ name: "terminal-tab", away: () => tab("overview"), back: CONVERSATION, target, evidence });
    assert.ok(result.pass, result.line);
  });

  it("page switch: Home, the project again, then Conversation shows the terminal within the limit", async () => {
    const away = async () => {
      await go(HOME);
      await tab("overview");
      await until(() => browser.execute((selector) => document.querySelector(selector)?.getAttribute("aria-selected") === "false", CONVERSATION), UI_MS,
        "the project page never came back on another tab than Conversation");
    };
    const result = await measureRow({ name: "terminal-page", away, back: CONVERSATION, target, evidence });
    assert.ok(result.pass, result.line);
  });
});
