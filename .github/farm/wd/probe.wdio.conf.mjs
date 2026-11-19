import { fileURLToPath } from "node:url";

const app = process.env.WD_APP;
const port = Number(process.env.WD_PORT);

/** The farm probe's wdio run: one spec against the real Windows app through the embedded
 * WebDriver server, with the options the macOS native suites pass. Inputs: WD_APP, the
 * app's path, and WD_PORT, a free local port. Output: the wdio config. */
export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("probe.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: app } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded", autoInstallTauriDriver: false, embeddedPort: port,
    appArgs: [], startTimeout: 60000, captureBackendLogs: true, backendLogLevel: "info",
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "info",
  connectionRetryCount: 0,
  connectionRetryTimeout: 180000,
  waitforTimeout: 15000,
  specFileRetries: 0,
  mochaOpts: { timeout: 180000, retries: 0 },
};
