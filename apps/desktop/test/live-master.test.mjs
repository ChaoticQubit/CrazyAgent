import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const io = await import("../src/screens/team/terminal-io.ts");
const live = await import("../src/screens/project/live-master.tsx");

const key = (type, extra = {}) => ({ type, key: "Enter", shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...extra });

test("Shift+Enter types a new line once and never reaches xterm as a second Enter", () => {
  assert.deepEqual(io.keyOverride(key("keydown", { shiftKey: true })), { typed: "\n", xterm: false });
  assert.deepEqual(io.keyOverride(key("keypress", { shiftKey: true })), { typed: null, xterm: false });
  assert.deepEqual(io.keyOverride(key("keyup", { shiftKey: true })), { typed: null, xterm: false });
  assert.deepEqual(io.keyOverride(key("keydown")), { typed: null, xterm: true }, "Enter alone still sends");
  for (const modifier of ["altKey", "ctrlKey", "metaKey"]) {
    assert.deepEqual(io.keyOverride(key("keydown", { shiftKey: true, [modifier]: true })), { typed: null, xterm: true }, modifier);
  }
  assert.deepEqual(io.keyOverride({ ...key("keydown", { shiftKey: true }), key: "a" }), { typed: null, xterm: true });
  assert.deepEqual([...io.encodeTyped(io.NEW_LINE)], [0x0a]);
});

test("only Codex and Claude Code masters run live, and the master is found by kind", () => {
  assert.equal(live.runsLive("codex"), true);
  assert.equal(live.runsLive("claude-code"), true);
  for (const other of ["rcli", "opencode", "", undefined]) assert.equal(live.runsLive(other), false, String(other));
  const node = (id, kind, terminal = null) => ({ id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal });
  assert.equal(live.masterNode([node("g", "harness_group"), node("a", "agent", "team-2")]), null);
  assert.equal(live.masterNode([node("a", "agent", "team-2"), node("m", "master", "team-1")]).id, "m");
});

test("the project page mounts the master area on one line, where the team activity strip wraps it", () => {
  const page = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(page, /active === "conversation" && <TeamActivity data=\{data\}><MasterArea data=\{data\} \/><\/TeamActivity>/);
});

const fixture = await bundle(new URL("./fixtures/live-master.mjs", import.meta.url).pathname);

async function inPage(check) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    return check({ evaluate, send });
  });
}

test("with the live master on, its terminal fills the master area with no close button and no message box", async () => {
  const seen = await inPage(async ({ evaluate }) => {
    await evaluate('Fixture.mountView("running")');
    return evaluate("Fixture.shown()");
  });
  assert.equal(seen.live, true);
  assert.equal(seen.terminalLabel, "Master terminal");
  assert.equal(seen.close, false, "the master's terminal is the area itself; nothing closes it");
  assert.equal(seen.conversation, false);
  assert.equal(seen.console, false);
  assert.ok(seen.buttons.includes("Earlier conversation"), seen.buttons.join(", "));
  assert.ok(seen.area.bottom - seen.area.top >= seen.viewport - 200 - 1, JSON.stringify(seen.area));
  assert.ok(seen.area.bottom - seen.box.bottom <= 1, `terminal reaches the bottom of the area: ${JSON.stringify(seen)}`);
  assert.ok(seen.box.height >= (seen.area.bottom - seen.area.top) - 60, `terminal takes the area below its head: ${JSON.stringify(seen)}`);
});

