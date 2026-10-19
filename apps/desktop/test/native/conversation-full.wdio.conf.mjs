import { config as baseline } from "../../wdio.conf.mjs";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { basename } from "node:path";
import { installRefresh } from "./refresh-barrier.mjs";

process.umask(0o077);

const run = basename(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE);
assert(/^[A-Za-z0-9_-]{1,64}$/.test(run), "bounded owned observation run required");
assert.equal(baseline.services.length, 1);
const [service, options] = baseline.services[0];
assert.equal(service, "@wdio/tauri-service");
assert.equal(options.driverProvider, "embedded");
assert.equal(options.autoInstallTauriDriver, false);
let sequence = 0;
let invalid = false;

function observation(browser, name, phase, error) {
  if (invalid) return;
  const session = browser.sessionId ?? "pre-session";
  if (!/^[A-Za-z0-9_$:.-]{1,64}$/.test(name) ||
      !/^[A-Za-z0-9_-]{1,128}$/.test(session) ||
      !["before", "after"].includes(phase) || sequence === 1024) {
    invalid = true;
    console.log(`native-refresh-command: ${JSON.stringify({ schema: 1, run, invalid: true })}`);
    return;
  }
  const record = JSON.stringify({ schema: 1, run, session, sequence: ++sequence,
    name, phase, wall_ms: Date.now(), elapsed_ns: process.hrtime.bigint().toString(),
    error: Boolean(error) });
  if (Buffer.byteLength(record) > 768) {
    invalid = true;
    console.log(`native-refresh-command: ${JSON.stringify({ schema: 1, run, invalid: true })}`);
    return;
  }
  console.log(`native-refresh-command: ${record}`);
}

export const config = {
  ...baseline,
  logLevels: { ...baseline.logLevels, "tauri-service": "info" },
  specs: [fileURLToPath(new URL("conversation-full.e2e.mjs", import.meta.url))],
  mochaOpts: { timeout: 150000, retries: 0 },
  services: [[service, { ...options, captureBackendLogs: true, backendLogLevel: "info",
    env: { ...options.env, CRAZYAGENT_NATIVE_REFRESH_OBSERVE: run } }]],
  before: async function (...args) {
    await baseline.before.apply(this, args);
    installRefresh(globalThis.browser, process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, run);
  },
  beforeCommand: function (name, args) {
    baseline.beforeCommand.call(this, name, args);
    observation(this, name, "before", false);
  },
  afterCommand: function (name, args, result, error) {
    observation(this, name, "after", error);
  },
};
