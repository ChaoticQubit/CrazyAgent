import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { writeFileSync } from "node:fs";
import { config as baseline } from "../../wdio.conf.mjs";
import NoRespawnTauriService from "./zero-retry-tauri-service.mjs";

process.umask(0o077);
assert.equal(process.env.CRAZYAGENT_DESKTOP_TEST_SUITE, "native-auth");
assert.equal(baseline.maxInstances, 1);
assert.equal(baseline.connectionRetryCount, 0);
assert.equal(baseline.services.length, 1);
assert.equal(baseline.services[0][0], "@wdio/tauri-service");
assert.equal(baseline.services[0][1].driverProvider, "embedded");
assert.equal(baseline.services[0][1].autoInstallTauriDriver, false);

export const config = {
  ...baseline,
  services: [[NoRespawnTauriService, {
    ...baseline.services[0][1],
    captureBackendLogs: false,
    captureFrontendLogs: false,
    env: {},
  }]],
  specs: [fileURLToPath(new URL("native-auth.e2e.mjs", import.meta.url))],
  specFileRetries: 0,
  mochaOpts: { timeout: 100000, retries: 0 },
  logLevel: "silent",
  logLevels: { "*": "silent", "tauri-service": "silent" },
  beforeCommand: function () {},
  afterTest: function (_test, _context, result) {
    const proof = JSON.stringify({ passed: Boolean(result.passed), sign_in_qualified: false });
    assert(Buffer.byteLength(proof) <= 128, "bounded nonsecret auth test status required");
    writeFileSync(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, "native-auth-status.json"),
      proof, { flag: "wx", mode: 0o600 });
  },
};