test("before the master's terminal opens, and when the engine can't start it, the area says so and keeps the earlier conversation reachable", async () => {
  const seen = await inPage(async ({ evaluate }) => {
    const states = {};
    for (const state of ["starting", "waiting", "not_ready"]) {
      await evaluate(`Fixture.mountView(${JSON.stringify(state)})`);
      states[state] = await evaluate("Fixture.shown()");
    }
    await evaluate('Fixture.mountView("refused", "team_master_pool_empty")');
    states.refused = await evaluate("Fixture.shown()");
    return states;
  });
  assert.match(seen.starting.text, /Starting the master \(Codex\)\.\.\./);
  assert.match(seen.waiting.text, /Waiting for the master's terminal\./);
  assert.match(seen.not_ready.text, /Not ready yet: the live master terminal isn't in this build\./);
  assert.match(seen.refused.text, /The master terminal couldn't start\. The engine refused\./);
  assert.ok(seen.refused.buttons.includes("Try again"), seen.refused.buttons.join(", "));
  for (const [state, view] of Object.entries(seen)) {
    assert.equal(view.terminal, false, state);
    assert.ok(view.buttons.includes("Earlier conversation"), state);
  }
});

test("Earlier conversation opens the old engine-driven conversation read-only in the console, and the live master comes back", async () => {
  const seen = await inPage(async ({ evaluate }) => {
    await evaluate('Fixture.mountView("running")');
    const opened = await evaluate('Fixture.click("Earlier conversation")');
    const earlier = await evaluate("Fixture.shown()");
    await evaluate('Fixture.typeAndEnter("one more thing")');
    const typed = await evaluate("Fixture.shown()");
    const closed = await evaluate('Fixture.click("Back to the live master")');
    return { opened, earlier, typed, closed, back: await evaluate("Fixture.shown()") };
  });
  assert.equal(seen.opened, true);
  assert.equal(seen.earlier.conversation, true);
  assert.equal(seen.earlier.console, true, "the earlier conversation is the terminal-style console");
  assert.equal(seen.earlier.terminal, false);
  assert.equal(seen.earlier.status, "Read-only: the master runs live in its terminal now.");
  assert.ok(!seen.earlier.buttons.some((text) => ["Send", "Send again", "Stop", "Conversation actions"].includes(text)), seen.earlier.buttons.join(", "));
  assert.equal(seen.typed.prompt, "one more thing", "Enter sends nothing: the text is held");
  assert.deepEqual(seen.typed.owner, []);
  assert.equal(seen.typed.status, "The master can't take a message right now. Your text stays here.");
  assert.equal(seen.closed, true);
  assert.equal(seen.back.terminal, true);
  assert.equal(seen.back.conversation, false);
});

test("the live master header offers Stop live master and says the master starts when the tab opens", async () => {
  const seen = await inPage(async ({ evaluate }) => {
    await evaluate('Fixture.mountView("running")');
    const running = await evaluate("Fixture.shown()");
    const stopped = await evaluate('Fixture.click("Stop live master")');
    return { running, stopped, switched: await evaluate("Fixture.switchedTo()") };
  });
  assert.ok(seen.running.buttons.includes("Stop live master"), seen.running.buttons.join(", "));
  assert.ok(!seen.running.buttons.includes("Start live master"), seen.running.buttons.join(", "));
  assert.match(seen.running.text, /Live master starts when you open this tab\./);
  assert.equal(seen.stopped, true);
  assert.deepEqual(seen.switched, [false]);
});

test("a stopped live master shows why it stopped and offers Start live master", async () => {
  const why = "The live master could not start: no signed-in account.";
  const seen = await inPage(async ({ evaluate }) => {
    await evaluate(`Fixture.mountView("stopped", ${JSON.stringify(why)})`);
    const guarded = await evaluate("Fixture.shown()");
    const started = await evaluate('Fixture.click("Start live master")');
    const switched = await evaluate("Fixture.switchedTo()");
    await evaluate('Fixture.mountView("stopped")');
    return { guarded, started, switched, owner: await evaluate("Fixture.shown()") };
  });
  assert.equal(seen.guarded.terminal, false);
  assert.ok(seen.guarded.text.includes(why), seen.guarded.text);
  assert.ok(seen.guarded.buttons.includes("Start live master"), seen.guarded.buttons.join(", "));
  assert.ok(!seen.guarded.buttons.includes("Stop live master"), seen.guarded.buttons.join(", "));
  assert.doesNotMatch(seen.guarded.text, /starts when you open this tab/);
  assert.ok(seen.guarded.buttons.includes("Earlier conversation"), "the earlier conversation stays reachable");
  assert.equal(seen.started, true);
  assert.deepEqual(seen.switched, [true]);
  assert.match(seen.owner.text, /The live master is stopped\. Its terminal opens again once you start it\./);
});

test("the live master switch is read from the team status, and a switch never set reads as null", async () => {
  const { liveSwitch } = await import("../src/screens/project/live-master.tsx");
  const status = (patch) => ({ kind: "ok", value: { project_id: "alpha", keep_warm: false, paused: false, min_agents: 0, max_agents: 1, harnesses: [], agents: [], tasks: [], messages: [], plan: [], ...patch } });
  assert.equal(liveSwitch(undefined, "alpha"), undefined, "not read yet");
  assert.deepEqual(liveSwitch(status({}), "alpha"), { on: null, problem: null }, "an engine without the field reads as never set");
  assert.deepEqual(liveSwitch(status({ live_master: null, live_problem: null }), "alpha"), { on: null, problem: null });
  assert.deepEqual(liveSwitch(status({ live_master: true, live_problem: null }), "alpha"), { on: true, problem: null });
  assert.deepEqual(liveSwitch(status({ live_master: false, live_problem: "No account." }), "alpha"), { on: false, problem: "No account." });
  assert.deepEqual(liveSwitch({ kind: "error", code: "engine_unavailable" }, "alpha"), { on: null, problem: null });
  assert.equal(liveSwitch(status({ live_master: true }), "beta"), undefined, "another project's read is not this switch");
});

test("a master harness with no terminal app keeps today's conversation, also when the engine says so", async () => {
  const seen = await inPage(async ({ evaluate }) => {
    await evaluate('Fixture.mountArea("rcli")');
    const plan = await evaluate("Fixture.shown()");
    await evaluate('Fixture.mountView("refused", "team_master_no_terminal")');
    const engine = await evaluate("Fixture.shown()");
    await evaluate("Fixture.mountArea(undefined)");
    const loading = await evaluate("Fixture.shown()");
    return { plan, engine, loading };
  });
  for (const [why, view] of Object.entries({ plan: seen.plan, engine: seen.engine })) {
    assert.equal(view.live, false, why);
    assert.equal(view.terminal, false, why);
    assert.equal(view.conversation, true, why);
    assert.equal(view.console, true, `${why}: the conversation is the terminal-style console`);
    assert.equal(view.status, "Enter to send, Shift+Enter for a new line, / for commands, @ to tag.", why);
  }
  assert.equal(seen.loading.conversation, false, "nothing is decided before the team plan is read");
  assert.equal(seen.loading.live, false);
  assert.match(seen.loading.text, /Loading the master\.\.\./);
});

test("in a real xterm, Shift+Enter types Ctrl+J once and Enter still types Enter", async () => {
  const typed = await inPage(async ({ evaluate, send }) => {
    await evaluate("Fixture.mountKeys()");
    const press = async (modifiers) => {
      for (const type of ["keyDown", "keyUp"]) {
        await send("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, modifiers, ...(type === "keyDown" ? { text: "\r" } : {}) });
      }
    };
    await press(8);
    await press(0);
    await send("Input.insertText", { text: "a" });
    return evaluate("Fixture.typedText()");
  });
  assert.deepEqual(typed, ["\n", "\r", "a"]);
});
