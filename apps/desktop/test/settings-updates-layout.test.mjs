import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { onTab, press, readable, shown, walk } from "./fixtures/settings-app-tabs-run.mjs";

const INSTALL = "Install command line tool";
const channelRow = (seen) => seen.sections[0]?.rows.find((row) => row.label === "Update channel");

test("Updates shows its title, then Version and channel, Updates and Command line tool in order", async () => {
  const seen = await onTab("updates", {}, shown);
  assert.equal(seen.top?.heading, "Updates", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.length > 0, JSON.stringify(seen.top));
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Version and channel", "Updates", "Command line tool"]);
  for (const part of ["CrazyAgent 0.2.0", "Engine 0.2.0", "CrazyAgent checks for updates when it starts and every 6 hours. It never installs one without you.", "Check now"]) {
    assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  }
});

test("the channel stands on the right of its words and Install command line tool on the right of its section", async () => {
  const seen = await onTab("updates", {}, shown);
  const row = channelRow(seen);
  assert.notEqual(row, undefined, "the channel is a row of Version and channel");
  assert.equal(row.help, "Stable gets tested releases. Dev gets every change as soon as it's merged.", JSON.stringify(row));
  assert.equal(row.radios, 2);
  assert.ok(row.controlLeft > row.labelRight, JSON.stringify(row));
  const tool = seen.sections[2];
  assert.equal(tool.about, "Use crazyagent in Terminal, for example crazyagent tui. It talks to the same engine as this app.");
  assert.deepEqual(tool.head, [INSTALL]);
  assert.ok(tool.headRight > tool.titleRight, JSON.stringify(tool));
});

test("the changed mark shows on Dev and never on Stable, the engine's default", async () => {
  const onStable = await onTab("updates", {}, shown);
  assert.equal(channelRow(onStable)?.mark, null, JSON.stringify(onStable.sections));
  const onDev = await onTab("updates", { channel: "dev" }, shown);
  assert.equal(channelRow(onDev)?.mark, "changed", JSON.stringify(onDev.sections));
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/update.rs", import.meta.url), "utf8");
  assert.match(engine, /None => Ok\(Channel::Stable\)/, "the mark's default is the engine's default");
  assert.match(engine, /ErrorKind::NotFound => return Ok\(Channel::Stable\)/);
  const source = readFileSync(new URL("../src/screens/settings/updates.tsx", import.meta.url), "utf8");
  assert.match(source, /UPDATE_CHANNEL_DEFAULT: UpdateChannel = "stable"/);
});

for (const [name, expected, missing] of [
  ["current", ["CrazyAgent is up to date.", "Check now"], "Download and install"],
  ["available", ["CrazyAgent 0.3.0 is available. You have 0.2.0.", "Faster startup.", "Installing stops the engine for a moment.", "Download and install", "Check again", "Update channel"], "Channel: Stable"],
  ["installing", ["Installing CrazyAgent 0.3.0.", "Downloading 5 bytes of 10 bytes (50%)", "Channel: Stable"], "Check now"],
  ["installed", ["CrazyAgent 0.3.0 is installed. Restart to finish.", "The engine starts again when CrazyAgent restarts.", "Restart now"], "Check now"],
  ["failed", ["Try again"], "Check now"],
]) {
  test(`the ${name} update stays in the Updates section with its words`, async () => {
    const seen = await onTab("updates", {}, async (page) => {
      await page.evaluate(`Fixture.phase(${JSON.stringify(name)})`);
      return { ...(await shown(page)), sectionWords: await page.evaluate("[...document.querySelectorAll('.set-group')].find((group) => group.querySelector('h3')?.textContent === 'Updates')?.innerText ?? ''") };
    });
    assert.equal(seen.top?.heading, "Updates", JSON.stringify(seen.top));
    for (const part of expected.filter((item) => !item.startsWith("Channel") && !item.startsWith("Update channel"))) {
      assert.ok(seen.sectionWords.replace(/\s+/g, " ").includes(part), `${part} in ${seen.sectionWords}`);
    }
    for (const part of expected.filter((item) => item.startsWith("Channel") || item.startsWith("Update channel"))) {
      assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
    }
    assert.ok(!seen.says.includes(missing), `${missing} in ${seen.says}`);
  });
}

for (const [state, scene, expected] of [
  ["failed", { updates: "error" }, ["Try again"]],
  ["not in this build", { updates: "missing" }, ["Updates isn't ready yet"]],
  ["engine mismatch", { engine: "differs" }, ["The engine is on another version.", "Restart engine"]],
]) {
  test(`Updates keeps its ${state} state under the title`, async () => {
    const seen = await onTab("updates", scene, shown);
    assert.equal(seen.top?.heading, "Updates", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

test("a refused channel and a failed tool install each say so inside their section", async () => {
  const seen = await onTab("updates", {}, async (page) => {
    await press(page, '[role="radio"]', "Dev");
    const channel = await page.evaluate("Fixture.alert()");
    await press(page, "button", INSTALL);
    return { channel, says: await page.evaluate("[...document.querySelectorAll('.set-group')].at(-1)?.querySelector('[role=\"alert\"]')?.textContent ?? ''") };
  });
  assert.ok(seen.channel.words.length > 0, JSON.stringify(seen));
  assert.equal(seen.channel.belowTop, true, JSON.stringify(seen));
  assert.ok(seen.says.length > 0, JSON.stringify(seen));
});

test("real Tab keys go channel, Check now, then Install command line tool", async () => {
  const seen = await onTab("updates", {}, (page) => walk(page, 3));
  assert.deepEqual(seen, ["Stable", "Check now", INSTALL]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose, help and changed mark read at 4.5 to 1 on Updates in ${theme}`, async () => {
    await onTab("updates", { channel: "dev" }, (page) => readable(page, [".settings-top p", ".set-help", ".set-changed", ".set-head p"]), theme);
  });
}
