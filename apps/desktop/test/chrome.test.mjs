import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { chromeArgs, chromeExecutable } from "./chrome.mjs";

const MODULE = new URL("./chrome.mjs", import.meta.url).href;
const KEYCHAIN = ["--use-mock-keychain", "--password-store=basic"];
const WAIT_MS = 5000;
const REMOVE = { recursive: true, force: true, maxRetries: 20, retryDelay: 100 };
const FAKE_CHROME = `#!/bin/sh
for arg in "$@"; do
  case "$arg" in --user-data-dir=*) profile="\${arg#--user-data-dir=}" ;; esac
done
printf '%s\\n' "$@" > "$CA_FAKE_CHROME_ARGS"
: > "$profile/Local State"
exec /bin/sleep 600
`;
const CHILD = `const { chromeExecutable, startChrome } = await import(${JSON.stringify(MODULE)});
const { chrome, profile } = startChrome(chromeExecutable());
process.once("SIGUSR2", () => process.exit(0));
process.stdout.write(JSON.stringify({ pid: chrome.pid, profile }) + "\\n");
setInterval(() => {}, 1000);`;
const ENDINGS = [
  { send: "SIGTERM", end: { code: null, killed: "SIGTERM" }, how: "killed by SIGTERM" },
  { send: "SIGINT", end: { code: null, killed: "SIGINT" }, how: "killed by SIGINT" },
  { send: "SIGHUP", end: { code: null, killed: "SIGHUP" }, how: "killed by SIGHUP" },
  { send: "SIGUSR2", end: { code: 0, killed: null }, how: "that exits on its own" },
];

/** Whether a process id is still taken. Input: the pid. Output: true until the process is gone. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error.code === "ESRCH") return false;
    throw error;
  }
}

/** Waits for a condition, failing after WAIT_MS. Inputs: the condition and what it means. */
async function until(done, what) {
  const deadline = Date.now() + WAIT_MS;
  while (!done()) {
    assert.ok(Date.now() < deadline, `${what} within ${WAIT_MS} ms`);
    await pause(20);
  }
}

/** Waits for a promise, failing after WAIT_MS so a test never hangs on a child that will not end.
 * Inputs: the promise and what it means. Output: what the promise gives. */
async function within(promise, what) {
  let timer;
  const late = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${what} within ${WAIT_MS} ms`)), WAIT_MS); });
  try {
    return await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** The first line a stream writes. Input: the stream. Output: the line, or a rejection when the
 * stream ends first. */
function firstLine(stream) {
  return new Promise((resolve, reject) => {
    let text = "";
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      text += chunk;
      if (text.includes("\n")) resolve(text.slice(0, text.indexOf("\n")));
    });
    stream.once("end", () => reject(new Error(`the child ended before its ready line: ${text}`)));
  });
}

test("the test Chrome starts with a mock keychain and basic password storage, so it never asks macOS for a keychain", () => {
  const args = chromeArgs("/tmp/ca-chrome-args");
  for (const flag of KEYCHAIN) assert.ok(args.includes(flag), `${flag} is passed`);
  assert.ok(args.includes("--user-data-dir=/tmp/ca-chrome-args"), "the profile is passed");
});

test("an unset, empty or missing CRAZYAGENT_CHROME fails naming the variable and never falls back to Google Chrome", () => {
  const root = mkdtempSync(join(tmpdir(), "ca-kc-test-"));
  try {
    for (const env of [{}, { CRAZYAGENT_CHROME: "" }, { CRAZYAGENT_CHROME: join(root, "absent") }]) {
      assert.throws(() => chromeExecutable(env), /CRAZYAGENT_CHROME/);
    }
    assert.equal(chromeExecutable({ CRAZYAGENT_CHROME: process.execPath }), process.execPath);
  } finally {
    rmSync(root, REMOVE);
  }
});

for (const { send, end, how } of ENDINGS) {
  test(`a test file process ${how} leaves no Chrome process and no profile`, async () => {
    const root = mkdtempSync(join(tmpdir(), "ca-kc-test-"));
    const temp = join(root, "tmp");
    const fake = join(root, "chrome");
    const argsFile = join(root, "args");
    mkdirSync(temp);
    writeFileSync(fake, FAKE_CHROME);
    chmodSync(fake, 0o755);
    const child = spawn(process.execPath, ["--input-type=module", "-e", CHILD], {
      env: { PATH: "/usr/bin:/bin", TMPDIR: temp, CRAZYAGENT_CHROME: fake, CA_FAKE_CHROME_ARGS: argsFile },
      stdio: ["ignore", "pipe", "inherit"],
    });
    const ended = new Promise((resolve) => child.once("exit", (code, killed) => resolve({ code, killed })));
    let pid = null;
    try {
      const ready = JSON.parse(await within(firstLine(child.stdout), "the child's ready line"));
      pid = ready.pid;
      assert.ok(Number.isInteger(pid) && pid > 0, "the stand-in Chrome pid");
      await until(() => existsSync(join(ready.profile, "Local State")), "the stand-in Chrome wrote into its profile");
      const passed = readFileSync(argsFile, "utf8").split("\n");
      for (const flag of KEYCHAIN) assert.ok(passed.includes(flag), `the spawned Chrome got ${flag}`);
      child.kill(send);
      assert.deepEqual(await within(ended, `the test file process ending after ${send}`), end, `the test file process ends as it would without Chrome after ${send}`);
      await until(() => !alive(pid), "the stand-in Chrome is gone");
      assert.equal(existsSync(ready.profile), false, "the profile is removed");
      assert.deepEqual(readdirSync(temp).filter((name) => name.startsWith("ca-chrome-")), [], "no ca-chrome-* profile is left");
    } finally {
      if (pid !== null && alive(pid)) process.kill(pid, "SIGKILL");
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      rmSync(root, REMOVE);
    }
  });
}
