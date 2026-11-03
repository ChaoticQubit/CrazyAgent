import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const DESKTOP = fileURLToPath(new URL("../..", import.meta.url));
const WDIO = join(DESKTOP, "node_modules", "@wdio", "cli", "bin", "wdio.js");
const CONF = join(DESKTOP, "test", "native", "trial3.wdio.conf.mjs");
const RUN_MS = 120000;

/** Runs the trial 3 WebdriverIO config the way run.mjs does, with an app path that does not
 * exist so no window can start. Inputs: the scratch folder and the extra environment.
 * Output: the exit status and everything the launcher printed. */
function launch(scratch, extra) {
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, LANG: "en_US.UTF-8", TMPDIR: scratch,
    TRIAL3_APP: join(scratch, "no-such-app"), TRIAL_EVIDENCE: join(scratch, "evidence"), WD_PORT: "4799", ...extra };
  const result = spawnSync(process.execPath, [WDIO, "run", CONF], { cwd: DESKTOP, env, encoding: "utf8", timeout: RUN_MS, killSignal: "SIGKILL" });
  return { status: result.status, output: `${result.stdout}\n${result.stderr}` };
}

/** Makes a short scratch folder under /tmp, so a fixture engine socket path fits. Output: its real path. */
function scratch() {
  return realpathSync(mkdtempSync("/tmp/ca-t3c-"));
}

test("a live run with no engine socket stops before any spec or window starts", () => {
  const folder = scratch();
  try {
    mkdirSync(join(folder, "project", ".git"), { recursive: true });
    const run = launch(folder, { TRIAL3_MODE: "live", CRAZYAGENT_DESKTOP_SOCKET: join(folder, "missing.sock"), TRIAL_PROJECT: join(folder, "project"), TRIAL_PROMPT: CONF });
    assert.notEqual(run.status, 0, run.output);
    assert.match(run.output, /trial 3 cannot start: no engine socket at /, run.output);
    assert.doesNotMatch(run.output, /RUNNING in /, "no spec ran");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a fixture root that already exists stops the run before any spec starts", () => {
  const folder = scratch();
  try {
    mkdirSync(join(folder, "root"));
    const run = launch(folder, { TRIAL3_MODE: "fixture", TRIAL3_FIXTURE_ROOT: join(folder, "root"), TRIAL3_ENGINE: "/usr/bin/false", TRIAL3_BRIDGE: "/usr/bin/false" });
    assert.notEqual(run.status, 0, run.output);
    assert.match(run.output, /trial 3 cannot start: TRIAL3_FIXTURE_ROOT must name a new folder/, run.output);
    assert.doesNotMatch(run.output, /RUNNING in /, "no spec ran");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a new fixture root is still new when the fixture is built, so its own failure stops the run", () => {
  const folder = scratch();
  try {
    const root = join(folder, "root");
    const run = launch(folder, { TRIAL3_MODE: "fixture", TRIAL3_FIXTURE_ROOT: root, TRIAL3_ENGINE: "/usr/bin/false", TRIAL3_BRIDGE: "/usr/bin/false" });
    assert.notEqual(run.status, 0, run.output);
    assert.match(run.output, /trial 3 cannot start: engine exited before ready/, run.output);
    assert.doesNotMatch(run.output, /RUNNING in /, "no spec ran");
    assert(existsSync(join(root, "home", "Projects", "ca-trial-3", ".git")), "the fixture seeded its project before the engine start failed");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});
