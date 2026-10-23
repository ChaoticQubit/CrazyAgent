import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("actual conversation config installs a rejecting supported refresh overwrite", async () => {
  const evidence = mkdtempSync(join(tmpdir(), "ca-refresh-unit-"));
  process.env.CRAZYAGENT_DESKTOP_TEST_APP = process.execPath;
  process.env.CRAZYAGENT_DESKTOP_TEST_PORT = "19258";
  process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE = evidence;
  const { config } = await import("./conversation-full.wdio.conf.mjs");
  let overwrite;
  let sized = false;
  const browser = {
    setWindowSize: async (width, height) => { sized = width === 1180 && height === 800; },
    overwriteCommand: (name, command) => {
      assert.equal(name, "refresh");
      assert.equal(overwrite, undefined);
      overwrite = command;
    },
  };
  globalThis.browser = browser;
  try {
    await config.before.call(browser);
    assert.equal(sized, true, "original native window setup must remain");
    assert.equal(typeof overwrite, "function", "actual refresh overwrite must be installed");
  } finally {
    delete globalThis.browser;
    rmSync(evidence, { recursive: true });
  }
});

test("actual service logger admits native info observations under the conversation config", async () => {
  const { config } = await import("./conversation-full.wdio.conf.mjs");
  const { default: logger } = await import("../../node_modules/@wdio/logger/build/index.js");
  logger.setLogLevelsConfig(config.logLevels, config.logLevel);
  assert.equal(logger("tauri-service:service").getLevel(), 2,
    "native info records must survive the WDIO service logger filter");
});
