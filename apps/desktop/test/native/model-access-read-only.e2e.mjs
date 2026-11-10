import assert from "node:assert/strict";
import { click, finishWelcome, observeModelAccess, pick, says, settingsTab, shot, shown, text, type, until } from "./settings-steps.mjs";

const READ_ONLY = "Settings can't be changed in this engine. Start it with writable settings.";

async function nothingSaved() {
  const engine = await observeModelAccess();
  assert.equal(engine.writable, false);
  assert.deepEqual([engine.registry, engine.accounts, engine.connections, engine.pools], [[], [], [], []]);
}

describe("Settings, Accounts and Pools on the real desktop app and a read-only engine", () => {
  it("first run ends on Add account and the engine reports read-only settings", async () => {
    await finishWelcome("Read Only Owner");
    await nothingSaved();
  });

  it("starting a ChatGPT sign-in is refused in plain words and nothing is saved", async () => {
    await pick("tile", "OpenAI");
    await pick("method", "Sign in with ChatGPT");
    await until(() => shown("#account-name"), 15000, "the sign-in form did not open");
    await type("#account-name", "ChatGPT refused");
    await click("button=Get sign-in code");
    await until(() => says("dialog[open] #account-name-error", READ_ONLY), 15000, "the refusal was not shown");
    assert.equal(await $("#account-name").getValue(), "ChatGPT refused", "the form stays open with the owner's input");
    await shot("sign-in-refused");
    await nothingSaved();
    await click("dialog[open] button[aria-label='Close']");
  });

  it("a new pool with no account to add stays a draft and nothing is saved", async () => {
    await settingsTab("Pools");
    await click("button=New pool");
    await until(() => says("main .pool-item.on .pool-item-label", "New pool"), 15000, "the new pool was not selected at once");
    await until(async () => (await text(".pool-add")).includes("Add an account first, then put it in a pool."), 15000, "the add panel did not say to add an account first");
    await shot("pool-draft-read-only");
    await nothingSaved();
  });
});
