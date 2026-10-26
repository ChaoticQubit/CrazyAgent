import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { after } from "node:test";
import NoRespawnService, { launcher as NoRespawnLauncher } from "./zero-retry-tauri-service.mjs";
import { default as InstalledWorker, launcher as InstalledLauncher } from "@wdio/tauri-service";
import { initializeLauncherService, initializeWorkerService } from "@wdio/utils";
import { SevereServiceError } from "webdriverio";

const evidence = mkdtempSync(join(tmpdir(), "ca-zero-retry-unit-"));
process.env.CRAZYAGENT_DESKTOP_TEST_APP = process.execPath;
process.env.CRAZYAGENT_DESKTOP_TEST_PORT = "19259";
process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE = evidence;
after(() => rmSync(evidence, { recursive: true }));
const { config } = await import("./projects.wdio.conf.mjs");
const options = config.services[0][1];
const unchangedInputs = {
  ...config,
  specs: structuredClone(config.specs),
  reporters: structuredClone(config.reporters),
  logLevels: structuredClone(config.logLevels),
  mochaOpts: structuredClone(config.mochaOpts),
  capabilities: structuredClone(config.capabilities),
  services: [[config.services[0][0], structuredClone(options)]],
};

test("actual registered constructors preserve launcher and worker role inputs", async () => {
  assert.equal(config.services[0][0], NoRespawnService);
  assert.equal(config.services.length, 1);
  const { launcherServices, ignoredWorkerServices } =
    await initializeLauncherService(config, config.capabilities);
  assert.deepEqual(ignoredWorkerServices, []);
  assert.equal(launcherServices.length, 1);
  const workerServices =
    await initializeWorkerService(config, config.capabilities[0], ignoredWorkerServices);
  assert.equal(workerServices.length, 1);
  assert.notEqual(launcherServices[0], workerServices[0]);
  for (const service of [launcherServices[0], workerServices[0]]) {
    assert(service instanceof NoRespawnService);
    assert(service instanceof InstalledWorker);
    assert.equal(Object.getPrototypeOf(service), NoRespawnService.prototype);
    assert.equal(service.before, InstalledWorker.prototype.before);
    assert.equal(service.beforeTest, InstalledWorker.prototype.beforeTest);
    assert.equal(service.beforeCommand, InstalledWorker.prototype.beforeCommand);
    assert.equal(service.driverProvider, "embedded");
    assert.equal(service.windowLabel, "main");
    assert.equal(service.clearMocks, false);
    assert.equal(service.resetMocks, false);
    assert.equal(service.restoreMocks, false);
    assert.equal(service.mode, undefined);
    assert.equal(service.devServerUrl, undefined);
  }
  assert.deepEqual(config, unchangedInputs);
});

test("actual never-prepared adapter delegates cold empty cleanup only", async () => {
  const { launcherServices } = await initializeLauncherService(config, config.capabilities);
  assert.equal(launcherServices.length, 1);
  const service = launcherServices[0];
  assert(service instanceof NoRespawnService);
  const ended = service.onWorkerEnd("0-0", 0, config.specs, 0);
  assert(ended instanceof Promise);
  assert.equal(await ended, undefined);
  assert.deepEqual(config, unchangedInputs);
  const completed = service.onComplete(0, config, config.capabilities);
  assert(completed instanceof Promise);
  assert.equal(await completed, undefined);
  const repeated = service.onComplete(0, config, config.capabilities);
  assert(repeated instanceof Promise);
  assert.equal(await repeated, undefined);
  assert.deepEqual(config, unchangedInputs);
});

test("actual constructed launcher rejects replacement before changing its empty state", async () => {
  const launcher = new NoRespawnLauncher(options, config.capabilities, config);
  assert(launcher instanceof InstalledLauncher);
  assert.equal(Object.getPrototypeOf(launcher), NoRespawnLauncher.prototype);
  assert.equal(launcher.options, options);
  assert.equal(launcher.restartEmbeddedServer, NoRespawnLauncher.prototype.restartEmbeddedServer);
  assert.equal(launcher.perWorkerMode, false);
  assert.equal(launcher.isEmbeddedMode, false);
  assert.equal(launcher.browserMode, false);
  assert.equal(launcher.testRunnerBackend, undefined);
  assert.equal(launcher.appBinaryPath, undefined);
  assert.equal(launcher.cnCycleConfigs, undefined);
  const maps = [launcher.instanceOptions, launcher.embeddedProcesses,
    launcher.embeddedConfigs, launcher.workerBackends];
  for (const map of maps) {
    assert(map instanceof Map);
    assert.equal(map.size, 0);
  }
  assert.deepEqual(launcher.portManager.getUsedPorts(), []);
  assert.deepEqual(launcher.backendPortManager.getUsedPorts(), []);
  assert.deepEqual(launcher.driverPool.getStatus(), { running: false, count: 0, identifiers: [] });
  assert.deepEqual(launcher.driverPool.getRunningPids(), []);
  await assert.rejects(
    launcher.restartEmbeddedServer("0", {
      appBinaryPath: config.capabilities[0]["tauri:options"].application,
      port: options.embeddedPort,
      options,
    }),
    (error) => {
      assert(error instanceof SevereServiceError);
      assert.equal(error.message,
        "Automatic embedded app restart is forbidden for project qualification");
      return true;
    },
  );
  for (const map of maps) {
    assert.equal(map.size, 0);
  }
  assert.deepEqual(launcher.portManager.getUsedPorts(), []);
  assert.deepEqual(launcher.backendPortManager.getUsedPorts(), []);
  assert.deepEqual(launcher.driverPool.getStatus(), { running: false, count: 0, identifiers: [] });
  assert.deepEqual(launcher.driverPool.getRunningPids(), []);
  const completed = launcher.onComplete(0, config, config.capabilities);
  assert(completed instanceof Promise);
  assert.equal(await completed, undefined);
  assert.deepEqual(config, unchangedInputs);
});
