import assert from "node:assert/strict";
import test from "node:test";
import { CONTRAST_MIN, argsOf, contrasts, count, onTab, press, read, say, tabStops, top } from "./settings-agents-page.mjs";

const PURPOSE = "CrazyAgent finds harnesses installed on this Mac. It does not install them. It can update one with that harness's own updater when you choose Update.";
const on = (scene, check, width, theme) => onTab("harnesses", scene, check, width, theme);
const CHECK = ".settings-top button";

test("the tab shows its title, purpose, last check and Check again, then one section per group", async () => {
  const seen = await on({}, async (page) => ({ top: await top(page), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Harnesses", JSON.stringify(seen.top));
  assert.equal(seen.top.purpose, PURPOSE);
  assert.deepEqual(seen.top.actions, ["Check again"]);
  assert.match(seen.top.says, /Checked just now/);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Ready (2)", "Limited control (1)", "Not installed (1)"]);
  assert.equal(seen.sections.every((section) => section.mark === null), true, "harnesses have no default to compare");
});

test("a group with no harness is left out", async () => {
  const sections = await on({ harnesses: "ready" }, read);
  assert.deepEqual(sections.map((section) => section.heading), ["Ready (1)"]);
});

test("a harness line has its name on the left, what it can do under it and its status on the right", async () => {
  const [ready, limited, absent] = await on({}, read);
  const [codex] = ready.rows;
  assert.equal(codex.label, "Codex");
  assert.ok(codex.controlLeft > codex.labelRight, JSON.stringify(codex));
  assert.equal(codex.control, "Ready");
  for (const part of ["Tools: Can turn on or off", "Accounts it can use: Subscription, API key"]) assert.ok(codex.help.includes(part), `${part} in ${codex.help}`);
  assert.equal(limited.rows[0].control, "Limited control: It has no switch for tools");
  for (const part of ["Tools: Its tools stay on", "Accounts it can use: URL, Local"]) assert.ok(limited.rows[0].help.includes(part), `${part} in ${limited.rows[0].help}`);
  assert.equal(absent.rows[0].control, "Not installed");
  assert.ok(absent.rows[0].help.includes("Accounts it can use: None"), absent.rows[0].help);
});

test("a real press opens a line's details and a second press closes them", async () => {
  const seen = await on({}, async (page) => {
    const closed = await say(page);
    await press(page, "button[aria-expanded]", "Codex");
    const codex = { says: await say(page), rows: await read(page) };
    await press(page, "button[aria-expanded]", "Gemini CLI");
    const gemini = await say(page);
    await press(page, "button[aria-expanded]", "Codex");
    return { closed, codex, gemini, shut: await read(page) };
  });
  assert.ok(!seen.closed.includes("Found at"), seen.closed);
  assert.equal(seen.codex.rows[0].rows[0].expanded, "true");
  for (const part of ["Found at", "~/.local/bin/codex", "Runs in the background with", "codex exec", "Model", "You can choose the model"]) assert.ok(seen.codex.says.includes(part), `${part} in ${seen.codex.says}`);
  for (const part of ["Not found on this Mac", "Chosen inside the harness"]) assert.ok(seen.gemini.includes(part), `${part} in ${seen.gemini}`);
  assert.equal(seen.shut[0].rows[0].expanded, "false");
});

test("while the list loads the title stays, Check again waits and the rows are skeletons", async () => {
  const seen = await on({ harnesses: "pending" }, async (page) => ({ top: await top(page), skeletons: await count(page, ".skeleton"), waiting: await count(page, `${CHECK}:disabled`) }));
  assert.equal(seen.top?.heading, "Harnesses", JSON.stringify(seen.top));
  assert.equal(seen.skeletons, 8);
  assert.equal(seen.waiting, 1);
});

for (const [state, expected] of [["missing", ["The harness list isn't ready yet"]], ["error", ["Couldn't check harnesses.", "Try again"]]]) {
  test(`the ${state} state stands under the title with Check again`, async () => {
    const seen = await on({ harnesses: state }, async (page) => ({ top: await top(page), says: await say(page), sections: await read(page) }));
    assert.equal(seen.top?.heading, "Harnesses", JSON.stringify(seen.top));
    assert.deepEqual(seen.top.actions, ["Check again"]);
    assert.deepEqual(seen.sections, []);
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

test("Try again runs the check a second time", async () => {
  const reads = await on({ harnesses: "error" }, async (page) => {
    await press(page, "button", "Try again");
    return argsOf(page, "desktop_harness_catalog");
  });
  assert.deepEqual(reads, [{ check: true }, { check: true }]);
});

test("Check again runs the check and says when", async () => {
  const seen = await on({}, async (page) => {
    await press(page, CHECK, "Check again");
    return { reads: await argsOf(page, "desktop_harness_catalog"), top: await top(page) };
  });
  assert.equal(seen.reads.length, 2);
  assert.match(seen.top?.says ?? "", /Checked at \d{1,2}:\d{2}/);
});

test("real Tab keys walk Check again, then each harness in group order", async () => {
  const walked = await on({}, (page) => tabStops(page, 5));
  assert.deepEqual(walked, ["Check again", "Codex", "Claude Code", "Rcli", "Gemini CLI"]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose and help read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await on({}, (page) => contrasts(page, [".set-help", ".settings-top p"]), 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
