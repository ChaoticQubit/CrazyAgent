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
  const full = { ...form, members: Array.from({ length: 64 }, (_, index) => `connection-${index}`).join(", ") };
  assert.throws(() => modelAccess.setPoolConnection(full, "overflow", true));
  assert.equal(modelAccess.setPoolConnection(full, "connection-0", true).members, full.members);
  assert.equal(modelAccess.setPoolConnection({ ...form, members: "only" }, "only", false).members, "");
});
