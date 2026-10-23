import assert from "node:assert/strict";
import { basename, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = process.env.CRAZYAGENT_DESKTOP_TEST_APP;
const port = Number(process.env.CRAZYAGENT_DESKTOP_TEST_PORT);
const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
assert(app && isAbsolute(app), "built native test application required");
assert(evidence && isAbsolute(evidence), "private native evidence directory required");
assert(Number.isInteger(port) && port > 1024 && port < 65536, "owned test port required");
const run = basename(evidence);
assert(/^[A-Za-z0-9_-]{1,64}$/.test(run), "bounded owned observation run required");

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("test/native/baseline.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: app } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded",
    autoInstallTauriDriver: false,
    embeddedPort: port,
    appArgs: [],
    startTimeout: 20000,
    captureBackendLogs: true,
    backendLogLevel: "info",
    env: { CRAZYAGENT_NATIVE_REFRESH_OBSERVE: run },
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  logLevels: { "tauri-service": "info" },
  outputDir: join(evidence, "webdriver"),
  connectionRetryCount: 0,
  connectionRetryTimeout: 10000,
  waitforTimeout: 8000,
  mochaOpts: { timeout: 90000, retries: 0 },
  beforeCommand: function (name) {
    console.log(`native-command: ${new Date().toISOString()} ${name}`);
  },
  before: async function () {
    await globalThis.browser.setWindowSize(1180, 800);
  },
  afterTest: async function (test, context, result) {
    if (!result.passed) {
      await globalThis.browser.saveScreenshot(join(evidence, "native-failure.png"));
    }
  },
};
