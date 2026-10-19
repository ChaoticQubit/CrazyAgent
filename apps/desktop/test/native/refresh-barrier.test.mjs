import assert from "node:assert/strict";
import test from "node:test";
import { appendFileSync, chmodSync, mkdirSync, mkdtempSync, renameSync,
  rmSync, symlinkSync, truncateSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createNavigationReader, parseNavigation, refreshBarrier, markDocument,
  documentReadiness, installRefresh }
  from "./refresh-barrier.mjs";

test("exact bare native asset origin supports a Started Finished pair", () => {
  const state = parseNavigation([lines([
    record(1, { url: "tauri://localhost" }),
    record(2, { url: "tauri://localhost" }),
  ])], "unit-258");
  assert.equal(state.url, "tauri://localhost");
  assert.equal(state.sequence, 2);
  assert.equal(state.generation, 1);
  assert.equal(state.finished, 1);
  assert.equal(state.pending, false);
});

test("bare origin does not admit query fragment host or path variants", () => {
  for (const url of ["tauri://localhost?private-query", "tauri://localhost#private-fragment",
    "tauri://localhost/?private-query", "tauri://localhost/#private-fragment",
    "tauri://foreign", "tauri://localhost.evil", "tauri://user@localhost",
    "tauri://localhost:1", "tauri://localhost/other", "tauri://localhost//",
    "tauri://LOCALHOST", "TAURI://localhost"]) {
    assert.throws(() => parseNavigation([lines([record(1, { url })])], "unit-258"),
      /native asset URL required/);
  }
});

test("single-instance service indexed backend prefix preserves native parsing", () => {
  const text = lines([record(1), record(2)]).replaceAll("[Tauri:Backend]", "[Tauri:Backend:0]");
  assert.equal(parseNavigation([text], "unit-258").finished, 1);
});

for (const prefix of ["[Tauri:Frontend:0]", "[Tauri:Backend:1]", "[Tauri:Backend:00]",
  "[Tauri:Backend:browserA]", "[Tauri:Backend] forged", "x[Tauri:Backend:0]",
  "[Tauri:Backend:0:extra]", "[tauri:backend:0]"]) {
  test(`native parser rejects unadmitted backend prefix ${prefix}`, () => {
    const text = lines([record(1)]).replace("[Tauri:Backend]", prefix);
    assert.throws(() => parseNavigation([text], "unit-258"), /native backend source required/);
  });
}

function record(sequence, changes = {}) {
  return { schema: 1, run: "unit-258", pid: 12, app_started_ns: "1", sequence,
    generation: Math.ceil(sequence / 2), phase: sequence % 2 ? "Started" : "Finished",
    window: "main", url: "tauri://localhost/", elapsed_ns: String(sequence),
    wall_ns: String(sequence + 1), ...changes };
}

function lines(records) {
  return records.map((item) => `[Tauri:Backend] native-refresh-page: ${JSON.stringify(item)}\n`)
    .join("");
}

function tree() {
  const root = mkdtempSync(join(tmpdir(), "ca-refresh-parser-"));
  chmodSync(root, 0o700);
  const logs = join(root, "webdriver");
  mkdirSync(logs, { mode: 0o700 });
  const file = join(logs, "wdio-unit.log");
  writeFileSync(file, lines([record(1), record(2)]), { mode: 0o600 });
  return { root, logs, file, close: () => rmSync(root, { recursive: true }) };
}

test("serial initial and replacement generations are distinct", () => {
  const state = parseNavigation([lines([record(1), record(2), record(3), record(4)])], "unit-258");
  assert.equal(state.finished, 2);
  assert.equal(state.identity, "12:1");
  assert.equal(state.pending, false);
});

test("an incomplete live prefix is not Finished", () => {
  const state = parseNavigation([lines([record(1)])], "unit-258");
  assert.equal(state.pending, true);
  assert.equal(state.finished, 0);
});

test("partial final event is withheld rather than accepted", () => {
  const state = parseNavigation([lines([record(1), record(2)]) + "[Tauri:Backend] native-ref"], "unit-258");
  assert.equal(state.partial, true);
});

for (const [name, records] of [
  ["overlap", [record(1), record(2, { phase: "Started" })]],
  ["finish without commit", [record(1, { phase: "Finished" })]],
  ["wrong run", [record(1, { run: "other" })]],
  ["wrong window", [record(1, { window: "other" })]],
  ["changed process", [record(1), record(2, { pid: 13 })]],
  ["changed epoch", [record(1), record(2, { app_started_ns: "2" })]],
  ["zero start identity", [record(1, { app_started_ns: "0" })]],
  ["PID overflow", [record(1, { pid: 2 ** 32 })]],
  ["changed asset URL", [record(1), record(2, { url: "tauri://localhost/index.html" })]],
  ["wrong URL origin", [record(1, { url: "https://localhost/" })]],
  ["unexpected URL query", [record(1, { url: "tauri://localhost/?token=x" })]],
  ["sequence gap", [record(1), record(3)]],
  ["generation gap", [record(1, { generation: 2 })]],
  ["backwards time", [record(1), record(2, { elapsed_ns: "0" })]],
  ["extra navigation", [record(1), record(2), record(3), record(4), record(5)]],
  ["terminal invalid", [{ schema: 1, run: "unit-258", invalid: true }]],
]) {
  test(`native parser rejects ${name}`, () => {
    assert.throws(() => parseNavigation([lines(records)], "unit-258"));
  });
}

