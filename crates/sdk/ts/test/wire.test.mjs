import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { encodeU64 } from "../src/core.ts";
import { decodeBlob, encodeBlob } from "../src/fixture_wire/wire.ts";

const shared = readFileSync(new URL("./wire.json", import.meta.url), "utf8");

const blob = {
  size: 18446744073709551615n,
  data: new Uint8Array([0, 1, 255]),
  parts: [0n, 9007199254740993n],
  tail: null,
};
const plain = { name: "x", mode: "write" };

test("the wire.store input of the shared values is wire.json byte for byte", () => {
  const input = { blob: encodeBlob(blob), plain, at: encodeU64(7n) };
  assert.equal(JSON.stringify(input), shared);
});

test("decodeBlob reads the wire.json blob back to the same values", () => {
  const parsed = JSON.parse(shared);
  assert.deepEqual(decodeBlob(parsed.blob), blob);
  assert.equal(JSON.stringify(encodeBlob(decodeBlob(parsed.blob))), JSON.stringify(parsed.blob));
});
