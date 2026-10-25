import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const client = await import("../src/engine-client.ts");
const api = await import("../src/api.ts");
const projects = await import("../src/api-projects.ts");

const intent = (kind, patch = {}) => ({ kind, generation: "4", project_id: null, baseline_id: null, model_access: null, project_spec: null, operation_id: "op-1", ...patch });
const receipt = (kind, outcome, pending, rejection = null, patch = {}) => ({ ...intent(kind, patch), outcome, pending, rejection });

test("a bridge reply that keeps a change pending, or a refusal because one is pending, leaves it unconfirmed", () => {
  assert.equal(client.leavesUnconfirmed({ ok: true, value: { pending: true } }), true);
  assert.equal(client.leavesUnconfirmed({ ok: true, value: { pending: false } }), false);
  assert.equal(client.leavesUnconfirmed({ ok: true, value: null }), false);
  assert.equal(client.leavesUnconfirmed({ ok: true, value: [{ pending: true }] }), false);
  assert.equal(client.leavesUnconfirmed({ ok: false, failure: { code: "pending_intent" } }), true);
  assert.equal(client.leavesUnconfirmed({ ok: false, failure: { code: "busy" } }), false);
  assert.equal(client.leavesUnconfirmed({ ok: false, failure: new TypeError("no bridge") }), false);
});

test("the app hears about every reply that leaves a change unconfirmed until it stops watching", () => {
  let heard = 0;
  const stop = client.watchUnconfirmed(() => { heard += 1; });
  client.reportReply({ ok: true, value: receipt("setup_project", "unknown", true) });
  client.reportReply({ ok: true, value: receipt("setup_project", "applied", false) });
  client.reportReply({ ok: false, failure: { code: "pending_intent" } });
  assert.equal(heard, 2);
  stop();
  client.reportReply({ ok: false, failure: { code: "pending_intent" } });
  assert.equal(heard, 2);
});

test("the banner names the change that is still unconfirmed, not a generic busy line", () => {
  assert.equal(api.unconfirmedLine(intent("setup_project"), null), "We couldn't confirm saving the project setup. Other changes wait until it's checked.");
  assert.equal(api.unconfirmedLine(intent("model_access", { model_access: { kind: "create_pool", pool: {} } }), "unknown"), "We couldn't confirm creating a pool. Other changes wait until it's checked.");
  assert.equal(api.unconfirmedLine(intent("owner_profile"), "not_recorded_at_barrier"), "Saving your profile hasn't reached the engine. Retry sends the same change again.");
  for (const kind of ["message", "brief", "approve_baseline", "revoke_baseline", "model_access", "create_project", "setup_project", "research_memory", "project_documents",
    "project_context_delete", "memory_review", "memory_add", "memory_promote", "memory_delete", "memory_forget", "memory_remember", "memory_selection", "master",
    "owner_profile", "projects_home", "move_projects_home", "add_existing_folder", "start_onboarding", "run_setup", "pack_set", "pack_remove"]) {
    assert.match(api.changeLabel(intent(kind)), /^[a-z][a-z ]+[a-z]$/, kind);
  }
});

test("a checked change says whether it went through, and stays quiet while it is still unconfirmed", () => {
  assert.equal(api.settledLine(receipt("setup_project", "applied", false)), "Saving the project setup went through.");
  assert.equal(api.settledLine(receipt("owner_profile", "unavailable", false)), "Saving your profile didn't go through. This isn't available right now. Try again.");
  assert.equal(api.settledLine(receipt("model_access", "rejected", false, "pool_member_limit", { model_access: { kind: "create_pool", pool: {} } })),
    "Creating a pool didn't go through. A pool can hold up to 15 accounts.");
  assert.equal(api.settledLine(receipt("setup_project", "unknown", true)), null);
  assert.equal(api.settledLine(receipt("setup_project", "rejected", true, "owner_runtime_stale")), null);
  assert.equal(api.settledLine(receipt("owner_profile", "not_recorded_at_barrier", true)), null);
});

test("a screen never reports success or a final refusal while its change is still pending", () => {
  assert.deepEqual(projects.receiptState(receipt("setup_project", "rejected", true, "owner_runtime_stale")), { kind: "unconfirmed" });
  assert.deepEqual(projects.receiptState(receipt("setup_project", "rejected", false, "owner_runtime_stale")), { kind: "rejected", code: "owner_runtime_stale" });
  assert.equal(api.receiptResult(receipt("owner_profile", "rejected", true, "owner_runtime_stale")).code, "uncertain");
});

test("a refusal because a change is pending points at the banner instead of saying a change is still being saved", () => {
  const line = "Another change isn't confirmed yet. Use Check again at the top of the window, then try again.";
  assert.equal(api.plainError("pending_intent"), line);
  assert.equal(projects.failureCopy("pending_intent"), line);
  assert.doesNotMatch(projects.failureCopy("pending_intent"), /still being saved/);
});

test("automatic checks are few, spaced out and end within a minute", () => {
  assert.ok(api.UNCONFIRMED_CHECKS_MS.length >= 3 && api.UNCONFIRMED_CHECKS_MS.length <= 6);
  assert.ok(api.UNCONFIRMED_CHECKS_MS.every((pause, index, all) => pause >= 1000 && (index === 0 || pause >= all[index - 1])));
  assert.ok(api.UNCONFIRMED_CHECKS_MS.reduce((sum, pause) => sum + pause, 0) <= 60_000);
});
