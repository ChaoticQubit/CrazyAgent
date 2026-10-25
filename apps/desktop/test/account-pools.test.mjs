import assert from "node:assert/strict";
import test from "node:test";
import * as modelAccess from "../src/model-access.ts";

test("a generated account pool can be edited without inventing project or role scope", () => {
  const pool = { id: "personal-pool", members: ["personal-connection"], projects: [], roles: [], fallback_allowed: false };
  const form = modelAccess.formFromRecord({ kind: "pool", pool }, "inspected-pool");
  assert.equal(form.projects, "");
  assert.equal(form.roles, "");
  assert.deepEqual(modelAccess.editFromForm(form), { kind: "replace_pool", pool },
    "an unrestricted saved pool must remain editable with its exact identity and empty scopes");
  const strict = modelAccess.editFromForm({ ...form, providerQuotaPolicy: "strict" });
  assert.deepEqual(strict.pool, { ...pool, provider_quota_policy: "strict" });
  assert.deepEqual(pool.projects, []);
  assert.deepEqual(pool.roles, []);
});

test("connection checkbox edits preserve the exact pool and other selected connections", () => {
  assert.equal(typeof modelAccess.setPoolConnection, "function", "actual pool forms need a bounded connection selection helper");
  const pool = { id: "existing-pool", members: ["off-page", "first"], projects: ["project"], roles: ["builder"], fallback_allowed: true, provider_quota_policy: "strict" };
  const form = modelAccess.formFromRecord({ kind: "pool", pool }, "inspected-pool");
  const selected = modelAccess.setPoolConnection(form, "second", true);
  assert.equal(selected.id, pool.id);
  assert.equal(selected.ticket, form.ticket);
  assert.deepEqual(modelAccess.editFromForm(selected), { kind: "replace_pool", pool: { ...pool, members: ["off-page", "first", "second"] } });
  assert.deepEqual(modelAccess.setPoolConnection(selected, "second", true), selected);
  const deselected = modelAccess.setPoolConnection(selected, "first", false);
  assert.deepEqual(modelAccess.editFromForm(deselected).pool.members, ["off-page", "second"]);
  assert.equal(form.members, "off-page, first");
  assert.throws(() => modelAccess.setPoolConnection(form, "unsafe value", true));
  assert.throws(() => modelAccess.setPoolConnection({ ...form, collection: "connections" }, "second", true));
  assert.throws(() => modelAccess.setPoolConnection({ ...form, action: "remove" }, "second", true));
  const full = { ...form, members: Array.from({ length: 15 }, (_, index) => `connection-${index}`).join(", ") };
  assert.throws(() => modelAccess.setPoolConnection(full, "overflow", true), /A pool can hold up to 15 accounts/);
  assert.equal(modelAccess.setPoolConnection(full, "connection-0", true).members, full.members);
  assert.equal(modelAccess.setPoolConnection({ ...form, members: "only" }, "only", false).members, "");
});

test("a pool keeps its display name through edits and a rename leaves its id alone", () => {
  const pool = { id: "pool-work-1", name: "Work", members: ["first"], projects: [], roles: [], fallback_allowed: false };
  const form = modelAccess.formFromRecord({ kind: "pool", pool }, "inspected-pool");
  assert.equal(form.name, "Work");
  assert.deepEqual(modelAccess.editFromForm(form), { kind: "replace_pool", pool });
  const renamed = modelAccess.editFromForm({ ...form, name: "  Daily work  " });
  assert.deepEqual(renamed, { kind: "replace_pool", pool: { ...pool, name: "Daily work" } });
  const unnamed = modelAccess.editFromForm({ ...form, name: "" });
  assert.equal("name" in unnamed.pool, false, "an empty name falls back to the pool id");
  assert.throws(() => modelAccess.editFromForm({ ...form, name: "x".repeat(65) }), /1 to 64 characters/);
  assert.throws(() => modelAccess.editFromForm({ ...form, name: "tab\there" }), /1 to 64 characters/);
  assert.equal(modelAccess.newForm("pools", "t").name, "");
});

test("a pool form refuses a sixteenth account before the engine sees it", () => {
  const form = { ...modelAccess.newForm("pools", "t"), id: "pool-big", members: Array.from({ length: 16 }, (_, index) => `c-${index}`).join(", ") };
  assert.throws(() => modelAccess.editFromForm(form), /A pool can hold up to 15 accounts/);
  const fifteen = { ...form, members: Array.from({ length: 15 }, (_, index) => `c-${index}`).join(", ") };
  assert.equal(modelAccess.editFromForm(fifteen).pool.members.length, 15);
});

test("a pool scoped to only projects or only roles keeps that exact partial scope through an edit", () => {
  const catalogPool = { id: "catalog-pool", members: ["catalog-connection"], projects: [], roles: [], fallback_allowed: false };
  for (const pool of [{ ...catalogPool, projects: ["project-first"] }, { ...catalogPool, roles: ["builder"] }]) {
    const before = structuredClone(pool);
    assert.deepEqual(modelAccess.editFromForm(modelAccess.formFromRecord({ kind: "pool", pool }, "retained-draft")), { kind: "replace_pool", pool: before });
    assert.deepEqual(pool, before);
  }
});
