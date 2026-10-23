import assert from "node:assert/strict";
import test from "node:test";
import * as modelAccess from "../src/model-access.ts";

test("configured account input accepts ordinary endpoint fields without retaining secrets or routing identities", () => {
  assert.equal(typeof modelAccess.configuredAccountInput, "function", "production must validate ordinary configured account input");
  const value = modelAccess.configuredAccountInput("api_key", " Work ", "https://api.example.test/v1", " model-a ");
  assert.deepEqual(value, { provider: "api_key", label: "Work", url: "https://api.example.test/v1", model: "model-a" });
  assert.deepEqual(Object.keys(value), ["provider", "label", "url", "model"]);
  assert.deepEqual(modelAccess.configuredAccountInput("local", "", "http://127.0.0.1:11434/v1", "local-model"), {
    provider: "local", label: "", url: "http://127.0.0.1:11434/v1", model: "local-model",
  });
});

test("no-key setup is restricted to explicit HTTP loopback endpoints", () => {
  assert.equal(typeof modelAccess.configuredAccountInput, "function");
  for (const url of ["http://localhost:8080/v1", "http://[::1]:8080/v1", "http://127.20.30.40/v1"]) {
    assert.equal(modelAccess.configuredAccountInput("local", "", url, "model").url, url);
  }
  for (const url of ["https://localhost/v1", "http://example.test/v1", "http://localhost.example.test/v1", "http://0.0.0.0/v1", "file:///tmp/model"]) {
    assert.throws(() => modelAccess.configuredAccountInput("local", "", url, "model"), /HTTP loopback/);
  }
});

test("configured inputs reject embedded credentials, URL side channels and unbounded or closed-choice fields", () => {
  assert.equal(typeof modelAccess.configuredAccountInput, "function");
  for (const url of ["https://user:password@example.test/v1", "https://example.test/v1?key=x", "https://example.test/v1#key", "ftp://example.test/v1", "not-a-url"]) {
    assert.throws(() => modelAccess.configuredAccountInput("api_key", "", url, "model"), /endpoint/i);
  }
  assert.throws(() => modelAccess.configuredAccountInput("codex", "", "https://example.test", "model"), /Choose API key or local/);
  assert.throws(() => modelAccess.configuredAccountInput("api_key", "x".repeat(97), "https://example.test", "model"), /96 UTF-8 bytes/);
  assert.throws(() => modelAccess.configuredAccountInput("api_key", "line\nbreak", "https://example.test", "model"), /one line/);
  assert.throws(() => modelAccess.configuredAccountInput("api_key", "", "https://example.test/" + "x".repeat(2048), "model"), /2048 UTF-8 bytes/);
  for (const model of ["", "x".repeat(257), "model\nsecret", "\u202emodel"]) {
    assert.throws(() => modelAccess.configuredAccountInput("api_key", "", "https://example.test", model), /model/i);
  }
});
