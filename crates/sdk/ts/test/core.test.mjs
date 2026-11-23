import { test } from "node:test";
import assert from "node:assert/strict";

import { call, decodeBytes, decodeU64, encodeBytes, encodeU64 } from "../src/core.ts";
import { COUNT, count } from "../src/fixture/fixture.ts";

const text = (bytes) => new TextDecoder().decode(bytes);
const ascii = (value) => new TextEncoder().encode(value);

test("encodeBytes matches the RFC 4648 vectors", () => {
  const vectors = [
    ["", ""],
    ["f", "Zg=="],
    ["fo", "Zm8="],
    ["foo", "Zm9v"],
    ["foob", "Zm9vYg=="],
    ["fooba", "Zm9vYmE="],
    ["foobar", "Zm9vYmFy"],
  ];
  for (const [plain, encoded] of vectors) {
    assert.equal(encodeBytes(ascii(plain)), encoded);
    assert.equal(text(decodeBytes(encoded)), plain);
  }
  const every = Uint8Array.from({ length: 256 }, (_, index) => index);
  assert.deepEqual(decodeBytes(encodeBytes(every)), every);
});

test("decodeBytes refuses non-canonical base64", () => {
  for (const value of ["Zg", "Zg=", "Zh==", "Z===", "!!!!"]) {
    assert.throws(() => decodeBytes(value), { name: "TypeError", message: "sdk_wire_bytes" }, value);
  }
});

test("u64 round trips 0, 2^53 + 1 and 2^64 - 1 and refuses 2^64, -1, \"01\", \"1e3\", 7", () => {
  for (const value of [0n, 2n ** 53n + 1n, 2n ** 64n - 1n]) {
    assert.equal(decodeU64(encodeU64(value)), value);
  }
  assert.equal(encodeU64(2n ** 64n - 1n), "18446744073709551615");
  assert.throws(() => encodeU64(2n ** 64n), { name: "RangeError", message: "sdk_u64_out_of_range" });
  assert.throws(() => encodeU64(-1n), { name: "RangeError", message: "sdk_u64_out_of_range" });
  for (const value of ["18446744073709551616", "-1", "01", "1e3", 7]) {
    assert.throws(() => decodeU64(value), { name: "TypeError", message: "sdk_wire_u64" }, String(value));
  }
});

test("call returns a refusal value and never throws", async () => {
  const refused = { kind: "not_granted", capability: "settings.save_quit", needs: "settings.write" };
  const host = { call: async () => ({ refused }) };
  const result = await call(host, COUNT, {}, () => assert.fail("no output to decode"));
  assert.deepEqual(result, { ok: false, refusal: refused });
});

test("call maps a throwing transport to failed sdk_transport_failed", async () => {
  const host = {
    call: async () => {
      throw new Error("pipe closed");
    },
  };
  const result = await call(host, COUNT, {}, () => null);
  assert.deepEqual(result, { ok: false, refusal: { kind: "failed", code: "sdk_transport_failed" } });
});

test("count sends the exact envelope", async () => {
  const sent = [];
  const host = {
    call: async (capability, input) => {
      sent.push([capability, input]);
      return { ok: 3 };
    },
  };
  const result = await count(host, [true, false]);
  assert.deepEqual(sent, [["fixture.count", { flags: [true, false] }]]);
  assert.deepEqual(result, { ok: true, value: 3 });
});
