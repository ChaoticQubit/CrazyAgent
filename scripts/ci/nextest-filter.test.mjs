import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { filterset } from "./nextest-filter.mjs";

const SCRIPT = fileURLToPath(new URL("./nextest-filter.mjs", import.meta.url));

test("empty lists select every test", () => {
  assert.equal(filterset("", ""), "all()");
  assert.equal(filterset("  \n ", " "), "all()");
});

test("a target, a target row and a library row each leave out exactly what the Makefile forms name, per package", () => {
  assert.equal(
    filterset("agents_live harness_launch/live_tools_on_turn github::tests::signed_in", "accounts_bridge/reach_the_real_engine picker_windows"),
    "not (binary_id(=crazyagent::agents_live) | (binary_id(=crazyagent::harness_launch) & test(=live_tools_on_turn)) | (package(=crazyagent) & test(=github::tests::signed_in))"
      + " | (binary_id(=crazyagent-desktop::accounts_bridge) & test(=reach_the_real_engine)) | binary_id(=crazyagent-desktop::picker_windows))",
  );
});

test("an entry outside the three forms, or one listed twice, is refused instead of widening or skewing the filter", () => {
  for (const bad of ["a/b/c", "a/b::c", "a)|all(", "a b a", "a::", "/row", "a b/c a"]) {
    assert.throws(() => filterset(bad, ""), /exclusion/, bad);
  }
  assert.throws(() => filterset("", "x x"), /exclusion/);
});

test("the command line reads FARM_ENGINE_EXCLUDE and FARM_DESKTOP_EXCLUDE and prints one filterset", () => {
  const env = { PATH: process.env.PATH, FARM_ENGINE_EXCLUDE: "agents_live\n  accounts/real_ollama", FARM_DESKTOP_EXCLUDE: "picker_windows" };
  const out = execFileSync(process.execPath, [SCRIPT], { env, encoding: "utf8" });
  assert.equal(out, "not (binary_id(=crazyagent::agents_live) | (binary_id(=crazyagent::accounts) & test(=real_ollama)) | binary_id(=crazyagent-desktop::picker_windows))\n");
  assert.throws(() => execFileSync(process.execPath, [SCRIPT], { env: { PATH: process.env.PATH }, stdio: "pipe" }), /FARM_ENGINE_EXCLUDE/);
});
