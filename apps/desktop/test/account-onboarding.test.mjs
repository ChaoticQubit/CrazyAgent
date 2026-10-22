import assert from "node:assert/strict";
import test from "node:test";
import * as modelAccess from "../src/model-access.ts";

test("managed account input contains only the closed provider choice and optional label", () => {
  assert.equal(typeof modelAccess.managedAccountInput, "function", "production model access must accept provider-first managed account input");
  assert.deepEqual(modelAccess.managedAccountInput("codex", ""), { provider: "codex", label: "" });
  assert.deepEqual(modelAccess.managedAccountInput("claude", " Personal "), { provider: "claude", label: "Personal" });
  assert.deepEqual(Object.keys(modelAccess.managedAccountInput("codex", "Work")), ["provider", "label"]);
});

test("managed account input rejects unsupported choices and bounds labels without accepting routing fields", () => {
  assert.equal(typeof modelAccess.managedAccountInput, "function");
  assert.throws(() => modelAccess.managedAccountInput("other", ""), /Choose Codex or Claude/);
  assert.throws(() => modelAccess.managedAccountInput("codex", "a".repeat(97)), /96 UTF-8 bytes/);
  assert.throws(() => modelAccess.managedAccountInput("claude", "a\nlabel"), /one line/);
  assert.throws(() => modelAccess.managedAccountInput("codex", "\u202eaccount"), /one line/);
  assert.throws(() => modelAccess.managedAccountInput("claude", null), /label/);
  assert.equal(modelAccess.managedAccountInput("claude", "界".repeat(32)).label, "界".repeat(32));
  assert.throws(() => modelAccess.managedAccountInput("claude", "界".repeat(33)), /96 UTF-8 bytes/);
});
