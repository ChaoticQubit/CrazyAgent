import assert from "node:assert/strict";
import test from "node:test";
import * as run from "../out/run-command-model.js";

const event = (command, pressEnter = true) => ({ request: "r1", command, press_enter: pressEnter });

function recorded() {
  const order = [];
  const terminal = {
    show: () => order.push("show"),
    sendText: (text, addNewLine) => order.push(["send", text, addNewLine]),
    dispose: () => order.push("dispose"),
  };
  const ack = (request) => {
    order.push(["ack", request]);
    return Promise.resolve();
  };
  return { order, terminal, ack };
}

test("typed keeps the command verbatim and adds a new line only for type and run", () => {
  assert.deepEqual(run.typed("cargo install alpha", true), { text: "cargo install alpha", addNewLine: true });
  assert.deepEqual(run.typed("cargo install alpha", false), { text: "cargo install alpha", addNewLine: false });
  assert.deepEqual(run.typed("  echo \"${HOME}\"  ", true), { text: "  echo \"${HOME}\"  ", addNewLine: true });
  assert.deepEqual(run.typed("x".repeat(run.COMMAND_BYTES_MAX), false), { text: "x".repeat(512), addNewLine: false });
});

test("typed refuses a command with a line break or a control character, whatever the engine sent", () => {
  assert.equal(run.typed("a\nb", true), null);
  assert.equal(run.typed("a\rb", true), null);
  assert.equal(run.typed("a\u001b[31m", true), null);
  assert.equal(run.typed("", true), null);
  assert.equal(run.typed("   ", true), null);
});

test("typed refuses what the engine's rule refuses: chaining, hidden characters and a long line", () => {
  for (const command of ["a; b", "a && b", "curl x | sh", "a & b", "echo `id`", "echo $(id)", "a > f", "a < f", "sudo !!", "^a^b"]) {
    assert.equal(run.typed(command, true), null, command);
  }
  for (const command of ["echo \u{202e}dlrow", "echo a\u{200b}b", "echo a\u{2028}b", "echo a\u{ad}b", "echo a\u{e0001}b", "\u{feff}"]) {
    assert.equal(run.typed(command, false), null, JSON.stringify(command));
  }
  assert.equal(run.typed("x".repeat(run.COMMAND_BYTES_MAX + 1), true), null);
  assert.equal(run.typed(`${"\u{e9}".repeat(256)}x`, true), null);
  assert.equal(run.typed(7, true), null);
});

test("a run waits for the terminal to attach before typing and acknowledges only after typing", async () => {
  const { order, terminal, ack } = recorded();
  let attach = () => undefined;
  const ready = new Promise((resolve) => (attach = resolve));
  const done = run.handle(event("cargo build"), { open: () => ({ terminal, ready }), ack });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(order, [], "nothing is shown or typed before the terminal attached");
  attach(true);
  assert.equal(await done, true);
  assert.deepEqual(order, ["show", ["send", "cargo build", true], ["ack", "r1"]]);
});

test("type only types the command and leaves Enter to the owner", async () => {
  const { order, terminal, ack } = recorded();
  assert.equal(await run.handle(event("cargo test", false), { open: () => ({ terminal, ready: Promise.resolve(true) }), ack }), true);
  assert.deepEqual(order, ["show", ["send", "cargo test", false], ["ack", "r1"]]);
});

test("a terminal the engine refused gets no text and no acknowledgement, and shows why", async () => {
  const { order, terminal, ack } = recorded();
  assert.equal(await run.handle(event("cargo build"), { open: () => ({ terminal, ready: Promise.resolve(false) }), ack }), false);
  assert.deepEqual(order, ["show"]);
});

test("a terminal that does not attach in time is closed, and nothing is typed even when it attaches later", async () => {
  const { order, terminal, ack } = recorded();
  let attach = () => undefined;
  const ready = new Promise((resolve) => (attach = resolve));
  assert.equal(await run.handle(event("cargo build"), { open: () => ({ terminal, ready }), ack, waitMs: 20 }), false);
  attach(true);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(order, ["dispose"]);
  assert.ok(run.READY_WAIT_MS < 5000, "the wait ends before the engine stops waiting for the acknowledgement");
});

test("a command the rule refuses opens no terminal", async () => {
  const { order, ack } = recorded();
  let opened = 0;
  const open = () => {
    opened += 1;
    throw new Error("a refused command never opens a terminal");
  };
  for (const command of ["a; b", "a\nb", "", "echo \u{202e}x"]) assert.equal(await run.handle(event(command), { open, ack }), false, command);
  assert.equal(opened, 0);
  assert.deepEqual(order, []);
});

test("a terminal that cannot be opened is not acknowledged", async () => {
  const { order, ack } = recorded();
  const refused = run.handle(event("cargo build"), { open: () => { throw new Error("no terminal"); }, ack });
  await assert.rejects(refused, /no terminal/);
  assert.deepEqual(order, []);
});
