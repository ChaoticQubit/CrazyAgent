import assert from "node:assert/strict";
import { click, control, finishWelcome, observeModelAccess, pick, settingsTab, shot, text, type, until } from "./settings-steps.mjs";

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
    await until(() => control("#account-name"), 15000, "the sign-in form did not open");
    await type("#account-name", "ChatGPT refused");
    await click("button=Get sign-in code");
    await until(async () => (await text("dialog[open]")).includes(READ_ONLY), 15000, "the refusal was not shown");
    assert.equal(await control("#account-name"), true, "the form stays open with the owner's input");
    await shot("sign-in-refused");
    await nothingSaved();
    await click("dialog[open] button[aria-label='Close']");
  });

  it("saving a new pool is refused in plain words and nothing is saved", async () => {
    await settingsTab("Pools");
    await click("button=New pool");
    await type("#pool-name", "Refused pool");
    await click("button=Save pool");
    await until(async () => (await text("main [role=alert]")).includes(`Couldn't save this pool. ${READ_ONLY}`), 15000, "the pool refusal was not shown");
    assert.equal(await control("#pool-name"), true, "the editor stays open with the owner's input");
    await shot("pool-refused");
    await nothingSaved();
  });
});
