import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { SevereServiceError } from "webdriverio";
import { chooseHome, clearWebStore, lockHome } from "./run-layout.mjs";

process.umask(0o077);
const source = process.env.GU_BUNDLE;
const root = process.env.GU_ROOT;
const evidence = process.env.GU_EVIDENCE;
const port = Number(process.env.WD_PORT);
for (const [name, path] of [["GU_BUNDLE", source], ["GU_ROOT", root], ["GU_EVIDENCE", evidence]]) {
  assert(path && isAbsolute(path), `${name} must be an absolute path`);
}
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");
assert(!process.env.CRAZYAGENT_DESKTOP_SOCKET, "the app must run its default engine: unset CRAZYAGENT_DESKTOP_SOCKET");
for (const temporary of ["/tmp/", "/private/tmp/", "/private/var/folders/", "/var/folders/", `${tmpdir()}/`]) {
  assert(!`${root}/`.startsWith(temporary), "GU_ROOT must sit outside temporary folders, where a login service is never installed");
}
assert(source.endsWith(".app"), "GU_BUNDLE is this build's CrazyAgent.app, as the app's bundle step makes it");
for (const binary of ["crazyagent-desktop", "crazyagent"]) {
  assert(existsSync(join(source, "Contents", "MacOS", binary)), `GU_BUNDLE holds ${binary}, the app and the engine sidecar beside it`);
}

export const FIXTURE = fileURLToPath(new URL("../../../../crates/crazyagent/tests/fixtures/v242-engine.db", import.meta.url));
export const BUNDLE = join(root, "Applications", "CrazyAgent.app");
export const SIDECAR = join(BUNDLE, "Contents", "MacOS", "crazyagent");
export const DATA = join(root, "data");
export const STAGED = join(evidence, "staged.json");
export const LAUNCHED = join(evidence, "launched.json");
export const EVIDENCE = evidence;
export const home = chooseHome(join(root, "home")).home;
const LABEL = /^service: (dev\.crazyagent\.engine\.[0-9a-f]{12})$/m;
const OWNER_LABEL = "dev.crazyagent.engine";
const NO_FIXTURE = "no upgrade fixture yet; write it with SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit";

/** The environment every `crazyagent service` call and the app get: the scratch HOME and engine
 * root, never the owner's. Input: none.
 * Output: the environment. */
export function scratchEnv() {
  return { PATH: "/usr/bin:/bin:/usr/sbin:/sbin", HOME: home, CFFIXED_USER_HOME: home, CRAZYAGENT_HOME: DATA, LANG: "en_US.UTF-8" };
}

/** Runs the scratch bundle's `crazyagent service <action>` on the scratch engine root.
 * Input: the action.
 * Output: { status, stdout, stderr }. */
