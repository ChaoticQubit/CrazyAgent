import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const state = await import("../src/state.ts");
const source = (name) => readFileSync(new URL(`../src/${name}`, import.meta.url), "utf8");
const settle = () => new Promise((done) => setTimeout(done, 10));

/** Puts a page with storage and the native bridge on the globals the app reads. Input: the
 * bridge's invoke, or null for a page without the native bridge. Output: what the page keeps
 * and the commands it sent. */
function page(invoke) {
  const kept = new Map();
  const shown = new Map();
  const asked = [];
  globalThis.document = { documentElement: { setAttribute: (name, value) => shown.set(name, value), removeAttribute: (name) => shown.delete(name) } };
  globalThis.window = {
    localStorage: { getItem: (key) => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value)), removeItem: (key) => kept.delete(key) },
    ...(invoke === null ? {} : { __TAURI_INTERNALS__: { invoke: (command, args) => { asked.push([command, args]); return invoke(command, args); } } }),
  };
  return { kept, shown, asked };
}

/** Runs a check with an unhandled rejection caught as a failure, then clears the globals.
 * Input: the check. Output: the rejections no code handled. */
async function guarded(check) {
  const unhandled = [];
  const listen = (reason) => unhandled.push(reason);
  process.on("unhandledRejection", listen);
  try {
    await check();
    await settle();
  } finally {
    process.off("unhandledRejection", listen);
    delete globalThis.document;
    delete globalThis.window;
  }
  return unhandled;
}

test("choosing a theme in Profile mirrors it to the native window and icon (DEC-181)", async () => {
  assert.equal(typeof state.chooseTheme, "function", "state.ts has chooseTheme");
  const unhandled = await guarded(async () => {
    const seen = page(async () => null);
    for (const [choice, attribute, stored] of [["dark", "dark", "dark"], ["light", "light", "light"], ["system", undefined, undefined]]) {
      state.chooseTheme(choice);
      await settle();
      assert.deepEqual(seen.asked.at(-1), ["desktop_set_theme", { choice }], choice);
      assert.equal(seen.shown.get("data-theme"), attribute, choice);
      assert.equal(seen.kept.get("crazyagent.theme"), stored, choice);
    }
    assert.equal(seen.asked.length, 3, "one mirror per choice");
  });
  assert.deepEqual(unhandled, []);
});

test("the mirror never throws or leaves a rejection when the native side is missing or refuses", async () => {
  assert.equal(typeof state.mirrorTheme, "function", "state.ts has mirrorTheme");
  for (const [what, invoke] of [
    ["no native bridge", null],
    ["a refusal, as a wall window gets", async () => { throw { code: "window_denied" }; }],
    ["a bridge that throws at once", () => { throw new Error("bridge gone"); }],
  ]) {
    const unhandled = await guarded(async () => {
      page(invoke);
      assert.doesNotThrow(() => state.mirrorTheme("dark"), what);
      assert.doesNotThrow(() => state.chooseTheme("light"), what);
    });
    assert.deepEqual(unhandled, [], what);
  }
});

test("only the main window mirrors: the wall boot skips it and Profile chooses through it", () => {
  const main = source("main.tsx");
  assert.match(main, /const wall = wallRoute\(window\.location\.hash\);\napplyTheme\(readTheme\(\)\);\nif \(wall === null\) mirrorTheme\(readTheme\(\)\);\n/);
  const profile = source("screens/settings/profile.tsx");
  assert.match(profile, /onValue=\{\(value\) => \{ setTheme\(value\); chooseTheme\(value\); \}\}/);
  assert.doesNotMatch(profile, /applyTheme/);
  const kept = source("state.ts");
  assert.match(kept, /^import \{ invoke \} from "@tauri-apps\/api\/core";$/m, "the mirror skips bridgeCall's retries and reply reports");
  assert.match(kept, /invoke<void>\("desktop_set_theme", \{ choice \}\)/);
});
