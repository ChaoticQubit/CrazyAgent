import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

globalThis.window = new EventTarget();
const { choiceListeners } = await import("../src/choice-listeners.ts");

/** Sends a storage event as another window's save or clear does. Input: the stored key, null
 * for a clear. */
function stored(key) {
  window.dispatchEvent(Object.assign(new Event("storage"), { key }));
}

/** Listens to one new choice key and counts what it is told. Input: the key. Output: the
 * count read so far and the call that stops listening. */
function counted(key) {
  const heard = choiceListeners(key);
  let told = 0;
  const stop = heard.listen(() => {
    told += 1;
  });
  return { told: () => told, stop };
}

test("a save of the choice in another window tells its listeners and a save of another key does not", () => {
  const choice = counted("crazyagent.test-saved");
  stored("crazyagent.test-saved");
  assert.equal(choice.told(), 1);
  stored("crazyagent.something-else");
  assert.equal(choice.told(), 1);
  choice.stop();
});

test("a storage clear in another window tells every choice's listeners, so each reads its default again", () => {
  const first = counted("crazyagent.test-cleared-a");
  const second = counted("crazyagent.test-cleared-b");
  stored(null);
  assert.equal(first.told(), 1);
  assert.equal(second.told(), 1);
  first.stop();
  second.stop();
});

test("a listener that stopped is told of no save and no clear", () => {
  const choice = counted("crazyagent.test-stopped");
  choice.stop();
  stored("crazyagent.test-stopped");
  stored(null);
  assert.equal(choice.told(), 0);
});
