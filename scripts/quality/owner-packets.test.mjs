import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { HELPER, check, packetLines } from "./owner-packets.mjs";

const script = fileURLToPath(new URL("./owner-packets.mjs", import.meta.url));

/** A client packet built by hand, as every owner client built it before the helper. */
const HAND_BUILT = [
  "let mut packet = [0; 37];",
  'packet[..4].copy_from_slice(b"CAO1");',
  "packet[5..].copy_from_slice(&self.token);",
].join("\n");

/** The same packet from the helper, and a 4-byte length copy that is not a packet. */
const CLEAN = [
  'let packet = owner_packet(b"CAO1", client_tag(self.kind), &self.token);',
  "frame[..4].copy_from_slice(&(length as u32).to_be_bytes());",
  "fn known(&self, packet: &[u8; 37]) -> bool;",
  "let wide = [0_u8; 370];",
].join("\n");

/**
 * A throwaway git repository holding the given files, all tracked.
 * Input: a map of relative path to text. Output: the repository folder.
 */
function repository(files) {
  const root = mkdtempSync(path.join(tmpdir(), "owner-packets-"));
  execFileSync("git", ["init", "-q"], { cwd: root });
  for (const [name, text] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  execFileSync("git", ["add", "-A"], { cwd: root });
  return root;
}

test("a hand-built packet fails in sources and integration tests, in every literal form", () => {
  const problems = check(
    new Map([
      ["crates/crazyagent/src/engine/owner/transport/watch.rs", HAND_BUILT],
      ["crates/crazyagent/tests/owner_client.rs", "let mut auth = [0u8;37];"],
      ["crates/crazyagent/src/ide/bridge.rs", "let mut packet = [0_u8; 37];"],
      [
        "crates/crazyagent/src/engine/owner/transport/ide.rs",
        "let mut packet = [0; secret_frame::OWNER_PACKET_BYTES];",
      ],
    ]),
  );
  assert.deepEqual(problems, [
    "crates/crazyagent/src/engine/owner/transport/watch.rs:1: build the owner packet with secret_frame::owner_packet",
    "crates/crazyagent/tests/owner_client.rs:1: build the owner packet with secret_frame::owner_packet",
    "crates/crazyagent/src/ide/bridge.rs:1: build the owner packet with secret_frame::owner_packet",
    "crates/crazyagent/src/engine/owner/transport/ide.rs:1: build the owner packet with secret_frame::owner_packet",
  ]);
});

test("the helper call, a 4-byte length copy and a packet parameter type pass", () => {
  assert.deepEqual(packetLines(CLEAN), []);
  assert.deepEqual(check(new Map([["crates/crazyagent/src/engine.rs", CLEAN]])), []);
});

test("the helper file itself may lay out the packet", () => {
  assert.deepEqual(check(new Map([[HELPER, HAND_BUILT]])), []);
});

test("the script exits 1 on a tracked hand-built packet and 0 once it uses the helper", () => {
  const dirty = repository({ "crates/crazyagent/src/a.rs": `\n${HAND_BUILT}\n` });
  const clean = repository({ "crates/crazyagent/src/a.rs": CLEAN, [HELPER]: HAND_BUILT });
  try {
    assert.throws(
      () => execFileSync("node", [script, dirty], { encoding: "utf8", stdio: "pipe" }),
      (error) => error.status === 1 && error.stderr.includes("crates/crazyagent/src/a.rs:2:"),
    );
    const passed = execFileSync("node", [script, clean], { encoding: "utf8" });
    assert.match(passed, /every owner packet is built by the helper/);
  } finally {
    rmSync(dirty, { recursive: true, force: true });
    rmSync(clean, { recursive: true, force: true });
  }
});
