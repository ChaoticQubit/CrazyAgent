import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { press, tabRunner } from "./settings-tab-steps.mjs";

const state = await import("../src/state.ts");
const shown = await import("../src/questions-shown.ts");
const fixture = await bundle(new URL("./fixtures/settings-access-layout.mjs", import.meta.url).pathname);
const onProfile = tabRunner(fixture);
const NOT_KEPT = "CrazyAgent couldn't keep this choice, so it stays as it was. Try again.";
const THEME_NOT_KEPT = "This theme shows now, but CrazyAgent couldn't keep it, so the next start uses the one before. Try again.";
const DENIED = `(() => { try { return window.localStorage.length < 0; } catch (error) { return error.name === "SecurityError"; } })()`;
const lineOf = (label) => `document.querySelector('[data-setting="${label}"]')`;
const checked = (label) => `[...${lineOf(label)}.querySelectorAll('[role=radiogroup] button[aria-checked="true"]')].map((button) => button.textContent.trim())`;
const said = (label, words) => `${lineOf(label)}.nextElementSibling?.querySelector('[role=alert]')?.textContent.includes(${JSON.stringify(words)}) ?? false`;
const other = (label) => `[...${lineOf(label)}.querySelectorAll('[role=radiogroup] button[aria-checked="false"]')][0].textContent.trim()`;

/** Puts a stand-in storage on the window for one call, the document's root for the theme, and
 * gives both back after. Inputs: whether a write throws, and the call. Output: what it returns. */
function withStorage(full, run) {
  const kept = new Map();
  const write = (key, value) => { if (full) throw new Error("full"); kept.set(key, value); };
  const remove = (key) => { if (full) throw new Error("full"); kept.delete(key); };
  globalThis.window = { localStorage: { getItem: (key) => kept.get(key) ?? null, setItem: write, removeItem: remove }, addEventListener: () => {} };
  globalThis.document = { documentElement: { setAttribute: () => {}, removeAttribute: () => {} } };
  try {
    return run(kept);
  } finally {
    delete globalThis.window;
    delete globalThis.document;
  }
}

test("each choice kept in this app says whether it was stored: true after the write, false when storage refuses it", () => {
  const saves = [
    () => shown.saveQuestionsShown("tab"),
    () => state.saveTerminalInput("both"),
    () => state.saveTerminalRenderer("standard"),
    () => state.chooseTheme("dark"),
  ];
  for (const save of saves) {
    assert.equal(withStorage(false, save), true, String(save));
    assert.equal(withStorage(true, (kept) => [save(), kept.size]).join(), "false,0", String(save));
  }
});

test("a Questions show, Terminal input or Terminal renderer choice the app cannot keep stays on the kept one and says so on its line", async () => {
  const seen = await onProfile({ tab: "profile" }, async (page) => {
    assert.equal(await page.evaluate(DENIED), true, "the page's own storage refuses, as a browser that keeps no site data does");
    const lines = [];
    for (const label of ["Questions show", "Terminal input", "Terminal renderer"]) {
      const wanted = await page.evaluate(other(label));
      await press(page, `[data-setting="${label}"] [role=radiogroup] button`, wanted);
      lines.push({ label, wanted, now: await page.evaluate(checked(label)), says: await page.evaluate(said(label, NOT_KEPT)) });
    }
    return lines;
  });
  for (const line of seen) {
    assert.notDeepEqual(line.now, [line.wanted], `${line.label} shows the choice it could not keep`);
    assert.equal(line.says, true, `${line.label} says the choice was not kept`);
  }
});

test("a Theme the app cannot keep shows now and its line says the next start uses the one before", async () => {
  const seen = await onProfile({ tab: "profile" }, async (page) => {
    assert.equal(await page.evaluate(DENIED), true, "the page's own storage refuses, as a browser that keeps no site data does");
    await press(page, '[data-setting="Theme"] [role=radiogroup] button', "Dark");
    return {
      theme: await page.evaluate("document.documentElement.dataset.theme"),
      now: await page.evaluate(checked("Theme")),
      says: await page.evaluate(said("Theme", THEME_NOT_KEPT)),
    };
  });
  assert.deepEqual(seen, { theme: "dark", now: ["Dark"], says: true });
});
