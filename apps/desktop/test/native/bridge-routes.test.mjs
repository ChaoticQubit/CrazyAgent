import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { LIB, ROUTES, bridgeSource, commands, methods, plan, render } from "./bridge-routes.mjs";

const lib = readFileSync(LIB, "utf8");
const impls = methods(bridgeSource());

const TWO_IMPLS = `impl NativeBridge {
    pub fn project_files(
        &self,
        id: &str,
        query: &str,
    ) -> Result<Unlocked<Vec<String>>, BridgeError> {
        listing(id, query)
    }
}

impl BridgeQueue {
    pub async fn project_files(&self, id: &str, query: &str) -> Result<Vec<String>, BridgeError> {
        self.read().await?.project_files(id, query)?.await
    }

    pub(super) async fn listing(&self) -> Result<(), BridgeError> {
        Ok(())
    }
}
`;

/** Builds the lib.rs `desktop_project_files` command with another body after its window check.
 * Input: the body's Rust text. Output: the command record `commands` reads from it. */
function filesCommand(body) {
  const [command] = commands(`#[tauri::command]
async fn desktop_project_files(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    query: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    ${body}
}
`);
  assert.equal(command.name, "desktop_project_files");
  return command;
}

test("the end-to-end bridge routes match the app's commands", () => {
  assert.equal(readFileSync(ROUTES, "utf8"), render(lib, impls), "routes.rs is stale: run node test/native/bridge-routes.mjs");
});

test("every plain bridge command is routed with its own arguments and native ones are left out", () => {
  const planned = new Map(commands(lib).map((command) => [command.name, plan(command, impls)]));
  const setup = planned.get("desktop_update_project_setup").route;
  assert.deepEqual(setup.args.map((arg) => arg.name), ["id", "setup", "name", "revision", "generation"]);
  assert.match(setup.body, /^lock\(state\)\s*\.await\?\s*\.update_project_setup\(&id, setup, name, revision, generation\)\s*\.await$/);
  assert.equal(planned.get("desktop_connect").route.body, "let mut bridge = lock(state).await?; bridge.connect().await");
  const catalog = planned.get("desktop_harness_catalog").route;
  assert.deepEqual(catalog.args.map((arg) => arg.name), ["check"]);
  assert.match(catalog.body, /^read\(state\)\s*\.await\?\s*\.harness_catalog\(check\.unwrap_or\(false\)\)\s*\.await$/, "plain reads share the bridge as the app's do");
  assert.match(planned.get("desktop_project_read_paths").route.body, /^read\(state\)\s*\.await\?\s*\.project_read_paths\(&id\)\s*\.await$/, "the read-folders read never takes the bridge alone");
  assert.equal(planned.get("desktop_project_files").route?.body, "state.project_files(&id, &query).await", "the @ listing goes through the bridge queue as the app's does");
  assert.throws(() => plan(filesCommand("read(&state).await?.project_files(&id, &query).await"), impls), /awaits NativeBridge::project_files, which is not async and gives a Result/, "the stale b19 route is refused against the real bridge");
  for (const native of ["desktop_pick_folder", "desktop_terminal_attach", "desktop_install_update", "desktop_engine_versions"]) {
    assert.ok(planned.get(native).skip, `${native} needs the native app`);
  }
  assert.ok([...planned.values()].filter((entry) => entry.route).length > 100, "the bridge serves the plain commands");
});

test("a route calls the method of the type its body names, by impl block, and refuses an ambiguous name", () => {
  const two = methods(TWO_IMPLS);
  assert.deepEqual(two.get("NativeBridge").get("project_files"), { async: false, pub: true, result: true });
  assert.deepEqual(two.get("BridgeQueue").get("project_files"), { async: true, pub: true, result: true });
  assert.deepEqual(two.get("BridgeQueue").get("listing"), { async: true, pub: false, result: true });
  const args = [{ name: "id", type: "String" }, { name: "query", type: "String" }];
  assert.deepEqual(plan(filesCommand("state.0.project_files(&id, &query).await"), two), {
    route: { name: "desktop_project_files", args, body: "state.project_files(&id, &query).await" },
  }, "the queue's async method, not the bridge's sync one of the same name");
  assert.deepEqual(plan(filesCommand("let listing = read(&state).await?.project_files(&id, &query)?; listing.await"), two), {
    route: { name: "desktop_project_files", args, body: "let listing = read(state).await?.project_files(&id, &query)?; listing.await" },
  }, "the bridge's sync method gives its listing before the await");
  assert.throws(() => plan(filesCommand("read(&state).await?.project_files(&id, &query).await"), two), /awaits NativeBridge::project_files, which is not async and gives a Result/);
  assert.throws(() => plan(filesCommand("let files = shared(&id)?; files.project_files(&id, &query).await"), two), /calls project_files, which both NativeBridge and BridgeQueue define, on a receiver/);
  assert.throws(() => plan(filesCommand("state.0.listing().await"), two), /calls BridgeQueue::listing, which no impl BridgeQueue defines as pub/);
  assert.throws(() => plan(filesCommand("lock(&state).await?.remove_everything().await"), two), /calls NativeBridge::remove_everything, which no impl NativeBridge defines as pub/);
});
