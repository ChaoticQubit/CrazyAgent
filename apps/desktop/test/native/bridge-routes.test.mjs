import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";

const lib = readFileSync(LIB, "utf8");

test("the end-to-end bridge routes match the app's commands", () => {
  assert.equal(readFileSync(ROUTES, "utf8"), render(lib), "routes.rs is stale: run node test/native/bridge-routes.mjs");
});

test("every plain bridge command is routed with its own arguments and native ones are left out", () => {
  const planned = new Map(commands(lib).map((command) => [command.name, plan(command)]));
  const setup = planned.get("desktop_update_project_setup").route;
  assert.deepEqual(setup.args.map((arg) => arg.name), ["id", "setup", "name", "revision", "generation"]);
  assert.match(setup.body, /^lock\(state\)\s*\.await\?\s*\.update_project_setup\(&id, setup, name, revision, generation\)\s*\.await$/);
  assert.equal(planned.get("desktop_connect").route.body, "let mut bridge = lock(state).await?; bridge.connect().await");
  for (const native of ["desktop_pick_folder", "desktop_terminal_attach", "desktop_install_update", "desktop_engine_versions"]) {
    assert.ok(planned.get(native).skip, `${native} needs the native app`);
  }
  assert.ok([...planned.values()].filter((entry) => entry.route).length > 100, "the bridge serves the plain commands");
});
