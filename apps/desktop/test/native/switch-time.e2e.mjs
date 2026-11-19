import assert from "node:assert/strict";
import { isAbsolute } from "node:path";
import { newProject, prepareWindow } from "./master-settings-window.mjs";
import { measureRow, onScreen } from "./switch-time.mjs";
import { go, paste, pick, reopen, run, tab, until, waitFor } from "./trial-kit.mjs";
import { startIfPaused, terminalLive } from "./trial4/n-d8-master-reply.mjs";

const evidence = process.env.MS_EVIDENCE;
const START_MS = 180000;
const UI_MS = 30000;
const CONVERSATION = "a#tab-conversation";
const HOME = "#/home";
const kit = { go, tab, pick, paste, reopen, until, waitFor, project: () => run.project };

/** The live master's terminal with the stand-in harness's ready line on it: the marker a
 * switch back must put on screen again. */
const MASTER_TERMINAL = { scope: ".pj-master-live .tg-terminal-screen", rows: ".tg-terminal-host", text: "stand-in ready" };

describe("Switching back to a live terminal in the real window (DEC-187)", () => {
  before(async () => {
    assert.ok(isAbsolute(evidence ?? ""), "MS_EVIDENCE names the evidence folder");
    await prepareWindow();
    await newProject("switch-time-shop");
    await tab("conversation");
    await startIfPaused(kit, run.project);
    await terminalLive(kit, START_MS);
    await until(() => onScreen(MASTER_TERMINAL), START_MS, "the master terminal never showed the stand-in's ready line");
  });

  it("tab switch: Overview and back to Conversation shows the terminal within the limit", async () => {
    const result = await measureRow({ name: "terminal-tab", away: () => tab("overview"), back: CONVERSATION, target: MASTER_TERMINAL, evidence });
    assert.ok(result.pass, result.line);
  });

  it("page switch: Home, the project again, then Conversation shows the terminal within the limit", async () => {
    const away = async () => {
      await go(HOME);
      await tab("overview");
      await until(() => browser.execute((selector) => document.querySelector(selector)?.getAttribute("aria-selected") === "false", CONVERSATION), UI_MS,
        "the project page never came back on another tab than Conversation");
    };
    const result = await measureRow({ name: "terminal-page", away, back: CONVERSATION, target: MASTER_TERMINAL, evidence });
    assert.ok(result.pass, result.line);
  });
});
