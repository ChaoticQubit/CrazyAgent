import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";

test("actual project profile requires a pinned service that forbids app replacement", async () => {
  const { config } = await import("./projects.wdio.conf.mjs");
  assert.equal(
    typeof config.services?.[0]?.[0],
    "function",
    "actual projects profile must register a custom no-respawn service constructor",
  );

  const { config: baseline } = await import("../../wdio.conf.mjs");
  assert.equal(config.services.length, 1);
  assert.deepEqual(config.services[0][1], baseline.services[0][1]);
  assert.deepEqual(config.capabilities, baseline.capabilities);
  assert.equal(config.maxInstances, 1);
  assert.equal(config.connectionRetryCount, 0);
  assert.equal(config.connectionRetryTimeout, 10000);
  assert.equal(config.specFileRetries, 0);
  assert.deepEqual(config.mochaOpts, { timeout: 80000, retries: 0 });
  assert.equal(config.services[0][1].driverProvider, "embedded");
  assert.equal(config.services[0][1].autoInstallTauriDriver, false);
  assert.equal(config.services[0][1].startTimeout, 20000);

  const bundle = readFileSync(new URL(import.meta.resolve("@wdio/tauri-service")));
  assert.equal(bundle.length, 215397);
  assert.equal(
    createHash("sha256").update(bundle).digest("hex"),
    "8db539e16e6e72dea4e87f635683de2d3462c015be8558772c9c537253ef9ee7",
    "installed Tauri service drift requires explicit compatibility qualification",
  );
  const { default: InstalledWorker, launcher: InstalledLauncher } =
    await import("@wdio/tauri-service");
  const { SevereServiceError } = await import("webdriverio");
  const { default: NoRespawnService, launcher: NoRespawnLauncher } =
    await import("./zero-retry-tauri-service.mjs");
  assert.equal(config.services[0][0], NoRespawnService);
  assert.equal(Object.getPrototypeOf(NoRespawnService.prototype), InstalledWorker.prototype);
  assert.equal(Object.getPrototypeOf(NoRespawnLauncher.prototype), InstalledLauncher.prototype);
  assert.equal(typeof InstalledLauncher.prototype.restartEmbeddedServer, "function");
  assert.equal(Object.hasOwn(NoRespawnLauncher.prototype, "restartEmbeddedServer"), true);
  assert.notEqual(
    NoRespawnLauncher.prototype.restartEmbeddedServer,
    InstalledLauncher.prototype.restartEmbeddedServer,
  );
  await assert.rejects(
    async () => NoRespawnLauncher.prototype.restartEmbeddedServer.call(undefined),
    (error) => {
      assert(error instanceof SevereServiceError, "restart must fail with the installed fatal type");
      assert.match(error.message, /respawn|restart|replacement/i);
      return true;
    },
  );
});
