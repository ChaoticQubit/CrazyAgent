import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { BLOCKED_SIGN_IN, borrowSignIn, engineProfileHomes, managedCodexHome, returnSignIn } from "./v1-flow-auth.mjs";

const SIGN_IN = "{\"synthetic\":\"sign-in file for a filesystem test\"}";

function scratch() {
  return mkdtempSync(join(tmpdir(), "v1-flow-auth-"));
}

function signedIn(root) {
  const home = join(root, "codex-home");
  mkdirSync(home, { mode: 0o700 });
  writeFileSync(join(home, "auth.json"), SIGN_IN, { mode: 0o600 });
  return home;
}

function engineHome(root, key) {
  const home = join(root, "engine", "native-profiles", key, "home");
  mkdirSync(home, { recursive: true, mode: 0o700 });
  return home;
}

test("a missing managed Codex sign-in fails with the plain BLOCKED message", () => {
  const root = scratch();
  const empty = join(root, "empty");
  mkdirSync(empty);
  const linked = join(root, "linked");
  mkdirSync(linked);
  symlinkSync(join(signedIn(root), "auth.json"), join(linked, "auth.json"));
  for (const env of [{}, { CRAZYAGENT_MANAGED_CODEX_HOME: "" }, { CRAZYAGENT_MANAGED_CODEX_HOME: "codex-home" },
    { CRAZYAGENT_MANAGED_CODEX_HOME: empty }, { CRAZYAGENT_MANAGED_CODEX_HOME: linked }]) {
    assert.throws(() => managedCodexHome(env), { message: BLOCKED_SIGN_IN });
  }
  assert.match(BLOCKED_SIGN_IN, /^BLOCKED: /);
  assert.match(BLOCKED_SIGN_IN, /CRAZYAGENT_MANAGED_CODEX_HOME/);
});

test("a signed-in managed Codex home is accepted as given", () => {
  const home = signedIn(scratch());
  assert.equal(managedCodexHome({ CRAZYAGENT_MANAGED_CODEX_HOME: home }), home);
});

test("only engine profile homes named by a profile digest are listed", () => {
  const root = scratch();
  assert.deepEqual(engineProfileHomes(join(root, "engine")), []);
  const first = engineHome(root, "b".repeat(64));
  const second = engineHome(root, "a".repeat(64));
  mkdirSync(join(root, "engine", "native-profiles", "not-a-profile", "home"), { recursive: true });
  mkdirSync(join(root, "engine", "native-profiles", "c".repeat(64)));
  assert.deepEqual(engineProfileHomes(join(root, "engine")), [second, first]);
});

test("the sign-in moves into the engine profile and back with its bytes unchanged", () => {
  const root = scratch();
  const source = signedIn(root);
  const target = engineHome(root, "d".repeat(64));
  const ledger = join(root, "borrowed.json");
  assert.equal(borrowSignIn(source, join(root, "engine"), ledger), target);
  assert.equal(existsSync(join(source, "auth.json")), false);
  assert.equal(readFileSync(join(target, "auth.json"), "utf8"), SIGN_IN);
  assert.equal(statSync(ledger).mode & 0o777, 0o600);
  assert.throws(() => borrowSignIn(source, join(root, "engine"), ledger), /already borrowed/);
  assert.equal(returnSignIn(ledger), true);
  assert.equal(readFileSync(join(source, "auth.json"), "utf8"), SIGN_IN);
  assert.equal(existsSync(join(target, "auth.json")), false);
  assert.equal(existsSync(ledger), false);
  assert.equal(returnSignIn(ledger), false);
});

test("the sign-in is not moved unless exactly one engine profile home exists", () => {
  const root = scratch();
  const source = signedIn(root);
  const ledger = join(root, "borrowed.json");
  assert.throws(() => borrowSignIn(source, join(root, "engine"), ledger), /exactly one engine profile home, found 0/);
  engineHome(root, "e".repeat(64));
  engineHome(root, "f".repeat(64));
  assert.throws(() => borrowSignIn(source, join(root, "engine"), ledger), /exactly one engine profile home, found 2/);
  assert.equal(readFileSync(join(source, "auth.json"), "utf8"), SIGN_IN);
  assert.equal(existsSync(ledger), false);
});

test("a return never overwrites a sign-in that reappeared at the source", () => {
  const root = scratch();
  const source = signedIn(root);
  const target = engineHome(root, "0".repeat(64));
  const ledger = join(root, "borrowed.json");
  borrowSignIn(source, join(root, "engine"), ledger);
  writeFileSync(join(source, "auth.json"), "newer", { mode: 0o600 });
  assert.throws(() => returnSignIn(ledger), /already exists/);
  assert.equal(readFileSync(join(source, "auth.json"), "utf8"), "newer");
  assert.equal(readFileSync(join(target, "auth.json"), "utf8"), SIGN_IN);
  assert.equal(existsSync(ledger), true);
});

test("a return keeps its record when the borrowed sign-in is gone", () => {
  const root = scratch();
  const source = signedIn(root);
  const target = engineHome(root, "1".repeat(64));
  const ledger = join(root, "borrowed.json");
  borrowSignIn(source, join(root, "engine"), ledger);
  writeFileSync(ledger, JSON.stringify({ source, target: join(root, "elsewhere") }), { mode: 0o600 });
  assert.throws(() => returnSignIn(ledger), /missing/);
  assert.equal(existsSync(ledger), true);
  assert.equal(readFileSync(join(target, "auth.json"), "utf8"), SIGN_IN);
});
