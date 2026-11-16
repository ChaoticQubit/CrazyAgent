import assert from "node:assert/strict";
import { bundledHarnesses, settingsRows, untilSettingsRow } from "./harness-rows.mjs";
import { click, finishWelcome, observeModelAccess, pick, settingsTab, shot, shown, text, texts, type, until } from "./settings-steps.mjs";

const NAME = "Gemini dummy";
const KEY = `AIza${"T21DummyKeyNeverReal".padEnd(35, "x")}`;
const MODEL = "gemini-2.5-pro";
const GOOGLE = /^https:\/\/generativelanguage\.googleapis\.com\/?$/u;
const LIMITED = "Limited control: ";
const STATUS = /^(Ready|Not installed|Can't run.*|Limited control: .+)$/u;

describe("Task 21: harness states and the Gemini API key tile on the real desktop app and engine", () => {
  it("Add account offers the Gemini API key right after Anthropic and saves a dummy key account", async () => {
    await finishWelcome("Harness States Owner");
    assert.deepEqual((await texts("dialog[open] button.tile .title")).slice(0, 3), ["OpenAI", "Anthropic", "Gemini API"]);
    await pick("tile", "Gemini API");
    await until(() => shown("#account-key"), 15000, "the Gemini API key form did not open");
    assert.equal(await shown("#account-url"), false, "Google's address is preset, so the form asks for no URL");
    await type("#account-name", NAME);
    await type("#account-key", KEY);
    await type("#account-model", MODEL);
    await click("button=Save");
    await until(async () => (await texts("dialog[open] .success strong")).includes(NAME), 30000, "the Gemini API account was not saved");
    await shot("gemini-key-saved");
  });

  it("the key never shows again, in the window or in what the engine keeps", async () => {
    const values = await browser.execute(() => [...document.querySelectorAll("input, textarea")].map((input) => input.value));
    assert.ok(!values.some((value) => value.includes(KEY)), "no field keeps the key once it is saved");
    await click("button=Done");
    await until(async () => (await texts("main .account-row .cell .ellipsis")).includes(NAME), 15000, "the account list did not show the Gemini API account");
    assert.ok(!(await text("body")).includes(KEY), "the window never shows the key");
    const engine = await observeModelAccess();
    const saved = engine.registry.find((account) => account.name === NAME);
    assert.ok(saved, "the engine keeps the Gemini API account");
    assert.deepEqual([saved.provider, saved.model], ["gemini", MODEL]);
    assert.match(saved.url, GOOGLE, "the account keeps Google's preset address");
    assert.ok(engine.connections.some((connection) => connection.protocol === "gemini"), "its connection speaks the Gemini wire format");
    assert.ok(!JSON.stringify(engine).includes(KEY), "what the engine keeps names the key by reference only");
    await shot("gemini-key-listed");
  });

  it("Settings, Harnesses lists every catalog harness with its status label", async () => {
    await settingsTab("Harnesses");
    const bundled = bundledHarnesses();
    await until(async () => (await settingsRows()).length === bundled.length, 30000, "the Harnesses page did not list every catalog harness");
    const rows = await settingsRows();
    assert.deepEqual(rows.map((row) => row.id).sort(), bundled.map((harness) => harness.id).sort());
    for (const row of rows) {
      const harness = bundled.find((item) => item.id === row.id);
      assert.match(row.status, STATUS, row.name);
      if (row.status.startsWith(LIMITED)) assert.ok(harness.reasons.includes(row.status.slice(LIMITED.length)), `${row.name}: ${row.status}`);
      assert.ok(row.accounts.length > 0, `${row.name} says which accounts it can use`);
    }
    await untilSettingsRow("RCLI", (row) => row.tools === "runs without tools", "that its tools stay off");
    await shot("harness-states");
  });
});
