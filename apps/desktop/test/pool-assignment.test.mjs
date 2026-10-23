import assert from "node:assert/strict";
import test from "node:test";
import * as access from "../src/model-access.ts";

test("explicit fresh project assignment preserves every pool field and adds only project_leader", () => {
  assert.equal(typeof access.withPoolProject, "function", "the actual pool form needs an explicit bounded project-assignment operation");
  const pool = {
    id: "retained-pool", members: ["off-page", "chosen-connection"],
    projects: ["prior-project"], roles: ["builder"], fallback_allowed: true,
    provider_quota_policy: "strict",
  };
  const form = access.formFromRecord({ kind: "pool", pool }, "owner-ticket");
  const result = access.withPoolProject(form, { project_id: "chosen-project", fresh: true });
  assert.deepEqual(access.editFromForm(result), {
    kind: "replace_pool",
    pool: { ...pool, projects: ["prior-project", "chosen-project"], roles: ["builder", "project_leader"] },
  });
  assert.equal(result.ticket, form.ticket);
  assert.equal(result.id, form.id);
  assert.equal(result.action, "replace");
  assert.deepEqual(access.withPoolProject(result, { project_id: "chosen-project", fresh: true }), result);
  assert.equal(form.projects, "prior-project");
  assert.equal(form.roles, "builder");
  assert.deepEqual(pool.projects, ["prior-project"]);
});

test("Unassigned assignment requires a real fresh project-shaped input and an already saved pool", () => {
  assert.equal(typeof access.withPoolProject, "function");
  const pool = { id: "pool", members: ["connection"], projects: [], roles: [], fallback_allowed: false };
  const form = access.formFromRecord({ kind: "pool", pool }, "owner-ticket");
  assert.deepEqual(access.editFromForm(access.withPoolProject(form, { project_id: "native-project", fresh: true })), {
    kind: "replace_pool", pool: { ...pool, projects: ["native-project"], roles: ["project_leader"] },
  });
  assert.throws(() => access.withPoolProject(form, { project_id: "native-project", fresh: false }));
  assert.throws(() => access.withPoolProject(form, { project_id: "", fresh: true }));
  assert.throws(() => access.withPoolProject(form, { project_id: "not a reference", fresh: true }));
  assert.throws(() => access.withPoolProject({ ...form, action: "create" }, { project_id: "project", fresh: true }));
  assert.throws(() => access.withPoolProject({ ...form, action: "remove" }, { project_id: "project", fresh: true }));
  assert.throws(() => access.withPoolProject({ ...form, collection: "connections" }, { project_id: "project", fresh: true }));
  assert.deepEqual(pool.projects, []);
  assert.deepEqual(pool.roles, []);
});

test("explicit project assignment keeps existing complete scope sets within the original 64-reference bounds", () => {
  assert.equal(typeof access.withPoolProject, "function");
  const projects = Array.from({ length: 64 }, (_, index) => `project-${index}`);
  const roles = ["project_leader", ...Array.from({ length: 63 }, (_, index) => `role-${index}`)];
  const form = access.formFromRecord({ kind: "pool", pool: {
    id: "pool", members: ["connection"], projects, roles, fallback_allowed: false,
  } }, "ticket");
  assert.deepEqual(access.withPoolProject(form, { project_id: "project-0", fresh: true }), form);
  assert.throws(() => access.withPoolProject(form, { project_id: "overflow", fresh: true }));
  const noLeader = { ...form, roles: Array.from({ length: 64 }, (_, index) => `worker-${index}`).join(", ") };
  assert.throws(() => access.withPoolProject(noLeader, { project_id: "project-0", fresh: true }));
  assert.equal(noLeader.roles.split(",").length, 64);
});
