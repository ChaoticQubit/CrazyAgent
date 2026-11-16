import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { config as baseline } from "../../wdio.conf.mjs";
import { webkitHelpers } from "./speed-budget.mjs";
import NoRespawnTauriService from "./zero-retry-tauri-service.mjs";

process.umask(0o077);
const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
assert.equal(process.env.CRAZYAGENT_DESKTOP_TEST_SUITE, "speed", "the speed suite");
assert.ok(evidence && isAbsolute(evidence), "a private evidence folder");
assert.equal(baseline.maxInstances, 1);
assert.equal(baseline.services.length, 1);

/** The pinned no-respawn Tauri service, stamping the launch (row T1's start) and the WebKit
 * helpers already running (so row T6 can tell the app's own) right before it spawns the app. */
class SpeedTauriService extends NoRespawnTauriService {
  onPrepare(...args) {
    const launch = { at_ms: Date.now(), helpers: webkitHelpers() };
    writeFileSync(join(evidence, "speed-launch.json"), JSON.stringify(launch), { flag: "wx", mode: 0o600 });
    return super.onPrepare(...args);
  }
}

/** The speed suite runs one spec on one app launch. The baseline's `before` sizes the window
 * first; here the spec does it after timing the launch, so T1 never waits behind it. */
export const config = {
  ...baseline,
  services: [[SpeedTauriService, baseline.services[0][1]]],
  specs: [fileURLToPath(new URL("speed-native.e2e.mjs", import.meta.url))],
  specFileRetries: 0,
  mochaOpts: { timeout: 600000, retries: 0 },
  before: async function () {},
};
