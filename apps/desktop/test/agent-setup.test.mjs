import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const api = await import("../src/api.ts");
const { setupTargets, setupNote } = await import("../src/screens/settings/pools.tsx");

const account = (id, method, patch = {}) => ({ id, name: `Account ${id}`, provider: method === "subscription" ? "chatgpt" : "openrouter",
  provider_name: "Provider", method, method_label: "Label", url: null, model: null, status: "key_saved", status_label: "Key saved", ...patch });

test("only key and URL accounts newly added to a pool are set up for agents, once each", () => {
  const accounts = [account("a", "api_key"), account("b", "url"), account("c", "subscription"), account("d", "api_key")];
  assert.deepEqual(setupTargets([], ["a", "b", "c"], accounts).map((item) => item.id), ["a", "b"]);
  assert.deepEqual(setupTargets(["a"], ["a", "b", "d"], accounts).map((item) => item.id), ["b", "d"]);
  assert.deepEqual(setupTargets(["a", "b"], ["a"], accounts), []);
  assert.deepEqual(setupTargets([], ["missing"], accounts), []);
});

test("accounts that need no harness profile count as ready; other refusals read as plain sentences", () => {
  assert.equal(api.profileNotNeeded("account_profile_harness_unsupported"), true);
  assert.equal(api.profileNotNeeded("account_profile_auth_unsupported"), true);
  assert.equal(api.profileNotNeeded("native_profile_in_use"), false);
  for (const code of ["rcli_private_key_writer_unavailable", "native_profile_in_use", "account_profile_operations_full", "account_profile_outcome_unknown_read_same_request"]) {
    const message = api.plainError(code);
    assert.notEqual(message, "Something went wrong. Try again.", code);
    assert.doesNotMatch(message, /_|rcli|profile/i, code);
  }
});

test("the note after saving names the accounts that are not ready for agents yet", () => {
  assert.equal(setupNote([]), undefined);
  assert.equal(setupNote([{ name: "Router", message: "An agent is using this account now. Try again after it stops." }]),
    "Router isn't ready for agents yet. An agent is using this account now. Try again after it stops.");
  assert.equal(setupNote([{ name: "A", message: "m." }, { name: "B", message: "n." }]), "A and B aren't ready for agents yet. m.");
  assert.equal(setupNote([{ name: "A", message: "m." }, { name: "B", message: "n." }, { name: "C", message: "o." }]), "A, B and C aren't ready for agents yet. m.");
});

test("setting an account up outside the desktop shell reports the engine as unreachable", async () => {
  const result = await api.readyForAgents("account-1");
  assert.equal(result.kind, "error");
  assert.equal(result.code, "engine_unreachable");
});
