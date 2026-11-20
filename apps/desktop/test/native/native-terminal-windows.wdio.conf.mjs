import assert from "node:assert/strict";
import { isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";

const app = process.env.WD_APP;
const port = Number(process.env.WD_PORT);
assert(app && isAbsolute(app), "WD_APP must be the app's absolute path");
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");

/** The Windows native terminal suite's wdio run: the real app with the real frontend, driven
 * through its embedded WebDriver server with the options the farm probe proved (NOTE-0545).
 * No engine, stand-in harness or AppKit placement, which the macOS rig needs. Inputs: WD_APP,
 * the app's path, and WD_PORT, a free local port. Output: the wdio config. */
export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("native-terminal-windows.e2e.mjs", import.meta.url))],
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
  waitforTimeout: 30000,
  specFileRetries: 0,
  mochaOpts: { timeout: 180000, retries: 0 },
};
