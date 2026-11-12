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
  for (const code of ["rcli_private_key_writer_unavailable", "native_profile_in_use", "account_profile_operations_full", "account_profile_outcome_unknown_read_same_request", "account_profile_recovery_required"]) {
    const message = api.plainError(code);
    assert.doesNotMatch(message, / \([a-z0-9_]+\)$/, code);
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

test("a kept account set-up reads as configured, never recorded, pending, gone or failed, and only a settled one gets a toast", () => {
  const view = (pending, outcome) => ({ kind: "ok", value: { ticket: "t", generation: "1", reply: { request_id: "r", database_instance: "d", engine_epoch: "e", outcome }, pending } });
  const configured = { configured_only: { receipt: {}, historical_replay: false } };
  assert.equal(api.profileCheck(view(false, configured)), "configured");
  assert.equal(api.profileCheck(view(false, "not_recorded")), "not_recorded");
  assert.equal(api.profileCheck(view(true, configured)), "pending");
  assert.equal(api.profileCheck({ kind: "error", code: "account_profile_operation_missing", message: "" }), "none");
  assert.equal(api.profileCheck({ kind: "error", code: "account_profile_outcome_unknown_read_same_request", message: "" }), "failed");
  assert.equal(api.profileSettledLine("configured"), "The account is set up for agents.");
  assert.match(api.profileSettledLine("not_recorded"), /Save the pool again/);
  for (const check of ["pending", "none", "failed"]) assert.equal(api.profileSettledLine(check), null, check);
  assert.doesNotMatch(api.PROFILE_UNCONFIRMED_LINE, /_|profile/i);
});

test("reading a kept account set-up outside the desktop shell never counts as settled", async () => {
  assert.equal(await api.checkAccountProfile(), "failed");
});

test("setting an account up outside the desktop shell reports the engine as unreachable", async () => {
  const result = await api.readyForAgents("account-1");
  assert.equal(result.kind, "error");
  assert.equal(result.code, "engine_unreachable");
});

test("a pool's saved-key, URL and local members can be set up for agents again from its row", async () => {
  const { keyedMembers, PoolsScreenView } = await import("../src/screens/settings/pools.tsx");
  const { createElement } = await import("react");
  const { renderToStaticMarkup } = await import("react-dom/server");
  const member = (id, kind) => ({ id, kind, provider: "Provider", status: "set_up", status_label: "Set up" });
  const pool = { id: "pool-1", name: "Research", summary: "3 accounts", members: [member("a", "api_key"), member("b", "chatgpt"), member("c", "url"), member("d", "local")], project_count: 0 };
  assert.deepEqual(keyedMembers(pool), ["a", "c", "d"]);
  assert.deepEqual(keyedMembers({ ...pool, members: [member("b", "claude")] }), []);
  const forbidden = () => { throw new Error("static rendering must not act"); };
  const NOT_READ = { kind: "loading" };
  const view = (pools) => renderToStaticMarkup(createElement(PoolsScreenView, { pools: { kind: "ok", value: pools }, accounts: NOT_READ, schedule: NOT_READ, ranking: NOT_READ, limits: NOT_READ, nowMs: 0, selected: undefined, onSelect: forbidden, onNew: forbidden, onEdit: forbidden, onDelete: forbidden, onRetry: forbidden, onRetryOrder: forbidden, onPrepare: forbidden }));
  const markup = view([pool]);
  assert.match(markup, /Research/);
  assert.match(markup, /aria-label="Get ready for agents: Research"/);
  const subscriptions = view([{ ...pool, members: [member("b", "claude")] }]);
  assert.doesNotMatch(subscriptions, /Get ready for agents/);
});

test("an account added to a pool from Add account is set up for agents the way the pool editor sets it up", async () => {
  const { readyAdded } = await import("../src/screens/settings/pools.tsx");
  const accounts = [account("k", "api_key", { name: "Work key" }), account("s", "subscription")];
  const run = async (previous, members) => {
    const noted = [];
    const note = (result) => { noted.push(result.kind === "error" ? result.code : result.kind); return result; };
    return { line: await readyAdded(previous, members, accounts, note), noted };
  };
  assert.deepEqual(await run(["s"], ["s"]), { line: undefined, noted: [] });
  assert.deepEqual(await run([], ["s"]), { line: undefined, noted: [] });
  assert.deepEqual(await run(["s"], ["s", "k"]), { line: "Work key isn't ready for agents yet. CrazyAgent's engine isn't running.", noted: ["engine_unreachable"] });
});

/** A bridge stand-in that keeps one selected account, as the desktop bridge does: inspecting
 * picks the account and gives its ticket, and a set-up with any other ticket is refused as
 * stale_model_access. Output: the commands seen in order and the invoke to install. */
function singleSelectionBridge() {
  const seen = [];
  let selection = 0;
  const invoke = async (command, args) => {
    if (command === "desktop_inspect_model_access") {
      selection += 1;
      const ticket = String(selection);
      seen.push(`inspect ${args.id}`);
      await new Promise((done) => setTimeout(done, 5));
      return { ticket };
    }
    if (command === "desktop_configure_account_profile") {
      seen.push(`configure ${args.ticket}`);
      if (args.ticket !== String(selection)) throw Object.assign(new Error("stale"), { code: "stale_model_access" });
      return { ticket: args.ticket, generation: "1", reply: { request_id: "r", database_instance: "d", engine_epoch: "e", outcome: { configured_only: { receipt: {}, historical_replay: false } } }, pending: false };
    }
    throw new Error(`unexpected command ${command}`);
  };
  return { seen, invoke };
}

test("DESIGN-5B-F2: set-up runs started together from the pool editor and Add account take the bridge's one selection in turn, and both accounts read ready", async () => {
  const { readyAdded } = await import("../src/screens/settings/pools.tsx");
  const bridge = singleSelectionBridge();
  const before = globalThis.window;
  globalThis.window = { __TAURI_INTERNALS__: { invoke: bridge.invoke } };
  try {
    const accounts = [account("a", "api_key", { name: "Editor key" }), account("b", "api_key", { name: "Added key" })];
    const note = (result) => result;
    const lines = await Promise.all([readyAdded([], ["a"], accounts, note), readyAdded([], ["b"], accounts, note)]);
    assert.deepEqual(lines, [undefined, undefined], "a good account never reads not ready because another run moved the selection");
    assert.deepEqual(bridge.seen, ["inspect a", "configure 1", "inspect b", "configure 2"], "one run holds the selection until its set-up ends");
  } finally {
    globalThis.window = before;
  }
});

test("DESIGN-5B-F2: the app holds a pool's Get ready run outside the Pools view, so a view mounted mid-run reads it and a save's run waits its turn", async () => {
  const { readyAdded, readyEach, readyRunning, subscribeReadyRun } = await import("../src/screens/settings/pools.tsx");
  const bridge = singleSelectionBridge();
  const before = globalThis.window;
  globalThis.window = { __TAURI_INTERNALS__: { invoke: bridge.invoke } };
  const heard = [];
  const stop = subscribeReadyRun(() => heard.push(readyRunning()));
  try {
    assert.equal(readyRunning(), null, "no run is held before Get ready");
    const note = (result) => result;
    const getReady = readyEach([{ id: "a", name: "Editor key" }], note, "pool-1");
    const saved = readyAdded([], ["b"], [account("b", "api_key", { name: "Added key" })], note);
    await new Promise((done) => setTimeout(done, 1));
    assert.equal(readyRunning(), "pool-1", "a Pools view mounted now shows the pool's Get ready as pending");
    assert.deepEqual(await getReady, []);
    assert.equal(await saved, undefined);
    assert.equal(readyRunning(), null, "the hold ends with the last run");
    assert.equal(heard[0], "pool-1");
    assert.equal(heard.at(-1), null);
    assert.deepEqual(bridge.seen, ["inspect a", "configure 1", "inspect b", "configure 2"]);
  } finally {
    stop();
    globalThis.window = before;
  }
});
