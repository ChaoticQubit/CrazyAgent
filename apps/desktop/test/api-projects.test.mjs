import assert from "node:assert/strict";
import test from "node:test";
import "./source-hooks.mjs";

const api = await import("../src/api-projects.ts");

test("a missing Tauri command maps to a typed not ready result", () => {
  assert.deepEqual(api.toResult("desktop_harness_catalog", "Command desktop_harness_catalog not found"),
    { kind: "not_ready", command: "desktop_harness_catalog" });
  assert.deepEqual(api.toResult("desktop_research_jobs", "Command desktop_research_jobs not allowed by ACL"),
    { kind: "not_ready", command: "desktop_research_jobs" });
  assert.deepEqual(api.toResult("desktop_research_jobs",
    "desktop_research_jobs not allowed on window \"main\", webview \"main\", URL: local\n\ncommand not allowed on any window/webview/URL context"),
  { kind: "not_ready", command: "desktop_research_jobs" });
});

test("engine refusals stay failures with their plain code and never become not ready", () => {
  assert.deepEqual(api.toResult("desktop_research_jobs", { code: "owner_read_failed" }), { kind: "failed", code: "owner_read_failed" });
  assert.deepEqual(api.toResult("desktop_research_jobs", "Command desktop_project_context not found"), { kind: "failed", code: "unknown" });
  assert.deepEqual(api.toResult("desktop_research_jobs", new Error("boom")), { kind: "failed", code: "unknown" });
  assert.deepEqual(api.toResult("desktop_research_jobs", null), { kind: "failed", code: "unknown" });
});

test("failure copy is plain language and falls back for unknown codes", () => {
  assert.equal(api.failureCopy("pending_intent"), "Another change is still being saved. Wait for it to finish, then try again.");
  assert.equal(api.failureCopy("document_not_a_regular_file"), "Can't add a folder or a shortcut. Choose the original file.");
  assert.equal(api.failureCopy("made_up_code"), "Something went wrong. Try again.");
  for (const code of ["owner_read_failed", "team_agent_range_invalid", "document_type_unsupported", "project_folder_exists"]) {
    assert.doesNotMatch(api.failureCopy(code), /_|—/);
  }
});

test("assigning a pool adds the project and master role once and keeps everything else", () => {
  const pool = { id: "main", members: ["a", "b"], projects: ["other"], roles: ["reviewer"], fallback_allowed: false, provider_quota_policy: "strict" };
  const assigned = api.assignedPool(pool, "p-1");
  assert.deepEqual(assigned, { ...pool, projects: ["other", "p-1"], roles: ["reviewer", "project_leader"] });
  assert.deepEqual(pool.projects, ["other"]);
  assert.deepEqual(api.assignedPool(assigned, "p-1"), assigned);
  assert.throws(() => api.assignedPool({ ...pool, projects: Array.from({ length: 64 }, (_, i) => `p${i}`) }, "new"), /64/);
});

test("a receipt is applied only when the engine recorded it without rejection", () => {
  const base = { kind: "setup_project", generation: "4", project_id: "p", baseline_id: null, model_access: null, project_spec: null, operation_id: "o" };
  assert.deepEqual(api.receiptState({ ...base, outcome: "applied", pending: false, rejection: null }), { kind: "applied" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "rejected", pending: false, rejection: "project_setup_bounds" }), { kind: "rejected", code: "project_setup_bounds" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "uncertain", pending: true, rejection: null }), { kind: "unconfirmed" });
  assert.deepEqual(api.receiptState({ ...base, outcome: "unavailable", pending: false, rejection: null }), { kind: "rejected", code: "unavailable" });
});

test("generations are positive, increasing and within the bridge bound", () => {
  const first = api.nextGeneration();
  const second = api.nextGeneration();
  assert.ok(first > 0 && second > first && second <= Number.MAX_SAFE_INTEGER);
});
