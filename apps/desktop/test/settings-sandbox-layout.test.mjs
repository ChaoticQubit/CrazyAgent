import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { onTab, press, readable, shown, walk } from "./fixtures/settings-app-tabs-run.mjs";

const LABEL = "Linux limited-network sockets";
const HELP = "How an agent limited to its model reaches the network when CrazyAgent runs on Linux. Landlock limits TCP by port only.";
const REFUSE = 'input[value="refuse"]';

test("Sandbox shows its title, then Network with the setting's words above its three choices", async () => {
  const seen = await onTab("sandbox", {}, async (page) => ({ ...(await shown(page)), border: await page.evaluate("getComputedStyle(document.querySelector('.set-line-ctl > fieldset') ?? document.body).borderTopWidth") }));
  assert.equal(seen.top?.heading, "Sandbox", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.length > 0, JSON.stringify(seen.top));
  assert.equal(seen.border, "0px", "the browser's fieldset box does not frame the choices");
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Network"]);
  const [row] = seen.sections[0].rows;
  assert.equal(row.label, LABEL);
  assert.equal(row.help, HELP);
  assert.equal(row.radios, 3);
  assert.ok(row.controlTop >= row.labelBottom, `the choices stand under the words: ${JSON.stringify(row)}`);
  for (const part of ["TCP only, names looked up over TCP (recommended)", "Do not start limited-network agents", "TCP ports only", "Save changes"]) {
    assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  }
});

test("the changed mark shows only away from the engine's default, for a stored value and a fresh pick", async () => {
  const mark = (page) => page.evaluate("Fixture.sections()[0]?.rows[0]?.mark ?? null");
  const onDefault = await onTab("sandbox", {}, async (page) => {
    const before = await mark(page);
    await press(page, REFUSE);
    return { before, picked: await mark(page) };
  });
  assert.equal(onDefault.before, null);
  assert.equal(onDefault.picked, "changed");
  assert.equal(await onTab("sandbox", { stored: "tcp_ports" }, mark), "changed");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox.rs", import.meta.url), "utf8");
  assert.match(engine, /pub enum LinuxSockets \{\s*#\[default\]\s*TcpOnly,/, "the mark's default is the engine's default");
  const source = readFileSync(new URL("../src/screens/settings/sandbox.tsx", import.meta.url), "utf8");
  assert.match(source, /LINUX_SOCKETS_DEFAULT: LinuxSockets = "tcp_only"/);
});

test("a refused save keeps the problem note on the page", async () => {
  const seen = await onTab("sandbox", {}, async (page) => {
    await press(page, REFUSE);
    await press(page, 'button[type="submit"]');
    return page.evaluate("Fixture.alert()");
  });
  assert.ok(seen.words.length > 0, JSON.stringify(seen));
  assert.equal(seen.belowTop, true, JSON.stringify(seen));
});

for (const [state, scene, expected] of [
  ["failed", { sandbox: "error" }, ["Couldn't load the sandbox settings.", "Try again"]],
  ["not in this build", { sandbox: "missing" }, ["The sandbox settings isn't ready yet"]],
]) {
  test(`Sandbox keeps its ${state} state under the title`, async () => {
    const seen = await onTab("sandbox", scene, shown);
    assert.equal(seen.top?.heading, "Sandbox", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

test("real Tab keys reach the choices, then Save changes once a pick differs", async () => {
  const seen = await onTab("sandbox", {}, async (page) => {
    await press(page, REFUSE);
    return walk(page, 2);
  });
  assert.deepEqual(seen, ["refuse", "Save changes"]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose, help and changed mark read at 4.5 to 1 on Sandbox in ${theme}`, async () => {
    await onTab("sandbox", { stored: "refuse" }, (page) => readable(page, [".settings-top p", ".set-help", ".set-changed"]), theme);
  });
}