export function service(action) {
  const result = spawnSync(SIDECAR, ["service", action], { env: scratchEnv(), encoding: "utf8", timeout: 240000 });
  return { status: result.status, stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

/** The scratch login service's label and whether launchd runs it. It refuses any label without
 * the scratch root's hash, a definition outside the scratch HOME or another data folder, so the
 * owner's dev.crazyagent.engine is never touched. Input: none. Output: { label, running, pid },
 * pid null when launchd names none. */
export function scratchService() {
  const status = service("status");
  const label = status.stdout.match(LABEL);
  if (!label) throw new SevereServiceError(`the run refuses a login service that is not a scratch one: ${status.stdout} ${status.stderr}`);
  const definition = status.stdout.match(/^definition: (.+) \((?:not )?installed\)$/m);
  const folder = status.stdout.match(/^data folder: (.+)$/m);
  if (!definition?.[1].startsWith(`${home}/`) || folder?.[1] !== DATA) {
    throw new SevereServiceError(`the run refuses a login service outside its scratch HOME and root: ${status.stdout}`);
  }
  const pid = status.stdout.match(/^running: yes \(pid (\d+)\)$/m);
  return { label: label[1], running: /^running: yes\b/m.test(status.stdout), pid: pid ? Number(pid[1]) : null };
}

/** The pids of every engine serving the scratch engine root, started by launchd or detached,
 * matched on their `engine --home <DATA>` arguments. Input: none.
 * Output: the pids, empty while none runs. */
export function enginePids() {
  const pattern = `engine --home ${DATA.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}( |$)`;
  const found = spawnSync("/usr/bin/pgrep", ["-f", pattern], { encoding: "utf8", timeout: 10000 });
  assert.ok(found.status === 0 || found.status === 1, `pgrep failed: ${found.status} ${found.stderr}`);
  return (found.stdout ?? "").split("\n").filter((line) => line !== "").map(Number);
}

/** Whether launchd holds a job with this label in this user's GUI domain, read with
 * `launchctl print` only. Input: the label.
 * Output: { loaded, pid }, pid null while nothing runs. */
export function launchd(label) {
  const printed = spawnSync("/bin/launchctl", ["print", `gui/${process.getuid()}/${label}`], { encoding: "utf8", timeout: 10000 });
  if (printed.status !== 0) return { loaded: false, pid: null };
  const pid = (printed.stdout ?? "").match(/^\s*pid = (\d+)$/m);
  return { loaded: true, pid: pid ? Number(pid[1]) : null };
}

/** The owner's own login service as launchd reports it, never read through `crazyagent`.
 * Input: none.
 * Output: { loaded, pid }. */
export function ownerEngine() {
  return launchd(OWNER_LABEL);
}

/** The bytes of the regular files under a folder, links not followed.
 * Input: the folder.
 * Output: the sum. */
export function bytes(folder) {
  let total = 0;
  for (const name of readdirSync(folder)) {
    const path = join(folder, name);
    const found = lstatSync(path);
    if (found.isDirectory()) total += bytes(path);
    else if (found.isFile()) total += found.size;
  }
  return total;
}

let homeLock = null;

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("golden-upgrade-restore.e2e.mjs", import.meta.url))],
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
    if (!existsSync(FIXTURE)) throw new SevereServiceError(NO_FIXTURE);
    mkdirSync(evidence, { recursive: true, mode: 0o700 });
    try {
      mkdirSync(root, { mode: 0o700 });
    } catch {
      throw new SevereServiceError(`GU_ROOT must name a new folder: ${root}`);
    }
    for (const folder of [home, DATA, join(root, "Applications")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
    try {
      homeLock = await lockHome(home, { label: `wdio ${evidence}` });
    } catch (failure) {
      throw new SevereServiceError(String(failure.message));
    }
    clearWebStore(home);
    const copied = spawnSync("/usr/bin/ditto", [source, BUNDLE], { encoding: "utf8" });
    if (copied.status !== 0 || !existsSync(SIDECAR)) throw new SevereServiceError(`the bundle did not copy: ${copied.stderr}`);
    const { label, running, pid } = scratchService();
    if (running || pid !== null || launchd(label).loaded || enginePids().length > 0) {
      throw new SevereServiceError(`an engine already runs on the scratch root or label: ${label}`);
    }
    const db = join(DATA, "engine.db");
    copyFileSync(FIXTURE, db);
    chmodSync(db, 0o600);
    writeFileSync(STAGED, JSON.stringify({ label, owner: ownerEngine(), seeded: bytes(DATA) }, null, 1));
  },
  beforeSession: function () {
    writeFileSync(LAUNCHED, JSON.stringify({ at: Date.now() }));
  },
  afterTest: async function (test, context, result) {
    if (!result.passed) await globalThis.browser.saveScreenshot(join(evidence, `failure-${test.title.replace(/[^A-Za-z0-9]+/g, "-").slice(0, 60)}.png`));
  },
  onComplete: async function () {
    homeLock?.release();
    if (!existsSync(SIDECAR)) return;
    const { label } = scratchService();
    const removed = service("uninstall");
    writeFileSync(join(evidence, "uninstall.txt"), `${removed.status}\n${removed.stdout}\n${removed.stderr}`);
    if (launchd(label).loaded) spawnSync("/bin/launchctl", ["bootout", `gui/${process.getuid()}/${label}`], { stdio: "ignore", timeout: 30000 });
    for (const pid of enginePids()) {
      try {
        process.kill(pid, "SIGTERM");
      } catch (failure) {
        if (failure.code !== "ESRCH") throw failure;
      }
    }
    if (launchd(label).loaded) throw new SevereServiceError(`the scratch login service is still loaded: ${label}`);
    if (!existsSync(STAGED)) return;
    const before = JSON.parse(readFileSync(STAGED, "utf8")).owner;
    const after = ownerEngine();
    writeFileSync(join(evidence, "owner-engine.txt"), `${JSON.stringify(before)}\n${JSON.stringify(after)}\n`);
    if (JSON.stringify(before) !== JSON.stringify(after)) throw new SevereServiceError(`the owner's engine changed during the run: ${JSON.stringify(before)} then ${JSON.stringify(after)}`);
  },
};
