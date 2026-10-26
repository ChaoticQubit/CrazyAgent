import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const api = await import("../src/api-projects.ts");

test("project failures use project copy first, then the app's shared copy", () => {
  assert.equal(api.failureCopy("stale_config"), "This changed since you opened it. Review and save again.");
  assert.equal(api.failureCopy("settings_read_only"), "Settings can't be changed in this engine. Start it with writable settings.");
  assert.equal(api.failureCopy("engine_unreachable"), "CrazyAgent's engine isn't running.");
  assert.equal(api.failureCopy("project_documents_too_large"), "This project's documents would be over 200 MB. Remove some files first.");
});

test("failure copy is plain language and falls back for unknown codes", () => {
  assert.equal(api.failureCopy("pending_intent"), "Another change isn't confirmed yet. Use Check again at the top of the window, then try again.");
  assert.equal(api.failureCopy("document_not_a_regular_file"), "Can't add a folder or a shortcut. Choose the original file.");
  assert.equal(api.failureCopy("made_up_code"), "Something went wrong. Try again.");
  for (const code of ["owner_read_failed", "team_agent_range_invalid", "document_type_unsupported", "project_folder_exists"]) {
    assert.doesNotMatch(api.failureCopy(code), /_|—/);
  }
});

test("a project has its pool only when exactly the chosen pool lists it, and no pool when none does", () => {
  const pool = (id, projects) => ({ id, members: ["a"], projects, roles: ["project_leader"], fallback_allowed: false });
  assert.equal(api.projectPoolIs([pool("main", ["p-1"]), pool("spare", ["p-2"])], "p-1", "main"), true);
  assert.equal(api.projectPoolIs([pool("main", ["p-1"]), pool("spare", ["p-1"])], "p-1", "main"), false);
  assert.equal(api.projectPoolIs([pool("main", ["p-2"])], "p-1", "main"), false);
  assert.equal(api.projectPoolIs([pool("main", ["p-2"])], "p-1", null), true);
  assert.equal(api.projectPoolIs([pool("main", ["p-1"])], "p-1", null), false);
  assert.equal(api.failureCopy("master_open_unavailable"), "Pick a pool to start. Choose one in this project's Settings, Pool.");
});

test("a receipt is applied only when the engine recorded it without rejection", () => {
  const base = { kind: "setup_project", generation: "4", project_id: "p", baseline_id: null, model_access: null, project_spec: null, operation_id: "o" };
  assert.deepEqual(api.receiptState({ ...base, outcome: "applied", pending: false, rejection: null }), { kind: "applied" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "rejected", pending: false, rejection: "project_setup_bounds" }), { kind: "rejected", code: "project_setup_bounds" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "uncertain", pending: true, rejection: null }), { kind: "unconfirmed" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "unavailable", pending: false, rejection: null }), { kind: "rejected", code: "unavailable" });
});

test("New Project reconciles once only when the engine answered OutcomeUnknown or the reply was lost", () => {
  const base = { kind: "setup_project", generation: "4", project_id: "p", baseline_id: null, model_access: null, project_spec: null, operation_id: "o" };
  assert.equal(api.needsReconcile({ ...base, outcome: "unknown", pending: true, rejection: null }), true);
  assert.equal(api.needsReconcile({ ...base, outcome: "uncertain", pending: true, rejection: null }), true);
  assert.equal(api.needsReconcile({ ...base, outcome: "applied", pending: false, rejection: null }), false);
  assert.equal(api.needsReconcile({ ...base, outcome: "rejected", pending: false, rejection: "project_setup_bounds" }), false);
  assert.equal(api.needsReconcile({ ...base, outcome: "not_recorded_at_barrier", pending: true, rejection: null }), false);
  assert.equal(api.needsReconcile({ ...base, outcome: "unknown", pending: false, rejection: null }), false);
});

test("a reconciled New Project counts as created only when the reconcile settled the same kept intent", () => {
  const base = { kind: "setup_project", generation: "4", project_id: "p", baseline_id: null, model_access: null, project_spec: null, operation_id: "o" };
  const sent = { ...base, outcome: "unknown", pending: true, rejection: null };
  const found = { ...base, outcome: "applied", pending: false, rejection: null };
  const settled = (value) => api.receiptState(api.settledReceipt(sent, { kind: "ok", value }));
  assert.deepEqual(api.settledReceipt(sent, { kind: "ok", value: found }), found);
  assert.deepEqual(settled(found), { kind: "applied" });
  assert.deepEqual(settled({ ...base, outcome: "unknown", pending: true, rejection: null }), { kind: "unconfirmed" });
  assert.deepEqual(settled({ ...base, outcome: "not_recorded_at_barrier", pending: true, rejection: null }), { kind: "unconfirmed" });
  assert.deepEqual(settled({ ...base, outcome: "rejected", pending: false, rejection: "project_name_taken" }), { kind: "rejected", code: "project_name_taken" });
  assert.deepEqual(settled({ ...found, operation_id: "other" }), { kind: "unconfirmed" });
  assert.deepEqual(settled({ ...found, project_id: "other" }), { kind: "unconfirmed" });
  assert.deepEqual(settled({ ...found, kind: "create_project" }), { kind: "unconfirmed" });
  assert.equal(api.settledReceipt(sent, { kind: "error", code: "no_pending_intent", message: "x" }), sent);
  assert.equal(api.settledReceipt(sent, { kind: "error", code: "engine_unreachable", message: "x" }), sent);
  assert.equal(api.settledReceipt(sent, { kind: "not_ready" }), sent);
});
