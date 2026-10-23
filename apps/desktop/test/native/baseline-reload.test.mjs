import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

process.env.CRAZYAGENT_DESKTOP_TEST_APP = process.execPath;
process.env.CRAZYAGENT_DESKTOP_TEST_PORT = "19323";
process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE = join(tmpdir(), "ca-reload-323");

test("actual baseline config admits the native document observations required by refresh", async () => {
  const { config } = await import("../../wdio.conf.mjs");
  assert.equal(config.services.length, 1);
  const [service, options] = config.services[0];
  assert.equal(service, "@wdio/tauri-service");
  assert.equal(options.driverProvider, "embedded");
  assert.equal(options.autoInstallTauriDriver, false);
  assert.equal(options.env?.CRAZYAGENT_NATIVE_REFRESH_OBSERVE, "ca-reload-323",
    "baseline must bind native Started/Finished observations to its owned run");
  assert.equal(options.captureBackendLogs, true);
  assert.equal(options.backendLogLevel, "info");
  const { default: logger } = await import("../../node_modules/@wdio/logger/build/index.js");
  logger.setLogLevelsConfig(config.logLevels, config.logLevel);
  assert.equal(logger("tauri-service:service").getLevel(), 2,
    "actual native backend records must survive the service logger filter");
  assert.equal(config.mochaOpts.timeout, 90000);
  assert.equal(config.mochaOpts.retries, 0);
  assert.equal(config.connectionRetryCount, 0);
});

test("baseline reload awaits the qualified document boundary before its original selection", () => {
  const path = new URL("./baseline.e2e.mjs", import.meta.url);
  const source = readFileSync(path, "utf8");
  assert(Buffer.byteLength(source) <= 16384, "bounded baseline spec required");
  const flow = source.match(/async function inspectAfterRendererReload\(\) \{([\s\S]+?)\n\}/)?.[1];
  assert(flow, "original renderer reload flow must remain");
  const refresh = flow.search(/^\s*await browser\.refresh\(\);/m);
  assert(refresh >= 0, "baseline must await the supported refresh command instead of racing its outgoing document");
  const install = flow.search(/^\s*installRefresh\(browser, evidence, run, 8000\);/m);
  assert(install >= 0 && install < refresh, "qualified native/document barrier must precede refresh");
  const selection = flow.search(/^\s*await clickActualButton\(/m);
  assert(selection > refresh, "the original baseline selection must follow completed document replacement");
  assert.equal(flow.includes("window.location.reload"), false);
});
