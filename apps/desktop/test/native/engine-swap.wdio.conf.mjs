import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, renameSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { SevereServiceError } from "webdriverio";
import { terminalServers } from "./e2e-harness.mjs";
import { chooseHome, clearWebStore, lockHome } from "./run-layout.mjs";

process.umask(0o077);
const app = process.env.WD_APP;
const engine = process.env.ES_ENGINE;
const runner = process.env.ES_RUNNER;
const asset = process.env.ES_OLD_APP;
const assetSha = process.env.ES_OLD_SHA256;
const root = process.env.ES_ROOT;
const evidence = process.env.ES_EVIDENCE;
const port = Number(process.env.WD_PORT);
for (const [name, path] of [["WD_APP", app], ["ES_ENGINE", engine], ["ES_RUNNER", runner], ["ES_OLD_APP", asset], ["ES_ROOT", root], ["ES_EVIDENCE", evidence]]) {
  assert(path && isAbsolute(path), `${name} must be an absolute path`);
}
assert(/^[0-9a-f]{64}$/.test(assetSha ?? ""), "ES_OLD_SHA256 must be the asset's sha256 from the release");
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");
assert(!process.env.CRAZYAGENT_DESKTOP_SOCKET, "the app must find its engine through the login service: unset CRAZYAGENT_DESKTOP_SOCKET");
for (const temporary of ["/tmp/", "/private/tmp/", "/private/var/folders/", "/var/folders/", `${tmpdir()}/`]) {
  assert(!`${root}/`.startsWith(temporary), "ES_ROOT must sit outside temporary folders, where a login service is never installed");
}

/** The scratch app bundle: the older release first, then this build's bytes in place. */
export const BUNDLE = join(root, "Applications", "CrazyAgent.app");
/** The bundle's engine, which the login service runs. */
export const SIDECAR = join(BUNDLE, "Contents", "MacOS", "crazyagent");
/** The engine data folder the app and both engines share, through CRAZYAGENT_HOME. */
export const DATA = join(root, "data");
/** What the spec reads about the older engine before the app starts. */
export const STAGED = join(evidence, "staged.json");
/** The HOME the engines, the app and its WebKit store share. */
export const home = chooseHome(join(root, "home")).home;
const OLD_READY_MS = 180000;
const LABEL = /^service: (dev\.crazyagent\.engine\.[0-9a-f]{12})$/m;

/** The environment every `crazyagent service` call and the app get: the scratch HOME and data
 * folder, never the owner's. Output: the environment. */
export function scratchEnv() {
  return { PATH: "/usr/bin:/bin:/usr/sbin:/sbin", HOME: home, CFFIXED_USER_HOME: home, CRAZYAGENT_HOME: DATA, LANG: "en_US.UTF-8" };
}

/** Runs the bundle's `crazyagent service <action>` on the scratch data folder. Inputs: the
 * action. Output: { status, stdout, stderr }. */
