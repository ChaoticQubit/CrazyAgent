import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const settings = await import("../src/screens/project/settings.tsx");
const api = await import("../src/api.ts");

test("the sandbox section opens from its route and adds only absolute, new folders up to the limit", () => {
  assert.equal(settings.settingsSection("sandbox"), "sandbox");
  assert.deepEqual(settings.addReadPath([], "  /Users/me/shared  "), { paths: ["/Users/me/shared"] });
  assert.deepEqual(settings.addReadPath([], "~/shared"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath([], "relative/folder"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath(["/a"], "/a"), { error: "That folder is already in the list." });
  assert.deepEqual(settings.addReadPath([], "/bad\npath"), { error: "Each folder must be one plain path of up to 1024 characters." });
  const full = Array.from({ length: settings.READ_PATHS_MAX }, (_, index) => `/f${index}`);
  assert.deepEqual(settings.addReadPath(full, "/one-more"), { error: "A project can share at most 8 extra folders." });
});

test("every engine refusal for a read-only folder has its own sentence", () => {
  for (const code of ["read_path_not_absolute", "read_path_missing", "read_path_not_folder", "read_path_holds_home", "read_path_credentials",
    "read_path_engine_data", "read_path_refused", "read_path_owner_unknown", "read_paths_too_many", "read_paths_invalid"]) {
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/extra_paths.rs", import.meta.url), "utf8");
  for (const code of engine.matchAll(/"(read_paths?_[a-z_]+)"/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
});
