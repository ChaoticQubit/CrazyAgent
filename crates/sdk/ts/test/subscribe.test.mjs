import { test } from "node:test";
import assert from "node:assert/strict";

import { LIST_ITEMS_MAX, subscribe } from "../src/core.ts";
import { SETTINGS_EVENT_KINDS, decodeSettingsEvent } from "../src/generated/settings.ts";
import { WIRE_EVENT_KINDS, decodeWireEvent } from "../src/fixture_wire/wire.ts";

const quit = { on_quit: "stop", left_running: "never", hours: 12 };

const record = (seq, kind = "AgentQuitSaved", body = { settings: quit }) => ({
  seq,
  revision: "1",
  aggregate: "settings",
  kind,
  schema: 1,
  body,
});

const failed = (code) => ({ ok: false, refusal: { kind: "failed", code } });

/** A test host that keeps each subscribe call and its record callback, and counts stops. */
function recordingHost() {
  const state = { subscribed: [], push: null, stops: 0 };
  const host = {
    call: async () => assert.fail("no call in a subscribe row"),
    subscribe: (kinds, after, onRecord) => {
      state.subscribed.push([kinds, after]);
      state.push = onRecord;
      return () => {
        state.stops += 1;
      };
    },
  };
  return { host, state };
}

test("subscribe_delivers_a_decoded_settings_event", () => {
  const { host, state } = recordingHost();
  const seen = [];
  subscribe(host, SETTINGS_EVENT_KINDS, decodeSettingsEvent, null, (event) => seen.push(event));
  state.push(record("5"));
  assert.deepEqual(state.subscribed, [[SETTINGS_EVENT_KINDS, null]]);
  assert.deepEqual(seen, [
    {
      ok: true,
      value: { seq: 5n, event: { kind: "AgentQuitSaved", body: { settings: quit } } },
    },
  ]);
});

test("subscribe_skips_a_seen_or_older_seq", () => {
  const { host, state } = recordingHost();
  const seen = [];
  subscribe(host, SETTINGS_EVENT_KINDS, decodeSettingsEvent, 4n, (event) => seen.push(event));
  for (const seq of ["3", "4", "5", "5", "2", "6"]) {
    state.push(record(seq));
  }
  assert.deepEqual(
    seen.map((event) => event.value.seq),
    [5n, 6n],
  );
});

test("subscribe_passes_after_to_the_host_as_a_decimal", () => {
  const { host, state } = recordingHost();
  subscribe(host, ["AgentQuitSaved"], decodeSettingsEvent, 2n ** 64n - 1n, () => {});
  subscribe(host, ["AgentQuitSaved"], decodeSettingsEvent, 0n, () => {});
  assert.deepEqual(state.subscribed, [
    [["AgentQuitSaved"], "18446744073709551615"],
    [["AgentQuitSaved"], "0"],
  ]);
  assert.throws(
    () => subscribe(host, ["AgentQuitSaved"], decodeSettingsEvent, -1n, () => {}),
    { name: "RangeError", message: "sdk_u64_out_of_range" },
  );
  assert.equal(state.subscribed.length, 2);
});

test("subscribe_drops_a_kind_it_did_not_ask_for", () => {
  const { host, state } = recordingHost();
  const seen = [];
  subscribe(host, ["AgentQuitSaved"], decodeSettingsEvent, null, (event) => seen.push(event));
  state.push(record("1", "BlobStored", { at: "1" }));
  state.push(record("2", "SomethingElse"));
  const wide = recordingHost();
  subscribe(wide.host, ["AgentQuitSaved", "Other"], decodeSettingsEvent, null, (event) =>
    seen.push(event),
  );
  wide.state.push(record("3", "Other", {}));
  state.push(record("4"));
  assert.deepEqual(
    seen.map((event) => event.value.seq),
    [4n],
  );
});

test("subscribe_reports_an_undecodable_body_and_keeps_going", () => {
  const { host, state } = recordingHost();
  const seen = [];
  subscribe(host, WIRE_EVENT_KINDS, decodeWireEvent, null, (event) => seen.push(event));
  const blob = { size: "1", data: "", parts: [], tail: null };
  state.push(record("1", "BlobStored", { at: "01", data: "", blob }));
  state.push(record("x1", "BlobStored", { at: "1", data: "", blob }));
  state.push(null);
  state.push(record("2", "BlobStored", { at: "7", data: "AAH/", blob }));
  assert.deepEqual(seen.slice(0, 3), [
    failed("sdk_event_undecodable"),
    failed("sdk_event_undecodable"),
    failed("sdk_event_undecodable"),
  ]);
  assert.equal(seen.length, 4);
  assert.equal(seen[3].ok, true);
  assert.equal(seen[3].value.seq, 2n);
  assert.equal(seen[3].value.event.body.at, 7n);
  assert.deepEqual(seen[3].value.event.body.data, new Uint8Array([0, 1, 255]));
});

test("subscribe_reports_a_transport_throw", () => {
  const seen = [];
  const host = {
    call: async () => assert.fail("no call"),
    subscribe: () => {
      throw new Error("pipe closed");
    },
  };
  const stop = subscribe(host, SETTINGS_EVENT_KINDS, decodeSettingsEvent, null, (event) =>
    seen.push(event),
  );
  assert.deepEqual(seen, [failed("sdk_transport_failed")]);
  stop();
  stop();
  assert.equal(seen.length, 1);
});

test("unsubscribe_stops_delivery_and_is_idempotent", () => {
  const { host, state } = recordingHost();
  const seen = [];
  const stop = subscribe(host, SETTINGS_EVENT_KINDS, decodeSettingsEvent, null, (event) =>
    seen.push(event),
  );
  state.push(record("1"));
  stop();
  stop();
  state.push(record("2"));
  assert.equal(state.stops, 1);
  assert.deepEqual(
    seen.map((event) => event.value.seq),
    [1n],
  );
});

test("subscribe_refuses_empty_or_oversized_kinds", () => {
  const { host, state } = recordingHost();
  const oversized = Array.from({ length: LIST_ITEMS_MAX + 1 }, (_, index) => `K${index}`);
  for (const kinds of [[], oversized]) {
    assert.throws(
      () => subscribe(host, kinds, decodeSettingsEvent, null, () => {}),
      { name: "RangeError", message: "sdk_subscribe_kinds" },
    );
  }
  const full = oversized.slice(0, LIST_ITEMS_MAX);
  subscribe(host, full, decodeSettingsEvent, null, () => {});
  assert.equal(state.subscribed.length, 1);
});
