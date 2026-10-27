import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
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
  assert.equal(api.unconfirmedLine(intent("owner_profile"), "not_recorded_at_barrier"), "Saving your profile never reached the engine. Retry sends it again, or Dismiss drops it.");
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

test("dismissing goes through the bridge and a refused dismissal says why in plain words", () => {
  const source = readFileSync(new URL("../src/api.ts", import.meta.url), "utf8");
  assert.match(source, /export function dropUnconfirmed\(\): Promise<Result<ConnectionStatus>> \{\s*return call<ConnectionStatus>\("desktop_drop_intent"\);/);
  assert.equal(api.plainError("change_may_be_recorded"), "This change may still go through, so it can't be dismissed. Use Check again.");
});

test("automatic checks are few, spaced out and end within a minute", () => {
  assert.ok(api.UNCONFIRMED_CHECKS_MS.length >= 3 && api.UNCONFIRMED_CHECKS_MS.length <= 6);
  assert.ok(api.UNCONFIRMED_CHECKS_MS.every((pause, index, all) => pause >= 1000 && (index === 0 || pause >= all[index - 1])));
  assert.ok(api.UNCONFIRMED_CHECKS_MS.reduce((sum, pause) => sum + pause, 0) <= 60_000);
});

test("every change the bridge can keep names itself in the banner and the toast", () => {
  const bridge = readFileSync(new URL("../src-tauri/src/bridge.rs", import.meta.url), "utf8");
  const mapping = bridge.slice(bridge.indexOf("pub fn pending_intent("), bridge.indexOf("bounded(Some(PendingIntent {"));
  const kinds = [...new Set([...mapping.matchAll(/\(\s*"([a-z_]+)",/g)].map((match) => match[1]))];
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  const union = client.slice(client.indexOf("export interface PendingIntent {"), client.indexOf("generation: string;", client.indexOf("export interface PendingIntent {")));
  assert.ok(kinds.length >= 31, `only ${kinds.length} kinds read from the bridge`);
  for (const kind of ["team", "project_setup_update", "project_remove", "research_cancel", "research_retry", "message", "pack_remove"]) assert.ok(kinds.includes(kind), kind);
  for (const kind of kinds) {
    assert.ok(union.includes(`"${kind}"`), `${kind} missing from PendingIntent`);
    const label = api.changeLabel(intent(kind));
    assert.match(label, /^[a-z][a-z' ]+[a-z]$/, kind);
    assert.equal(api.unconfirmedLine(intent(kind), "not_recorded_at_barrier"), `${label.charAt(0).toUpperCase()}${label.slice(1)} never reached the engine. Retry sends it again, or Dismiss drops it.`);
    assert.equal(api.settledLine(receipt(kind, "applied", false)), `${label.charAt(0).toUpperCase()}${label.slice(1)} went through.`);
  }
  assert.equal(api.unconfirmedLine(intent("project_remove"), "unknown"), "We couldn't confirm removing the project. Other changes wait until it's checked.");
});

test("a change kind the window does not know still gets a plain banner and toast", () => {
  assert.equal(api.unconfirmedLine(intent("a_future_change"), "not_recorded_at_barrier"), "Your change never reached the engine. Retry sends it again, or Dismiss drops it.");
  assert.equal(api.unconfirmedLine(intent("a_future_change"), null), "We couldn't confirm your change. Other changes wait until it's checked.");
  assert.equal(api.settledLine(receipt("a_future_change", "applied", false)), "Your change went through.");
  assert.equal(api.changeLabel(intent("model_access", { model_access: { kind: "add_connected_account", connection: {} } })), "changing accounts and pools");
});
