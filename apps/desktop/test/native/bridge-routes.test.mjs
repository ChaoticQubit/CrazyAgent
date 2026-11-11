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
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    query: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&webview)?;
    ${body}
}
`);
  assert.equal(command.name, "desktop_project_files");
  return command;
}

const LIMITS_HEAD = "impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for Limits";
const LIMITS_TAKE = `let tauri::ipc::CommandItem {
            plugin,
            name,
            message,
            acl,
            ..
        } = command;`;
const LIMITS_READ = `|key| {
            <Option<u32> as tauri::ipc::CommandArg<'de, R>>::from_command(tauri::ipc::CommandItem {
                plugin,
                name,
                key,
                message,
                acl,
            })
        }`;
const LIMITS_READS = '            turn_secs: read("turnSecs")?,\n            idle_secs: read("idleSecs")?,';

/** Builds a lib.rs with one argument type it reads through its own `CommandArg` and a command
 * taking it, in the canonical Tauri form unless a part is given. Input: the impl head, the
 * item destructuring, the read closure, lines before the answer and the field lines. Output:
 * the source text. */
function limitsSource({ head = LIMITS_HEAD, take = LIMITS_TAKE, read = LIMITS_READ, before = "", reads = LIMITS_READS } = {}) {
  return `struct Limits {
    turn_secs: Option<u32>,
    idle_secs: Option<u32>,
}

${head} {
    /// Reads each limit from its own key.
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        ${take}
        let read = ${read};
${before}        Ok(Self {
${reads}
        })
    }
}

#[tauri::command]
async fn desktop_team_time_limits(
    webview: Webview,
    state: State<'_, NativeState>,
    limits: Limits,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    let Limits { turn_secs, idle_secs } = limits;
    lock(&state).await?.team_time_limits(turn_secs, idle_secs).await
}
`;
}

test("an argument the app reads through its own CommandArg is routed by the keys it reads", () => {
  const route = plan(commands(lib).find((command) => command.name === "desktop_team_time_limits"), impls).route;
  assert.deepEqual(route.args.find((arg) => arg.name === "limits").keys, [
    { field: "turn_secs", key: "turnSecs", type: "Option<u32>" },
    { field: "session_secs", key: "sessionSecs", type: "Option<u32>" },
    { field: "idle_secs", key: "idleSecs", type: "Option<u32>" },
  ]);
  const arm = render(limitsSource(), impls);
  assert.match(arm, /struct Limits \{\n\s+#\[serde\(rename = "turnSecs"\)\]\n\s+turn_secs: Option<u32>,\n\s+#\[serde\(rename = "idleSecs"\)\]\n\s+idle_secs: Option<u32>,\n\s+\}/);
  assert.match(arm, /#\[serde\(flatten\)\]\n\s+limits: Limits,/, "the route reads the keys at the top level, as the app does");
  assert.throws(() => commands(limitsSource({ reads: '            turn_secs: read("turnSecs")?,\n            idle_secs: read_idle()?,' })), /routes: Limits reads its fields in a way the generator cannot follow/);
  assert.throws(() => commands(limitsSource({ reads: '            turn_secs: read("turnSecs")?,\n            idle_secs: read("turnSecs")?,' })), /routes: Limits reads its fields in a way the generator cannot follow/);
});

test("only the canonical Tauri CommandArg body is followed, whatever its layout; any other shape is refused", () => {
  const keys = (source) => commands(source).find((command) => command.name === "desktop_team_time_limits").params.find((param) => param.name === "limits").keys?.map(({ key }) => key);
  assert.deepEqual(keys(limitsSource()), ["turnSecs", "idleSecs"]);
  assert.deepEqual(keys(limitsSource({ take: "let tauri::ipc::CommandItem { plugin, name, message, acl, .. } = command;" })), ["turnSecs", "idleSecs"], "the same tokens on one line");
  const shapes = {
    "a read closure that defaults a missing key": { read: "|key| { <Option<u32> as tauri::ipc::CommandArg<'de, R>>::from_command(tauri::ipc::CommandItem { plugin, name, key, message, acl }).map(|v| v.or(Some(60))) }" },
    "a read closure that reads one fixed key": { read: "|key| { let _ = key; <Option<u32> as tauri::ipc::CommandArg<'de, R>>::from_command(tauri::ipc::CommandItem { plugin, name, key: \"limits\", message, acl }) }" },
    "an impl head that names CommandArg through a use": { head: "impl<'de, R: tauri::Runtime> CommandArg<'de, R> for Limits" },
    "a body that validates before it answers": { before: "        if plugin.is_some() {\n            return Err(tauri::ipc::InvokeError::from(\"plugin\"));\n        }\n" },
    "a field read with a transform": { reads: '            turn_secs: read("turnSecs")?.map(|s| s * 60),\n            idle_secs: read("idleSecs")?,' },
  };
  for (const [shape, parts] of Object.entries(shapes)) {
    assert.throws(() => commands(limitsSource(parts)), /routes: Limits reads its fields in a way the generator cannot follow/, shape);
  }
});

test("the end-to-end bridge routes match the app's commands", () => {
  assert.equal(readFileSync(ROUTES, "utf8"), render(lib, impls), "routes.rs is stale: run node test/native/bridge-routes.mjs");
});

test("every plain bridge command is routed with its own arguments and native ones are left out", () => {
  const planned = new Map(commands(lib).map((command) => [command.name, plan(command, impls)]));
  const setup = planned.get("desktop_update_project_setup").route;
  assert.deepEqual(setup.args.map((arg) => arg.name), ["id", "setup", "name", "revision", "generation", "kept"]);
  assert.match(setup.body, /^lock\(state\)\s*\.await\?\s*\.update_project_setup\(&id, setup, name, revision, kept, generation\)\s*\.await$/);
  assert.equal(planned.get("desktop_connect").route.body, "let mut bridge = lock(state).await?; bridge.connect().await");
  const catalog = planned.get("desktop_harness_catalog").route;
  assert.deepEqual(catalog.args.map((arg) => arg.name), ["check"]);
  assert.match(catalog.body, /^read\(state\)\s*\.await\?\s*\.harness_catalog\(check\.unwrap_or\(false\)\)\s*\.await$/, "plain reads share the bridge as the app's do");
  assert.match(planned.get("desktop_project_read_paths").route.body, /^read\(state\)\s*\.await\?\s*\.project_read_paths\(&id\)\s*\.await$/, "the read-folders read never takes the bridge alone");
  assert.equal(planned.get("desktop_project_files").route?.body, "state.project_files(&id, &query).await", "the @ listing goes through the bridge queue as the app's does");
  assert.throws(() => plan(filesCommand("read(&state).await?.project_files(&id, &query).await"), impls), /awaits NativeBridge::project_files, which is not async and gives a Result/, "the stale b19 route is refused against the real bridge");
  for (const native of ["desktop_pick_folder", "desktop_terminal_attach", "desktop_install_update", "desktop_engine_versions", "desktop_submit_claude_auth_code"]) {
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