test("malformed complete JSON and oversized lines reject", () => {
  assert.throws(() => parseNavigation(["[Tauri:Backend] native-refresh-page: {\n"], "unit-258"));
  assert.throws(() => parseNavigation(["x".repeat(1025)], "unit-258"));
});

test("real log append advances the actual reader", () => {
  const fixture = tree();
  try {
    const read = createNavigationReader(fixture.root, "unit-258");
    assert.equal(read().finished, 1);
    appendFileSync(fixture.file, lines([record(3), record(4)]));
    assert.equal(read().finished, 2);
  } finally { fixture.close(); }
});

test("real log symlink rejects", () => {
  const fixture = tree();
  try {
    renameSync(fixture.file, join(fixture.root, "original"));
    symlinkSync(join(fixture.root, "original"), fixture.file);
    assert.throws(() => createNavigationReader(fixture.root, "unit-258")());
  } finally { fixture.close(); }
});

test("real file replacement and truncation reject across reads", () => {
  const fixture = tree();
  try {
    const read = createNavigationReader(fixture.root, "unit-258");
    read();
    truncateSync(fixture.file, 0);
    assert.throws(read);
    renameSync(fixture.file, join(fixture.root, "old"));
    writeFileSync(fixture.file, "", { mode: 0o600 });
    assert.throws(read);
  } finally { fixture.close(); }
});

test("same-inode same-size rewrite cannot replace previously observed native evidence", () => {
  const fixture = tree();
  try {
    const read = createNavigationReader(fixture.root, "unit-258");
    read();
    writeFileSync(fixture.file, lines([record(1, { pid: 13 }), record(2, { pid: 13 })]));
    assert.throws(read, /prefix changed/);
  } finally { fixture.close(); }
});

test("real oversized and public log files reject", () => {
  const fixture = tree();
  try {
    chmodSync(fixture.file, 0o644);
    assert.throws(() => createNavigationReader(fixture.root, "unit-258")());
    chmodSync(fixture.file, 0o600);
    truncateSync(fixture.file, 4 * 1024 * 1024 + 1);
    assert.throws(() => createNavigationReader(fixture.root, "unit-258")());
  } finally { fixture.close(); }
});

function unitCommands(changes = {}) {
  const trace = [];
  let sent = false;
  const initial = parseNavigation([lines([record(1), record(2)])], "unit-258");
  const replacement = parseNavigation([lines([record(1), record(2), record(3), record(4)])], "unit-258");
  return { trace, options: {
    read: () => { trace.push("native"); return sent ? replacement : initial; },
    mark: async () => { trace.push("mark"); },
    original: async () => { trace.push("original"); sent = true; return "original-result"; },
    ready: async () => { trace.push("ready"); return true; },
    timeoutMs: 100,
    ...changes,
  } };
}

test("actual barrier logic dispatches once between initial and new native completion", async () => {
  const unit = unitCommands();
  assert.equal(await refreshBarrier(unit.options), "original-result");
  assert.deepEqual(unit.trace, ["native", "mark", "native", "original", "native", "ready", "native"]);
});

test("initial missing evidence fails before original dispatch", async () => {
  const unit = unitCommands({ read: () => parseNavigation([], "unit-258"), timeoutMs: 1 });
  await assert.rejects(refreshBarrier(unit.options), /deadline/);
  assert.deepEqual(unit.trace, []);
});

test("original error is propagated exactly once without readiness or replay", async () => {
  let calls = 0;
  const unit = unitCommands({ original: async () => { calls += 1; throw new Error("original failure"); } });
  await assert.rejects(refreshBarrier(unit.options), /original failure/);
  assert.equal(calls, 1);
  assert.equal(unit.trace.includes("ready"), false);
});

test("missing replacement times out after one dispatch without document evaluation", async () => {
  const initial = parseNavigation([lines([record(1), record(2)])], "unit-258");
  const unit = unitCommands({ read: () => initial, timeoutMs: 30 });
  await assert.rejects(refreshBarrier(unit.options), /deadline/);
  assert.equal(unit.trace.filter((item) => item === "original").length, 1);
  assert.equal(unit.trace.includes("ready"), false);
});

test("old document or disconnected readiness cannot pass", async () => {
  const unit = unitCommands({ ready: async () => false, timeoutMs: 30 });
  await assert.rejects(refreshBarrier(unit.options), /deadline/);
  assert.equal(unit.trace.filter((item) => item === "original").length, 1);
});

test("readiness evaluation errors are not swallowed or retried", async () => {
  let calls = 0;
  const unit = unitCommands({ ready: async () => { calls += 1; throw new Error("lost document"); } });
  await assert.rejects(refreshBarrier(unit.options), /lost document/);
  assert.equal(calls, 1);
});

