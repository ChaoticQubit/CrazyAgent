import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";

const SIGNALS = ["SIGTERM", "SIGINT", "SIGHUP"];
const KILL_WAIT_MS = 5000;
const REMOVE = { recursive: true, force: true, maxRetries: 20, retryDelay: 100 };
const META = 4;

/** Why a DevTools call never goes out on this system, or null when it may. A key event with Cmd
 * (Meta) held is refused on macOS, whatever the page does with it: Chrome there hands a key the
 * page left uncancelled to the Mac menu bar, and test keys opened About This Mac on the owner's
 * screen (ABOUT-THIS-MAC). Inputs: the method, its params and the platform. Output: the reason. */
export function refusedCall(method, params, platform = process.platform) {
  if (platform !== "darwin" || method !== "Input.dispatchKeyEvent" || ((params?.modifiers ?? 0) & META) === 0) return null;
  return "a DevTools key event with Cmd held never goes out on macOS (ABOUT-THIS-MAC): dispatch a KeyboardEvent in the page, or run the row on Linux";
}

/** The Chrome for Testing executable the desktop tests run, named by CRAZYAGENT_CHROME. There is
 * no default, so an unset variable never falls back to the owner's Google Chrome and its keychain.
 * Input: the environment. Output: the executable path. Throws when the variable is unset, empty
 * or names no file. */
export function chromeExecutable(env = process.env) {
  const chrome = env.CRAZYAGENT_CHROME;
  if (!chrome) throw new Error("these tests need Chrome for Testing: set CRAZYAGENT_CHROME to its executable (it is unset, and the tests never fall back to /Applications/Google Chrome.app)");
  if (!existsSync(chrome)) throw new Error(`these tests need Chrome for Testing, and CRAZYAGENT_CHROME names no file: ${chrome}`);
  return chrome;
}

/** The arguments a desktop test Chrome starts with: headless, a DevTools port Chrome picks, its own
 * profile, and a mock keychain with basic password storage so it never asks macOS for a keychain.
 * Input: the absolute profile folder. Output: the argument list. */
export function chromeArgs(profile) {
  assert.ok(isAbsolute(profile), "the Chrome profile is an absolute path");
  return ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run",
    "--no-default-browser-check", "--disable-extensions", "--use-mock-keychain", "--password-store=basic", "about:blank"];
}

/** Starts one test Chrome in a fresh `ca-chrome-*` profile, in the file process's own process group
 * so a wall that kills that group reaches Chrome too. If the process exits, or gets SIGTERM, SIGINT
 * or SIGHUP, Chrome is killed and the profile removed, and a signalled process then dies by that
 * signal. Input: the executable. Output: the Chrome process, its profile and `stop()`, which asks
 * Chrome to quit, waits for it and removes the profile. */
export function startChrome(executable) {
  const profile = mkdtempSync(join(tmpdir(), "ca-chrome-"));
  const chrome = spawn(executable, chromeArgs(profile), { stdio: "ignore" });
  const exited = new Promise((resolve) => chrome.once("exit", resolve));
  const running = () => chrome.exitCode === null && chrome.signalCode === null;
  chrome.unref();
  process.once("exit", () => {
    if (running()) chrome.kill("SIGKILL");
    rmSync(profile, REMOVE);
  });
  const reap = async (signal) => {
    for (const name of SIGNALS) process.removeListener(name, reap);
    try {
      chrome.ref();
      if (running()) chrome.kill("SIGKILL");
      await Promise.race([exited, pause(KILL_WAIT_MS)]);
      rmSync(profile, REMOVE);
    } finally {
      process.kill(process.pid, signal);
    }
  };
  for (const name of SIGNALS) process.on(name, reap);
  const stop = async () => {
    chrome.ref();
    if (running()) chrome.kill("SIGTERM");
    await exited;
    rmSync(profile, REMOVE);
  };
  return { chrome, profile, stop };
}
