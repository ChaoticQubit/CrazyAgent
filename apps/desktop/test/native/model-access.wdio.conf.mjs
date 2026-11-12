import { config as baseline } from "../../wdio.conf.mjs";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { statSync } from "node:fs";
import { join } from "node:path";
import NoRespawnTauriService from "./zero-retry-tauri-service.mjs";

process.umask(0o077);
assert.equal(baseline.maxInstances, 1);
assert.equal(baseline.connectionRetryCount, 0);
assert.equal(baseline.services.length, 1);
assert.equal(baseline.services[0][0], "@wdio/tauri-service");
assert.equal(baseline.services[0][1].driverProvider, "embedded");
assert.equal(baseline.services[0][1].autoInstallTauriDriver, false);

const SPECS = {
  "model-access": "model-access.e2e.mjs",
  "model-access-read-only": "model-access-read-only.e2e.mjs",
  "pools-screen": "pools-screen.e2e.mjs",
  "first-run-guide": "first-run-guide.e2e.mjs",
  "first-run-skip-guide": "first-run-skip-guide.e2e.mjs",
  "learning-replay-guide": "learning-replay-guide.e2e.mjs",
  "learning-replay-first-run": "learning-replay-first-run.e2e.mjs",
};
assert(SPECS[process.env.CRAZYAGENT_DESKTOP_TEST_SUITE], "a suite this configuration runs");

export const config = {
  ...baseline,
  services: [[NoRespawnTauriService, baseline.services[0][1]]],
  specs: [fileURLToPath(new URL(SPECS[process.env.CRAZYAGENT_DESKTOP_TEST_SUITE], import.meta.url))],
  specFileRetries: 0,
  mochaOpts: { timeout: 100000, retries: 0 },
  afterTest: async function (test, context, result) {
    await baseline.afterTest.call(this, test, context, result);
    if (!result.passed) {
      assert(statSync(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, "native-failure.png")).size <= 2 * 1024 * 1024,
        "failure screenshot byte bound");
    }
  },
};