export function service(action) {
  const result = spawnSync(SIDECAR, ["service", action], { env: scratchEnv(), encoding: "utf8", timeout: 240000 });
  return { status: result.status, stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

/** The scratch login service's label and running pid. It refuses any label without the
 * scratch root's hash, so the owner's `dev.crazyagent.engine` is never touched. Output:
 * { label, pid }, pid null while nothing runs. */
export function scratchService() {
  const status = service("status");
  const label = status.stdout.match(LABEL);
  assert(label, `the scratch service has its own label: ${status.stdout} ${status.stderr}`);
  const pid = status.stdout.match(/^running: yes \(pid (\d+)\)$/m);
  return { label: label[1], pid: pid ? Number(pid[1]) : null };
}

/** The version the running engine published in its locator. Output: the version or null. */
export function lockedVersion() {
  try {
    return JSON.parse(readFileSync(join(DATA, "owner-current"), "utf8")).engine_version ?? null;
  } catch {
    return null;
  }
}

/** The version a `crazyagent` binary reports. Input: its path. Output: the version. */
export function versionOf(binary) {
  const printed = spawnSync(binary, ["--version"], { encoding: "utf8" }).stdout ?? "";
  const found = printed.match(/^crazyagent (\S+) \(/);
  assert(found, `${binary} --version names its version: ${printed}`);
  return found[1];
}

/** Puts one of this build's binaries over the bundle's file at a new inode, as the updater
 * replaces a bundle: a running process keeps the bytes it started with. Inputs: the source
 * and the bundle file's name. Output: none. */
function swapIn(source, name) {
  const target = join(BUNDLE, "Contents", "MacOS", name);
  const staged = `${target}.swap`;
  copyFileSync(source, staged);
  renameSync(staged, target);
}

/** Waits until the older engine runs under the scratch service and published its version.
 * Input: the older version. Output: the scratch service. */
async function oldEngineRunning(version) {
  const deadline = Date.now() + OLD_READY_MS;
  for (;;) {
    const running = scratchService();
    if (running.pid !== null && lockedVersion() === version) return running;
    if (Date.now() > deadline) throw new SevereServiceError(`the older engine never ran: ${JSON.stringify(running)} locator ${lockedVersion()}`);
    await new Promise((done) => setTimeout(done, 500));
  }
}

let homeLock = null;

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("engine-swap.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: join(BUNDLE, "Contents", "MacOS", "crazyagent-desktop") } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded", autoInstallTauriDriver: false, embeddedPort: port,
    appArgs: [], startTimeout: 60000, captureBackendLogs: true, backendLogLevel: "info",
    env: scratchEnv(),
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  outputDir: join(evidence, "webdriver"),
  connectionRetryCount: 0,
  connectionRetryTimeout: 180000,
  waitforTimeout: 15000,
  specFileRetries: 0,
  mochaOpts: { timeout: 900000, retries: 0 },
  onPrepare: async function () {
    mkdirSync(evidence, { recursive: true, mode: 0o700 });
    try {
      mkdirSync(root, { mode: 0o700 });
    } catch {
      throw new SevereServiceError(`ES_ROOT must name a new folder: ${root}`);
    }
    for (const folder of [home, DATA, join(root, "Applications")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
    try {
      homeLock = await lockHome(home, { label: `wdio ${evidence}` });
    } catch (failure) {
      throw new SevereServiceError(String(failure.message));
    }
    clearWebStore(home);
    const sha = createHash("sha256").update(readFileSync(asset)).digest("hex");
    if (sha !== assetSha) throw new SevereServiceError(`the older release asset is not the recorded one: ${sha}`);
    const unpacked = spawnSync("/usr/bin/tar", ["-xzf", asset, "-C", join(root, "Applications")], { encoding: "utf8" });
    if (unpacked.status !== 0 || !existsSync(SIDECAR)) throw new SevereServiceError(`the older bundle did not unpack: ${unpacked.stderr}`);
    const older = versionOf(SIDECAR);
    const newer = versionOf(engine);
    assert.notEqual(older, newer, "the release asset is another version than this build");
    const { label } = scratchService();
    const installed = service("install");
    const running = await oldEngineRunning(older);
    swapIn(engine, "crazyagent");
    swapIn(runner, "crazyagent-runner");
    swapIn(app, "crazyagent-desktop");
    assert.equal(versionOf(SIDECAR), newer, "the bundle now holds this build's engine");
    writeFileSync(STAGED, JSON.stringify({
      label, older, newer, oldPid: running.pid, install: installed.status,
      bundle: realpathSync(BUNDLE), terminals: terminalServers(DATA),
    }, null, 1));
  },
  before: async function () {
    await globalThis.browser.setWindowSize(1280, 900);
  },
  afterTest: async function (test, context, result) {
    if (!result.passed) await globalThis.browser.saveScreenshot(join(evidence, `failure-${test.title.replace(/[^A-Za-z0-9]+/g, "-").slice(0, 60)}.png`));
  },
  onComplete: async function () {
    if (existsSync(SIDECAR)) {
      scratchService();
      const removed = service("uninstall");
      writeFileSync(join(evidence, "uninstall.txt"), `${removed.status}\n${removed.stdout}\n${removed.stderr}`);
    }
    const server = join(DATA, "terminal", "tmux.sock");
    if (existsSync(server)) spawnSync("tmux", ["-S", server, "kill-server"], { env: { PATH: process.env.PATH }, stdio: "ignore" });
    homeLock?.release();
  },
};
