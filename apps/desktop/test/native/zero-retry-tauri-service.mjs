import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const bundle = readFileSync(new URL(import.meta.resolve("@wdio/tauri-service")));
if (
  bundle.length !== 215397 || createHash("sha256").update(bundle).digest("hex") !==
    "8db539e16e6e72dea4e87f635683de2d3462c015be8558772c9c537253ef9ee7"
) {
  throw new Error("Pinned Tauri service bundle changed; no-respawn compatibility is unqualified");
}

const { default: InstalledWorker, launcher: InstalledLauncher } =
  await import("@wdio/tauri-service");
const { SevereServiceError } = await import("webdriverio");
if (typeof InstalledWorker !== "function" || typeof InstalledLauncher !== "function" ||
  typeof SevereServiceError !== "function") {
  throw new Error("Pinned Tauri worker, launcher or fatal error export changed");
}
for (const name of ["onPrepare", "onWorkerStart", "onWorkerEnd", "onComplete",
  "restartEmbeddedServer"]) {
  if (typeof InstalledLauncher.prototype?.[name] !== "function") {
    throw new Error(`Pinned Tauri launcher method changed: ${name}`);
  }
}
for (const name of ["before", "beforeTest", "beforeCommand"]) {
  if (typeof InstalledWorker.prototype?.[name] !== "function") {
    throw new Error(`Pinned Tauri worker method changed: ${name}`);
  }
}

class NoRespawnLauncher extends InstalledLauncher {
  async restartEmbeddedServer() {
    throw new SevereServiceError(
      "Automatic embedded app restart is forbidden for project qualification",
    );
  }
}

export { NoRespawnLauncher as launcher };

export default class NoRespawnTauriService extends InstalledWorker {
  #launcher;

  constructor(options, capabilities, config) {
    super(options, capabilities);
    this.#launcher = new NoRespawnLauncher(options, capabilities, config);
    assert.equal(
      this.#launcher.restartEmbeddedServer,
      NoRespawnLauncher.prototype.restartEmbeddedServer,
      "retained launcher must dispatch to the fatal no-respawn guard",
    );
    for (const name of ["before", "beforeTest", "beforeCommand"]) {
      assert.equal(
        this[name], InstalledWorker.prototype[name], "installed worker hooks must stay inherited",
      );
    }
  }

  onPrepare(...args) {
    return this.#launcher.onPrepare(...args);
  }

  onWorkerStart(...args) {
    return this.#launcher.onWorkerStart(...args);
  }

  onWorkerEnd(...args) {
    return this.#launcher.onWorkerEnd(...args);
  }

  onComplete(...args) {
    return this.#launcher.onComplete(...args);
  }
}
