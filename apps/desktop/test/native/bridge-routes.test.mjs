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
  const catalog = planned.get("desktop_harness_catalog").route;
  assert.deepEqual(catalog.args.map((arg) => arg.name), ["check"]);
  assert.match(catalog.body, /^read\(state\)\s*\.await\?\s*\.harness_catalog\(check\.unwrap_or\(false\)\)\s*\.await$/, "plain reads share the bridge as the app's do");
  assert.match(planned.get("desktop_project_read_paths").route.body, /^read\(state\)\s*\.await\?\s*\.project_read_paths\(&id\)\s*\.await$/, "the read-folders read never takes the bridge alone");
  for (const native of ["desktop_pick_folder", "desktop_terminal_attach", "desktop_install_update", "desktop_engine_versions"]) {
    assert.ok(planned.get(native).skip, `${native} needs the native app`);
  }
  assert.ok([...planned.values()].filter((entry) => entry.route).length > 100, "the bridge serves the plain commands");
});

test("a pure bridge helper in a body is routed through the desktop crate and native bridge calls still skip", () => {
  const planned = new Map(commands(lib).map((command) => [command.name, plan(command)]));
  assert.match(planned.get("desktop_tracker_op").route?.body ?? "", /^let call = if bridge::tracker::writes\(&op\) \{ lock\(state\)\.await\?\.tracker_op\(op, args\)\? \} else \{ read\(state\)\.await\?\.tracker_op\(op, args\)\? \}; call\.await$/, "tracker writes take the bridge alone, reads share it");
  for (const native of ["desktop_pick_folder", "desktop_pick_documents", "desktop_import_instructions_file", "desktop_open_link", "desktop_system_notifications", "desktop_team_attention"]) {
    assert.equal(planned.get(native).skip, "native-only body", `${native} calls a native bridge helper`);
  }
  const routes = render(lib);
  assert.match(routes, /"desktop_tracker_op" => \{[\s\S]*?async \{ let call = if crazyagent_desktop::bridge::tracker::writes\(&op\) \{/, "the helper is named through the desktop crate");
  assert.doesNotMatch(routes, /(?<![\w:])bridge::/, "every bridge path in routes.rs names the desktop crate");
});
