import assert from "node:assert/strict";
import test from "node:test";
import { LOCKED_BYTES_MAX, parseLocked, restores } from "../out/lock-model.js";

const locked = {
  "chat.disableAIFeatures": true,
  "update.mode": "none",
  "extensions.autoUpdate": false,
  "task.allowAutomaticTasks": "off",
};

test("the re-assert diff restores only locked keys", () => {
  const seen = {
    "chat.disableAIFeatures": { value: true, user: undefined },
    "update.mode": { value: "start", user: "start" },
    "extensions.autoUpdate": { value: true, user: undefined },
    "task.allowAutomaticTasks": { value: "off", user: "off" },
    "editor.fontSize": { value: 18, user: 18 },
    "workbench.preferredDarkColorTheme": { value: "Solarized Dark", user: "Solarized Dark" },
  };
  assert.deepEqual(restores(locked, seen), [
    { key: "update.mode", value: "none" },
    { key: "extensions.autoUpdate", value: false },
  ]);
});

test("a User value that contradicts the lock is restored even while a higher layer wins", () => {
  assert.deepEqual(restores(locked, { "chat.disableAIFeatures": { value: true, user: false } }), [
    { key: "chat.disableAIFeatures", value: true },
  ]);
});

test("a key whose User value already matches is never written again", () => {
  assert.deepEqual(restores(locked, { "update.mode": { value: "start", user: "none" } }), []);
  assert.deepEqual(
    restores(locked, Object.fromEntries(Object.entries(locked).map(([key, value]) => [key, { value, user: undefined }]))),
    [],
  );
});

test("a locked key nobody reported is left alone", () => {
  assert.deepEqual(restores(locked, {}), []);
  assert.deepEqual(restores({ toString: "x" }, {}), []);
});

test("only one JSON object within the size limit is a locked-keys file", () => {
  assert.deepEqual(parseLocked(JSON.stringify(locked)), locked);
  for (const text of ["", "[]", "null", "true", "\"x\"", "{", "{} {}"]) {
    assert.equal(parseLocked(text), undefined, text);
  }
  const large = JSON.stringify({ key: "x".repeat(LOCKED_BYTES_MAX) });
  assert.equal(parseLocked(large), undefined);
  assert.equal(LOCKED_BYTES_MAX, 64 * 1024);
});