test("changed app identity during refresh rejects", async () => {
  let reads = 0;
  const initial = parseNavigation([lines([record(1), record(2)])], "unit-258");
  const other = parseNavigation([lines([record(1, { pid: 13 }), record(2, { pid: 13 })])], "unit-258");
  const unit = unitCommands({ read: () => ++reads <= 2 ? initial : other });
  await assert.rejects(refreshBarrier(unit.options), /identity/);
  assert.equal(unit.trace.filter((item) => item === "original").length, 1);
});

test("hung readiness is bounded without a second action", async () => {
  const unit = unitCommands({ ready: () => new Promise(() => {}), timeoutMs: 30 });
  await assert.rejects(refreshBarrier(unit.options), /deadline/);
  assert.equal(unit.trace.filter((item) => item === "original").length, 1);
});

test("deadline expiration during final native read prevents original dispatch", async () => {
  const initial = parseNavigation([lines([record(1), record(2)])], "unit-258");
  let reads = 0;
  let calls = 0;
  const wait = new Int32Array(new SharedArrayBuffer(4));
  const unit = unitCommands({ timeoutMs: 10,
    read: () => {
      reads += 1;
      if (reads === 2) Atomics.wait(wait, 0, 0, 15);
      return initial;
    },
    original: async () => { calls += 1; },
  });
  await assert.rejects(refreshBarrier(unit.options), /deadline/);
  assert.equal(calls, 0, "expired admission must not dispatch the original refresh");
});

test("actual document functions reject old markers and require each readiness condition", () => {
  const oldDocument = globalThis.document;
  const oldLocation = globalThis.location;
  const owner = { disabled: false, getClientRects: () => [1] };
  const footer = { textContent: "Owner connected" };
  let shell = {};
  const current = { readyState: "complete", querySelector: (selector) => {
    if (selector === '[data-testid="connect-owner"]') return owner;
    if (selector === "#root .app-shell") return shell;
    if (selector === ".sidebar-footer") return footer;
    return null;
  } };
  globalThis.document = current;
  globalThis.location = { href: "tauri://localhost/" };
  try {
    assert.equal(documentReadiness("unit-marker", location.href), true);
    current.readyState = "loading";
    assert.equal(documentReadiness("unit-marker", location.href), false);
    current.readyState = "complete";
    owner.disabled = true;
    assert.equal(documentReadiness("unit-marker", location.href), false);
    owner.disabled = false;
    footer.textContent = "Owner disconnected";
    assert.equal(documentReadiness("unit-marker", location.href), false);
    footer.textContent = "Owner connected";
    shell = null;
    assert.equal(documentReadiness("unit-marker", location.href), false);
    shell = {};
    assert.throws(() => documentReadiness("unit-marker", "tauri://localhost/index.html"));
    assert.equal(markDocument("unit-marker"), true);
    assert.equal(documentReadiness("unit-marker", location.href), false);
    assert.throws(() => markDocument("unit-marker"));
    assert.throws(() => documentReadiness("other-marker", location.href));
  } finally {
    globalThis.document = oldDocument;
    globalThis.location = oldLocation;
  }
});

test("the installed wrapper calls its supplied original once and refuses a second refresh", async () => {
  const fixture = tree();
  let wrapper;
  let dispatches = 0;
  let evaluations = 0;
  const browser = {
    overwriteCommand: (name, command) => { assert.equal(name, "refresh"); wrapper = command; },
    execute: async (command) => {
      evaluations += 1;
      assert([markDocument, documentReadiness].includes(command));
      return true;
    },
  };
  try {
    installRefresh(browser, fixture.root, "unit-258");
    const original = async () => {
      dispatches += 1;
      appendFileSync(fixture.file, lines([record(3), record(4)]));
      return "unit-dispatch-result";
    };
    assert.equal(await wrapper.call(browser, original), "unit-dispatch-result");
    assert.equal(dispatches, 1);
    assert.equal(evaluations, 2);
    await assert.rejects(wrapper.call(browser, original), /exactly one/);
    assert.equal(dispatches, 1);
  } finally { fixture.close(); }
});

test("installed WDIO overwrite dispatches once with nested observational notifications", async () => {
  const { webdriverMonad, wrapCommand } =
    await import("../../node_modules/@wdio/utils/build/index.js");
  const fixture = tree();
  const notifications = [];
  let dispatches = 0;
  const unit = webdriverMonad({
    beforeCommand: (name) => { if (name === "refresh") notifications.push("before"); },
    afterCommand: (name) => { if (name === "refresh") notifications.push("after"); },
  }, undefined, {
    refresh: { value: async () => {
      dispatches += 1;
      appendFileSync(fixture.file, lines([record(3), record(4)]));
      return "unit-original-result";
    } },
    execute: { value: async () => true },
  });
  const client = unit("unit-session", wrapCommand);
  try {
    installRefresh(client, fixture.root, "unit-258");
    assert.equal(await client.refresh(), "unit-original-result");
    assert.equal(dispatches, 1);
    assert.deepEqual(notifications, ["before", "before", "after", "after"]);
    await assert.rejects(client.refresh(), /exactly one/);
    assert.equal(dispatches, 1);
  } finally { fixture.close(); }
});
