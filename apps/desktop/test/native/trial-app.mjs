import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";

export const RELAUNCHED = "relaunched.json";
/** The variable naming the folder a native-webdriver build of the app takes its next folder
 * pick from (apps/desktop/src-tauri/src/bridge/trial_pick.rs). The trial conf sets it on the
 * runner and on the app it launches; the kit's chooseFolder writes the answers there. */
export const PICKS_VARIABLE = "CRAZYAGENT_TRIAL_PICKS";
export const WINDOW = { width: 1440, height: 960 };
const TOOL_MS = 10000;
const QUIT_MS = 15000;
const POLL_MS = 250;

/** The .app bundle that open -a starts as the Dock does. Input: the trial's app, the bundle or
 * its Contents/MacOS binary. Output: the bundle path; throws for a bare binary. */
export function appBundle(app) {
  assert(typeof app === "string" && isAbsolute(app), `the app is an absolute path, got ${app}`);
  const at = app.lastIndexOf(".app/");
  const bundle = app.endsWith(".app") ? app : at >= 0 ? app.slice(0, at + 4) : null;
  assert(bundle, `relaunch needs the app as a .app bundle, which open -a starts as the Dock does; got ${app}`);
  return bundle;
}

/** The process that listens on a local TCP port, the app's WebDriver server in a trial.
 * Input: the port. Output: its pid, or null when nothing listens; throws when two do. */
export function listener(port) {
  assert(Number.isInteger(port) && port > 0 && port < 65536, `a TCP port, got ${port}`);
  const result = spawnSync("/usr/sbin/lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN", "-t"], { encoding: "utf8", timeout: TOOL_MS, killSignal: "SIGKILL" });
  const pids = [...new Set(result.stdout.split("\n").filter(Boolean).map(Number))];
  assert(pids.length <= 1, `more than one process listens on port ${port}: ${pids.join(", ")}`);
  return pids[0] ?? null;
}

/** The program a process runs, as ps shows it. Input: the pid. Output: the program, or null
 * when no such process runs. */
export function command(pid) {
  assert(Number.isInteger(pid) && pid > 1, `a process id, got ${pid}`);
  const result = spawnSync("/bin/ps", ["-o", "comm=", "-p", String(pid)], { encoding: "utf8", timeout: TOOL_MS, killSignal: "SIGKILL" });
  return result.status === 0 ? result.stdout.trim() || null : null;
}

/** Waits until a process no longer runs a program. Inputs: the pid, the program and the time
 * limit in ms. Output: whether it ended in time. */
export async function ended(pid, binary, timeout) {
  for (const deadline = Date.now() + timeout; Date.now() < deadline; await pause(POLL_MS)) {
    if (command(pid) !== binary) return true;
  }
  return command(pid) !== binary;
}

/** Turns off @wdio/tauri-service's focus recovery for the session by switching to the app's own
 * window once, as an explicit switchToWindow does. Without it the service asks the page for
 * window states before every find and click through the @wdio/tauri-plugin guest API, which this
 * build does not ship, so each of those commands waited 5 s ("Tauri core.invoke not available
 * after 5s timeout"). The app has one window. Input: the browser. A failed switch is printed
 * and the run goes on at the slow pace. */
export async function keepWindow(browser) {
  try {
    await browser.switchToWindow(await browser.getWindowHandle());
  } catch (failure) {
    console.warn(`trial: focus recovery stays on, so finds and clicks wait 5 s each: ${failure?.message ?? failure}`);
  }
}

/** Readies a new WebDriver session on the app: the trial's window size, then keepWindow.
 * Input: the browser. */
export async function attach(browser) {
  await browser.setWindowSize(WINDOW.width, WINDOW.height);
  await keepWindow(browser);
}

/** Quits the app relaunch() opened, which no WebDriver service owns and the runner's process
 * group does not hold, so its WebDriver server never outlives the run. Reads relaunched.json in
 * the run folder and stops that pid only while it still runs the recorded program: SIGTERM,
 * then SIGKILL after 15 s. Input: the run folder. Output: what it did, or null when nothing
 * was relaunched. */
export async function stopRelaunched(evidence) {
  const file = join(evidence, RELAUNCHED);
  if (!existsSync(file)) return null;
  const { pid, binary } = JSON.parse(readFileSync(file, "utf8"));
  assert(Number.isInteger(pid) && typeof binary === "string" && binary.length > 0, `${RELAUNCHED} names a pid and its program`);
  if (command(pid) !== binary) return `the relaunched app ${pid} already ended`;
  process.kill(pid, "SIGTERM");
  if (await ended(pid, binary, QUIT_MS)) return `stopped the relaunched app ${pid}`;
  process.kill(pid, "SIGKILL");
  return `killed the relaunched app ${pid}: it outlived SIGTERM by 15 s`;
}
