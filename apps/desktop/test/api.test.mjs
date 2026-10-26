import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const api = await import("../src/api.ts");

test("a Tauri command that is not registered yet maps to a typed not-ready result", () => {
  assert.deepEqual(api.classifyFailure("Command desktop_account_catalog not found"), { kind: "not_ready" });
  assert.deepEqual(api.classifyFailure("desktop_harness_catalog not allowed. Command not found"), { kind: "not_ready" });
  assert.deepEqual(api.classifyFailure("desktop_skills not allowed on window \"main\", webview \"main\", URL: local\n\nCommand not found"), { kind: "not_ready" });
});

test("bridge refusals map to plain copy and never echo raw codes or unknown text", () => {
  const limit = api.classifyFailure({ code: "pool_member_limit" });
  assert.deepEqual(limit, { kind: "error", code: "pool_member_limit", message: "A pool can hold up to 15 accounts." });
  assert.equal(api.classifyFailure({ code: "pool_in_use" }).message, "Can't delete this pool. A project uses it. Choose another pool for it first.");
  assert.equal(api.classifyFailure({ code: "approved_accounts_limit" }).message,
    "Projects can use up to 64 approved accounts. Remove an account you no longer use, then try again.");
  assert.equal(api.classifyFailure({ code: "allowed_destinations_limit" }).message,
    "Up to 64 sign-ins and model endpoints can be allowed. Remove a connection you no longer use, then try again.");
  assert.equal(api.classifyFailure({ code: "not_connected" }).code, "engine_unreachable");
  assert.equal(api.classifyFailure(new TypeError("Cannot read properties of undefined (reading 'invoke')")).code, "engine_unreachable");
  const unknown = api.classifyFailure({ code: "native_profile_custody_unsafe <b>" });
  assert.equal(unknown.kind, "error");
  assert.equal(unknown.message, "Something went wrong. Try again.");
  assert.equal(api.classifyFailure("some raw panic text with /Users/secret/path").message, "Something went wrong. Try again.");
  for (const code of api.KNOWN_ERROR_CODES) {
    const message = api.plainError(code);
    assert.doesNotMatch(message, /_|\bnull\b|\u2014/, code);
    assert.match(message, /^[A-Z].*\.$/, code);
  }
});

test("an engine receipt counts as saved only when applied and settled", () => {
  const base = { kind: "owner_profile", generation: "4", project_id: null, baseline_id: null, model_access: null, project_spec: null, operation_id: "x" };
  assert.deepEqual(api.receiptResult({ ...base, outcome: "applied", pending: false, rejection: null }), { kind: "ok", value: undefined });
  assert.deepEqual(api.receiptResult({ ...base, outcome: "rejected", pending: false, rejection: "pool_member_limit" }),
    { kind: "error", code: "pool_member_limit", message: "A pool can hold up to 15 accounts." });
  for (const outcome of ["applied", "unknown", "uncertain", "not_recorded_at_barrier"]) {
    assert.deepEqual(api.receiptResult({ ...base, outcome, pending: true, rejection: null }),
      { kind: "error", code: "uncertain", message: "We couldn't confirm your last change. Check it and save again." });
  }
  assert.equal(api.receiptResult({ ...base, outcome: "unavailable", pending: false, rejection: null }).kind, "error");
});

test("draft generations are positive, increasing and within the bridge's safe integer bound", () => {
  const first = api.nextGeneration();
  const second = api.nextGeneration();
  assert.ok(Number.isSafeInteger(first) && first > 0);
  assert.ok(second > first);
});

test("calls outside the desktop shell report the engine as unreachable instead of throwing", async () => {
  const result = await api.getProfile();
  assert.equal(result.kind, "error");
  assert.equal(result.code, "engine_unreachable");
});
