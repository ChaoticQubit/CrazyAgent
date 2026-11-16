import assert from "node:assert/strict";
import { statSync } from "node:fs";
import { join } from "node:path";
import { refusedGraphifyPaths } from "./run-layout.mjs";
import { addAccountFromFirstRun, click, passHarnessStep, reachToolsStep, says, settingsTab, shot, shown, skipToolsStep, text, until } from "./settings-steps.mjs";

const CHECK_MS = 180000;
const LABEL = "Graphify command";
const FIRST_RUN_ROW = `//li[contains(concat(" ", normalize-space(@class), " "), " in-row ")][.//strong[normalize-space(.)="${LABEL}"]]`;
const PACK_ROW = `//div[contains(concat(" ", normalize-space(@class), " "), " set-row ")][@data-setting="${LABEL}"]`;
const fixButton = (row) => `${row}//button[normalize-space(.)="Fix permissions"]`;
const DIALOG_FIX = `//dialog[@open]//button[normalize-space(.)="Fix permissions"]`;
const DIALOG_CANCEL = `//dialog[@open]//button[normalize-space(.)="Cancel"]`;
const { prefix, outside } = refusedGraphifyPaths(process.env.HOME);

/** Reads the visible text of the first element an XPath names, as the owner reads that row.
 * Input: the XPath. Output: its text, empty when it is not shown. */
function rowText(xpath) {
  return browser.execute((xpath) => {
    const node = document.evaluate(xpath, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
    return node && node.getClientRects().length ? node.textContent : "";
  }, xpath);
}

/** Reads a path's permission bits from the suite's home. Input: the path. Output: the mode. */
function mode(path) {
  return statSync(path).mode & 0o777;
}

/** Calls one desktop command the way the window does, bypassing every button, and keeps a
 * refusal as data. Inputs: the command and its arguments. Output: { value } or { failed }. */
function invoke(command, args) {
  return browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: JSON.stringify(failure) }));
  }, command, args);
}

/** Checks the open confirm the way the owner reads it before a fix: it names the tool, the tree
 * the engine changes and the command, and nothing changed yet. Input: none. */
async function confirmNamesTheTree() {
  await until(() => shown(DIALOG_FIX), 15000, "Fix permissions opened no confirm");
  const words = await text("dialog[open]");
  assert.match(words, new RegExp(`Fix permissions for ${LABEL}\\?`));
  assert(words.includes(`write access for group and others from ${prefix}`), `the confirm names the tree: ${words}`);
  assert(words.includes(`chmod -R go-w`), `the confirm shows the command: ${words}`);
  assert.equal(mode(prefix), 0o775, "nothing changes before the confirm");
}

describe("DEC-162: a graphify found and passed over, on the real desktop app and a clean-Mac gate engine", () => {
  it("first run's Set up tools reads it as Found, not used, and Cancel on its Fix permissions confirm changes nothing", async function () {
    this.timeout(CHECK_MS + 60000);
    await reachToolsStep("Refused Tool Owner");
    await until(async () => (await rowText(FIRST_RUN_ROW)).includes("Found, not used"), CHECK_MS,
      "the Graphify command row never read Found, not used; the owner fixture must be built with --features crazyagent/clean-mac-gate");
    assert.match(await rowText(FIRST_RUN_ROW), /graphify can't start/);
    await click(fixButton(FIRST_RUN_ROW));
    await confirmNamesTheTree();
    await shot("refused-tool-first-run-confirm");
    await click(DIALOG_CANCEL);
    await until(async () => !(await shown("dialog[open]")), 15000, "Cancel did not close the confirm");
    assert.equal(mode(prefix), 0o775, "Cancel changes nothing");
    await skipToolsStep();
    await passHarnessStep();
    await until(() => says("h1", "Add an account"), 15000, "the accounts step did not open");
    await addAccountFromFirstRun();
    await click("dialog[open] button[aria-label='Close']");
  });

  it("Settings, Pack with Unused tools on Show the command only shows the command, offers no fix and the engine refuses one", async function () {
    this.timeout(CHECK_MS + 60000);
    await settingsTab("Pack");
    await until(async () => (await rowText(PACK_ROW)).includes("Found, not used"), CHECK_MS, "Settings, Pack never showed the passed-over graphify");
    await click("label=Show the command only");
    await until(async () => (await rowText(PACK_ROW)).includes("To fix it, run in Terminal: chmod -R go-w"), 30000,
      "the Graphify command row did not show the command once Unused tools was Show the command only");
    assert.equal(await shown(fixButton(PACK_ROW)), false, "command only offers no Fix permissions");
    const answer = await invoke("desktop_fix_tool_permissions", { name: "graphify", tree: prefix, recursive: true, generation: 1 });
    assert.match(answer.failed ?? "", /tool_fix_off/, `the engine refuses a fix while fixing is off: ${JSON.stringify(answer)}`);
    assert.equal(mode(prefix), 0o775, "a refused fix changes nothing");
    await shot("refused-tool-command-only");
  });

  it("Show a fix button brings Fix permissions back, and its confirm fixes only the tree it named", async function () {
    this.timeout(CHECK_MS + 60000);
    await click("label=Show a fix button (recommended)");
    await until(() => shown(fixButton(PACK_ROW)), 30000, "Fix permissions did not come back with Show a fix button");
    assert.doesNotMatch(await rowText(PACK_ROW), /To fix it, run in Terminal/);
    await click(fixButton(PACK_ROW));
    await confirmNamesTheTree();
    await click(DIALOG_FIX);
    await until(async () => {
      const row = await rowText(PACK_ROW);
      return row.includes("Version 0.9.9 on this Mac.") && !row.includes("Found, not used") && !(await shown(fixButton(PACK_ROW)));
    }, CHECK_MS, "the Graphify command row did not read Found once the fix ended");
    for (const folder of [prefix, join(prefix, "bin")]) assert.equal(mode(folder), 0o755, folder);
    assert.equal(mode(outside), 0o777, "a link out of the tree leaves its file as it was");
    await shot("refused-tool-fixed");
  });
});
