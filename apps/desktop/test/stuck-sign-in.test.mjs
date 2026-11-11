import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const api = await import("../src/api.ts");
const { STUCK_SIGN_IN_CODES } = await import("../src/state.ts");
const { cancelError } = await import("../src/screens/settings/sign-in.tsx");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("a refusal from a stuck sign-in points the owner at Clear stuck sign-in, not a restart", () => {
  const line = api.plainError("native_auth_recovery_required");
  assert.match(line, /Clear stuck sign-in/);
  assert.doesNotMatch(line, /restart/i);
  assert.match(cancelError("native_auth_recovery_required"), /Clear stuck sign-in/);
  assert.match(cancelError("native_auth_operation_scope_conflict"), /Clear stuck sign-in/);
  assert.match(cancelError("native_auth_operation_missing"), /Delete the account/);
  assert.match(cancelError("native_profile_source_conflict"), /Clear stuck sign-in/);
  assert.equal(cancelError("native_auth_not_active"), undefined);
  assert.match(api.plainError("native_auth_in_progress"), /still running/);
  assert.doesNotMatch(api.plainError("native_auth_in_progress"), /Clear stuck sign-in/);
});

test("the clear toast says cleared only when the engine answered for the sign-in", () => {
  assert.equal(api.stuckClearedLine({ kind: "ok", value: true }), "Stuck sign-in cleared.");
  assert.equal(api.stuckClearedLine({ kind: "ok", value: false }), api.plainError("native_auth_cancel_pending"));
  const failed = { kind: "error", code: "engine_unreachable", message: api.plainError("engine_unreachable") };
  assert.equal(api.stuckClearedLine(failed), failed.message);
  assert.equal(api.stuckClearedLine({ kind: "not_ready" }), api.plainError("unknown"));
  const stale = { kind: "error", code: "native_profile_source_conflict", message: "" };
  assert.equal(api.stuckClearedLine(stale), api.STALE_SIGN_IN_LINE);
  assert.match(api.STALE_SIGN_IN_LINE, /only the engine can end it/);
});

test("the window reads the stuck sign-in again after the refusals that can leave one", () => {
  assert.deepEqual(STUCK_SIGN_IN_CODES, ["native_auth_recovery_required", "native_auth_outcome_unknown_read_same_operation"]);
  const app = read("src/app.tsx");
  assert.match(app, /session\.stuckSignIn && <Banner/);
  assert.match(app, />Clear stuck sign-in<\/Button>/);
  assert.match(read("src/api.ts"), /call<boolean>\("desktop_clear_native_auth"\)/);
});
