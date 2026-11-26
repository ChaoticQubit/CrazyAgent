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

const SUPERSEDED = { failure: { code: "watch_superseded", message: "a newer page follows the engine watch" } };

/** Runs the harness page script on scripted bridge answers, with timers that fire on the next
 * turn and keep each wait asked, then starts its watch and waits for every answer to be taken
 * and a few more turns, so a loop that did not end asks once more and hangs on that ask.
 * Input: the answers, the last one ending the loop. Output: what the page asked, what its
 * channel got, the waits and its id. */
async function watchPage(answers) {
  const asked = [];
  const waits = [];
  let ended;
  const drained = new Promise((resolve) => (ended = resolve));
  const window = { addEventListener() {} };
  const fetch = async (_url, { body }) => {
    asked.push(JSON.parse(body));
    if (answers.length === 0) return new Promise(() => {});
    const answer = answers.shift();
    if (answers.length === 0) ended();
    return { json: async () => answer };
  };
  const setTimeout = (done, ms) => {
    waits.push(ms);
    setImmediate(done);
  };
  vm.runInNewContext(INTERNALS, { window, fetch, setTimeout });
  const got = [];
  const id = window.__TAURI_INTERNALS__.transformCallback((raw) => got.push(JSON.parse(JSON.stringify(raw))));
  const channel = { id, toJSON: () => `__CHANNEL__:${id}` };
  assert.equal(await window.__TAURI_INTERNALS__.invoke("desktop_watch", { changes: channel }), null);
  await drained;
  for (let turn = 0; turn < 20; turn += 1) await new Promise((next) => setImmediate(next));
  return { asked, got, waits, id };
}

test("v2.5 S3c: the harness page hands each e2e_watch_next change to the watch's channel in order, skips empty waits and stops once a newer page polls", async () => {
  const { asked, got, waits, id } = await watchPage([
    { value: null },
    { value: { topics: ["team"] } },
    { value: { topics: [] } },
    { value: { topics: ["ide", "unknown"] } },
    SUPERSEDED,
  ]);
  assert.deepEqual(got, [
    { index: 0, message: { topics: ["team"] } },
    { index: 1, message: { topics: ["ide", "unknown"] } },
  ]);
  assert.deepEqual(asked.map(({ cmd }) => cmd), ["desktop_watch", "e2e_watch_next", "e2e_watch_next", "e2e_watch_next", "e2e_watch_next"]);
  assert.deepEqual(asked[0].args, { changes: `__CHANNEL__:${id}` });
  assert.deepEqual(waits, []);
});

test("the harness page keeps following the engine watch after a dropped long-poll, as the app's feed does, asks the bridge to watch again and still stops once a newer page polls", async () => {
  const dropped = { dropped: "e2e_watch_next got no answer from the bridge: socket hang up" };
  const { asked, got, waits, id } = await watchPage([
    { value: null },
    dropped,
    { value: null },
    dropped,
    { value: null },
    dropped,
    { value: null },
    dropped,
    { value: null },
    { value: { topics: ["team", "unknown"] } },
    SUPERSEDED,
  ]);
  assert.deepEqual(got, [{ index: 0, message: { topics: ["team", "unknown"] } }]);
  assert.deepEqual(asked.map(({ cmd }) => cmd), [
    "desktop_watch",
    ...Array(4).fill(["e2e_watch_next", "desktop_watch"]).flat(),
    "e2e_watch_next",
    "e2e_watch_next",
  ]);
  for (const resent of asked.filter(({ cmd }) => cmd === "desktop_watch")) assert.deepEqual(resent.args, { changes: `__CHANNEL__:${id}` });
  assert.deepEqual(waits, [1000, 2000, 4000, 5000]);
});
