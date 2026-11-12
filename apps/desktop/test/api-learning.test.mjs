import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { loadLearningProgress, setLearningMark } = await import("../src/api-learning.ts");

async function bridged(reply, run) {
  const sent = [];
  const before = globalThis.window;
  globalThis.window = { __TAURI_INTERNALS__: { async invoke(command, args) {
    sent.push([command, args]);
    return reply;
  } } };
  try {
    await run(sent);
  } finally {
    globalThis.window = before;
  }
}

test("the learning read goes to its own command and hands back the engine's steps", async () => {
  const progress = { steps: [{ id: "project", topic: "basics", title: "Create a project", body: "One. Two.", count: 1 }], skipped: false };
  await bridged(progress, async (sent) => {
    assert.deepEqual(await loadLearningProgress(), { kind: "ok", value: progress });
    assert.deepEqual(sent.map(([command]) => command), ["desktop_learning_progress"]);
  });
});

test("a learning mark sends its step, its state and a fresh generation", async () => {
  await bridged({ outcome: "applied", pending: false, rejection: null }, async (sent) => {
    assert.deepEqual(await setLearningMark("skipped", true), { kind: "ok", value: undefined });
    assert.equal(sent.length, 1);
    const [command, args] = sent[0];
    assert.equal(command, "desktop_set_learning_mark");
    assert.equal(args.step, "skipped");
    assert.equal(args.on, true);
    assert.ok(Number.isSafeInteger(args.generation) && args.generation > 0, String(args.generation));
  });
});

test("a mark the engine already held is ok though no receipt came back", async () => {
  await bridged(null, async () => {
    assert.deepEqual(await setLearningMark("process", true), { kind: "ok", value: undefined });
  });
});

test("a refused or unconfirmed mark is not reported as set", async () => {
  await bridged({ outcome: "rejected", pending: false, rejection: "owner_authentication_required" }, async () => {
    assert.equal((await setLearningMark("usage", true)).kind, "error");
  });
  await bridged({ outcome: "applied", pending: true, rejection: null }, async () => {
    const result = await setLearningMark("terminal", true);
    assert.equal(result.kind, "error");
    assert.equal(result.code, "uncertain");
  });
});
