import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as core from "@crazyagent/sdk/core";
import * as generated from "@crazyagent/sdk/generated";
import { run } from "../example/quit/main.ts";

const manifest = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the_package_exports_resolve_by_name", () => {
  assert.deepEqual(manifest, {
    name: "@crazyagent/sdk",
    private: true,
    type: "module",
    exports: {
      "./core": "./src/core.ts",
      "./generated": "./src/generated/index.ts",
    },
    engines: { node: ">=26.13.1" },
  });
  assert.equal(typeof core.call, "function");
  assert.equal(typeof core.subscribe, "function");
  assert.equal(generated.settings.SAVE_QUIT.name, "settings.save_quit");
  assert.deepEqual(generated.settings.SETTINGS_EVENT_KINDS, ["AgentQuitSaved"]);
});

test("the_sample_saves_quit_and_sees_its_event_through_a_test_host", async () => {
  const quit = { on_quit: "stop", left_running: "never", hours: 12 };
  const calls = [];
  const subscriptions = [];
  let stops = 0;
  const host = {
    call: async (capability, input) => {
      calls.push([capability, JSON.stringify(input)]);
      return { ok: null };
    },
    subscribe: (kinds, after, onRecord) => {
      subscriptions.push([kinds, after]);
      onRecord({
        seq: "1",
        revision: "1",
        aggregate: "settings",
        kind: "AgentQuitSaved",
        schema: 1,
        body: { settings: quit },
      });
      return () => {
        stops += 1;
      };
    },
  };
  const first = await run(host);
  assert.deepEqual(calls, [
    ["settings.save_quit", '{"settings":{"on_quit":"stop","left_running":"never","hours":12}}'],
  ]);
  assert.deepEqual(subscriptions, [[["AgentQuitSaved"], null]]);
  assert.deepEqual(first, {
    ok: true,
    value: { seq: 1n, event: { kind: "AgentQuitSaved", body: { settings: quit } } },
  });
  assert.equal(stops, 1);
});

test("the_sample_returns_the_save_refusal_and_does_not_subscribe", async () => {
  const refused = {
    kind: "not_granted",
    capability: "settings.save_quit",
    needs: "settings.write",
  };
  const host = {
    call: async () => ({ refused }),
    subscribe: () => assert.fail("no subscribe after a refused save"),
  };
  assert.deepEqual(await run(host), { ok: false, refusal: refused });
});
