import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import vm from "node:vm";
import { INTERNALS } from "./e2e-harness.mjs";

const DESKTOP = fileURLToPath(new URL("../../", import.meta.url));

/** Every file under a folder of the desktop app. Input: the folder, relative to the app.
 * Output: the files' paths. */
function files(folder) {
  return readdirSync(join(DESKTOP, folder), { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => join(entry.parentPath, entry.name));
}

test("v2.5 S3c: e2e_watch_next lives in the e2e bridge only and never builds into the app", () => {
  const cargo = readFileSync(join(DESKTOP, "src-tauri/Cargo.toml"), "utf8");
  assert.match(cargo, /\[\[bin\]\]\nname = "desktop-e2e-bridge"\npath = "e2e_bridge\/main\.rs"\nrequired-features = \["e2e-bridge"\]\n/);
  assert.match(cargo, /\ndefault = \["custom-protocol"\]\n/, "a default build leaves the e2e bridge out");
  const app = [...files("src-tauri/src"), ...files("src-tauri/capabilities"), join(DESKTOP, "src-tauri/build.rs"), ...files("src")];
  assert.ok(app.length > 100, `the app's sources are read: ${app.length}`);
  for (const path of app) {
    const text = readFileSync(path, "utf8");
    assert.doesNotMatch(text, /e2e_watch_next/, path);
    assert.doesNotMatch(text, /e2e_bridge/, path);
  }
  assert.match(readFileSync(join(DESKTOP, "src-tauri/e2e_bridge/watch.rs"), "utf8"), /pub const WATCH_NEXT: &str = "e2e_watch_next";/);
  assert.match(readFileSync(join(DESKTOP, "src-tauri/e2e_bridge/main.rs"), "utf8"), /^mod watch;$/m);
});

test("v2.5 S3c: the harness page hands each e2e_watch_next change to the watch's channel in order, skips empty waits and stops with the bridge", async () => {
  const answers = [
    { value: null },
    { value: { topics: ["team"] } },
    { value: { topics: [] } },
    { value: { topics: ["ide", "unknown"] } },
    { dropped: "the bridge closed" },
  ];
  const asked = [];
  let ended;
  const drained = new Promise((resolve) => (ended = resolve));
  const window = { addEventListener() {} };
  const fetch = async (_url, { body }) => {
    asked.push(JSON.parse(body));
    const answer = answers.shift();
    if (answers.length === 0) ended();
    return { json: async () => answer };
  };
  vm.runInNewContext(INTERNALS, { window, fetch });
  const got = [];
  const id = window.__TAURI_INTERNALS__.transformCallback((raw) => got.push(JSON.parse(JSON.stringify(raw))));
  const channel = { id, toJSON: () => `__CHANNEL__:${id}` };
  assert.equal(await window.__TAURI_INTERNALS__.invoke("desktop_watch", { changes: channel }), null);
  await drained;
  assert.deepEqual(got, [
    { index: 0, message: { topics: ["team"] } },
    { index: 1, message: { topics: ["ide", "unknown"] } },
  ]);
  assert.deepEqual(asked.map(({ cmd }) => cmd), ["desktop_watch", "e2e_watch_next", "e2e_watch_next", "e2e_watch_next", "e2e_watch_next"]);
  assert.deepEqual(asked[0].args, { changes: `__CHANNEL__:${id}` });
});
