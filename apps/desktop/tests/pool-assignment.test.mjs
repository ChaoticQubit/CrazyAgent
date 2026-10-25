import "../test/load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { formFromRecord, editFromForm } = await import("../src/model-access.ts");

const catalogPool = {
  id: "catalog-pool", members: ["saved-connection"], projects: [], roles: [],
  fallback_allowed: true, provider_quota_policy: "strict",
};

test("partially assigned pools keep their exact configuration through the form", () => {
  for (const pool of [
    catalogPool,
    { ...catalogPool, projects: ["project-first"] },
    { ...catalogPool, roles: ["builder"] },
  ]) {
    const before = structuredClone(pool);
    assert.deepEqual(editFromForm(formFromRecord({ kind: "pool", pool }, "retained-draft")), {
      kind: "replace_pool", pool: before,
    });
    assert.deepEqual(pool, before);
  }
});
