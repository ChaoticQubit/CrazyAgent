import assert from "node:assert/strict";
import test from "node:test";
import { editFromForm, formFromRecord, newForm, recordId, rebaseForm } from "../src/model-access.ts";

test("production forms generate all eight typed metadata edits without credentials", () => {
  const account = { ...newForm("accounts", "11"), id: "account" };
  assert.deepEqual(editFromForm(account), { kind: "add_account", account_ref: "account" });
  const accountRecord = { kind: "account", account_ref: "account" };
  assert.deepEqual(editFromForm(formFromRecord(accountRecord, "12", true)), { kind: "remove_account", account_ref: "account" });
  const connection = { ...newForm("connections", "13"), id: "connection", provider: "provider", harness: "harness", protocol: "openai", account: "account", model: "author/model", author: "author", url: "https://example.test/v1", auth: "api_key", credential: "stored-ref" };
  const created = editFromForm(connection);
  assert.equal(created.kind, "create_connection");
  assert.deepEqual(created.connection.auth, { method: "api_key", credential_ref: "stored-ref" });
  const connectionRecord = { kind: "connection", connection: created.connection };
  assert.deepEqual(editFromForm(formFromRecord(connectionRecord, "14")), { kind: "replace_connection", connection: created.connection });
  assert.deepEqual(editFromForm(formFromRecord(connectionRecord, "15", true)), { kind: "remove_connection", id: "connection" });
  const pool = { ...newForm("pools", "16"), id: "pool", members: "connection, second", projects: "project", roles: "builder", fallback: true };
  const poolEdit = editFromForm(pool);
  assert.equal(poolEdit.kind, "create_pool");
  assert.deepEqual(poolEdit.pool.members, ["connection", "second"]);
  const poolRecord = { kind: "pool", pool: poolEdit.pool };
  assert.deepEqual(editFromForm(formFromRecord(poolRecord, "17")), { kind: "replace_pool", pool: poolEdit.pool });
  assert.deepEqual(editFromForm(formFromRecord(poolRecord, "18", true)), { kind: "remove_pool", id: "pool" });
  assert.equal(recordId(connectionRecord), "connection");
});

test("production validation rejects unsafe references, credential-bearing URLs and unbounded lists", () => {
  for (const id of ["", "../key", "secret value", "a".repeat(129)]) {
    assert.throws(() => editFromForm({ ...newForm("accounts", "1"), id }));
  }
  const connection = { ...newForm("connections", "2"), id: "connection", provider: "provider", harness: "harness", protocol: "openai", account: "account", model: "model", author: "author", url: "https://example.test/v1" };
  for (const url of ["https://user:password@example.test", "https://example.test?key=secret", "https://example.test#token", "file:///tmp/key", "https://example.test/a b", "https://example.test\\path", "https://example.test/" + "é".repeat(1020)]) {
    assert.throws(() => editFromForm({ ...connection, url }));
  }
  for (const members of ["", "a,a", Array.from({ length: 65 }, (_, i) => `c${i}`).join(","), "a,,b"]) {
    assert.throws(() => editFromForm({ ...newForm("pools", "3"), id: "pool", members, projects: "project", roles: "builder" }));
  }
  assert.throws(() => editFromForm({ ...connection, model: "<script>" }));
  assert.throws(() => editFromForm({ ...connection, auth: "native_managed", credential: "profile" }));
  const native = editFromForm({ ...connection, endpoint: "native", protocol: "native", auth: "native_managed", credential: "profile" });
  assert.deepEqual(native.connection.endpoint, { kind: "native" });
  assert.deepEqual(native.connection.auth, { method: "native_managed", profile_ref: "profile" });
});

test("retained edits rebase only after an explicit read of the same identity and collection", () => {
  const record = { kind: "pool", pool: { id: "pool", members: ["connection"], projects: ["project"], roles: ["builder"], fallback_allowed: false } };
  const form = { ...formFromRecord(record, "old"), roles: "reviewer" };
  const detail = { ticket: "new", collection: "pools", record };
  assert.equal(rebaseForm(form, detail).roles, "reviewer");
  assert.equal(rebaseForm(form, detail).ticket, "new");
  assert.equal(form.ticket, "old");
  assert.throws(() => rebaseForm(form, { ...detail, record: { ...record, pool: { ...record.pool, id: "other" } } }));
  assert.throws(() => rebaseForm(form, { ...detail, collection: "connections" }));
  const create = { ...newForm("accounts", "old"), id: "account" };
  assert.equal(rebaseForm(create, { ticket: "fresh", collection: "accounts" }).ticket, "fresh");
});
