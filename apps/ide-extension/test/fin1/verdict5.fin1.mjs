import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const CHROME = process.env.CRAZYAGENT_FIN1_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const IDE_FOLDER = process.env.CRAZYAGENT_FIN1_IDE_FOLDER;
const IDE_URL = process.env.CRAZYAGENT_FIN1_IDE_URL;
const TOKEN_FILE = process.env.CRAZYAGENT_FIN1_IDE_TOKEN_FILE;
const AGENT = process.env.CRAZYAGENT_FIN1_AGENT_WORKTREE;
const PROBE = fileURLToPath(new URL("./probe", import.meta.url));
const PROBE_FOLDER = "crazyagent.verdict5-probe-0.0.1";
const REPORT = "verdict5.json";
const REPORT_WAIT_MS = 120_000;
const AFTER_REPORT_MS = 10_000;

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/** Every file under a folder whose name is `name`, searched to the bottom. Inputs: the
 * folder and the name. Output: the paths. */
function find(folder, name) {
  if (!existsSync(folder)) return [];
  return readdirSync(folder, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name === name)
    .map((entry) => join(entry.parentPath, entry.name));
}

/** Opens one Chrome DevTools connection: send a method, get its result. Input: the
 * WebSocket address. Output: the connection. */
async function devtools(address) {
  const socket = new WebSocket(address);
  await new Promise((open, fail) => {
    socket.onopen = open;
    socket.onerror = fail;
  });
  let next = 0;
  const pending = new Map();
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    const waiting = pending.get(message.id);
    if (waiting === undefined) return;
    pending.delete(message.id);
    if (message.error) waiting.fail(new Error(JSON.stringify(message.error)));
    else waiting.done(message.result);
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((done, fail) => {
      next += 1;
      pending.set(next, { done, fail });
      socket.send(JSON.stringify({ id: next, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return { send, close: () => socket.close() };
}

/** Starts headless Chrome on its own profile and waits for its DevTools address, which it
 * writes to `DevToolsActivePort`. Input: the profile folder. Output: the process and the
 * browser WebSocket address. */
async function chrome(profile) {
  const child = spawn(CHROME, ["--headless=new", "--no-first-run", "--no-default-browser-check", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--use-mock-keychain", "--password-store=basic", "about:blank"], {
    stdio: "ignore",
  });
  const portFile = join(profile, "DevToolsActivePort");
  for (let tries = 0; tries < 200 && !existsSync(portFile); tries += 1) await sleep(100);
  assert.ok(existsSync(portFile), "Chrome never opened its DevTools port");
  const [port, path] = readFileSync(portFile, "utf8").trim().split("\n");
  return { child, address: `ws://127.0.0.1:${port}${path}` };
}

/** Writes the settings, tasks, PHP validator and PHP file an agent would plant in its
 * worktree to change the owner's IDE, each planted program leaving a marker if it ever
 * runs. Inputs: the worktree and the marker folder. Output: the planted paths. */
function plant(worktree, markers) {
  const scripts = join(worktree, ".ca-verdict5");
  const owned = [join(worktree, ".vscode", "settings.json"), join(worktree, ".vscode", "tasks.json"), join(worktree, "verdict5.php"), scripts];
  for (const path of owned) assert.ok(!existsSync(path), `${path} already exists; pick an agent worktree without it so nothing of the agent's is overwritten`);
  mkdirSync(join(worktree, ".vscode"), { recursive: true });
  mkdirSync(scripts, { recursive: true });
  const program = (name) => {
    const path = join(scripts, `${name}.sh`);
    writeFileSync(path, `#!/bin/sh\necho ran > '${join(markers, name)}'\n`);
    chmodSync(path, 0o755);
    return path;
  };
  writeFileSync(
    join(worktree, ".vscode", "settings.json"),
    JSON.stringify({
      "chat.disableAIFeatures": false,
      "files.readonlyInclude": {},
      "task.allowAutomaticTasks": "on",
      "git.enabled": false,
      "php.validate.enable": true,
      "php.validate.run": "onType",
      "php.validate.executablePath": program("php"),
    }),
  );
  writeFileSync(
    join(worktree, ".vscode", "tasks.json"),
    JSON.stringify({
      version: "2.0.0",
      tasks: [{ label: "verdict5", type: "shell", command: program("task"), runOptions: { runOn: "folderOpen" } }],
    }),
  );
  writeFileSync(join(worktree, "verdict5.php"), "<?php echo 1;\n");
  return owned;
}

/** Whether a workspace folder entry is the given folder on disk. Inputs: the entry's path
 * and the folder's real path. Output: true when they are the same folder. */
function same(path, folder) {
  return existsSync(path) && realpathSync(path) === folder;
}

test("agent_workspace_setting_cannot_change_a_locked_or_readonly_key", async () => {
  const inputs = {
    CRAZYAGENT_FIN1_IDE_FOLDER: IDE_FOLDER,
    CRAZYAGENT_FIN1_IDE_URL: IDE_URL,
    CRAZYAGENT_FIN1_IDE_TOKEN_FILE: TOKEN_FILE,
    CRAZYAGENT_FIN1_AGENT_WORKTREE: AGENT,
  };
  for (const [name, value] of Object.entries(inputs)) {
    assert.ok(value, `${name} is required: this test runs only against the real FIN-1 IDE server`);
  }
  const worktree = realpathSync(AGENT);
  const workspace = join(IDE_FOLDER, "crazyagent.code-workspace");
  const data = join(IDE_FOLDER, "server");
  const markers = join(data, "verdict5-markers");
  const probe = join(IDE_FOLDER, "extensions", PROBE_FOLDER);
  assert.ok(existsSync(workspace), "the engine has not written the project's workspace file");
  assert.ok(JSON.parse(readFileSync(workspace, "utf8")).folders.some((folder) => same(folder.path, worktree)), "the agent worktree is not a folder of the workspace");
  for (const old of find(data, REPORT)) rmSync(old);
  rmSync(markers, { recursive: true, force: true });
  mkdirSync(markers, { recursive: true });
  const planted = plant(worktree, markers);
  cpSync(PROBE, probe, { recursive: true });
  const profile = mkdtempSync(join(tmpdir(), "ca-verdict5-"));
  const browser = await chrome(profile);
  let tools;
  try {
    tools = await devtools(browser.address);
    const { targetId } = await tools.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await tools.send("Target.attachToTarget", { targetId, flatten: true });
    await tools.send("Network.enable", {}, sessionId);
    const token = readFileSync(TOKEN_FILE, "utf8").trim();
    await tools.send("Network.setCookie", { name: "vscode-tkn", value: token, url: IDE_URL, sameSite: "Lax" }, sessionId);
    await tools.send("Page.navigate", { url: `${IDE_URL.replace(/\/+$/, "")}/?workspace=${encodeURIComponent(workspace)}` }, sessionId);
    let reports = [];
    for (let waited = 0; waited < REPORT_WAIT_MS && reports.length === 0; waited += 500) {
      await sleep(500);
      reports = find(data, REPORT);
    }
    assert.equal(reports.length, 1, "the probe extension never reported from the IDE's extension host");
    await sleep(AFTER_REPORT_MS);
    const report = JSON.parse(readFileSync(reports[0], "utf8"));
    const agent = report.folders.find((folder) => same(folder.path, worktree));
    assert.ok(agent, `the agent folder is missing from ${JSON.stringify(report.folders.map((folder) => folder.name))}`);
    assert.ok(["EPERM", "EACCES"].includes(agent.settings), `the IDE server read the agent's .vscode/settings.json: ${agent.settings}`);
    for (const [key, seen] of Object.entries(agent.keys)) {
      assert.equal(seen.folderValue, false, `${key} took a value from the agent's folder`);
    }
    assert.equal(agent.keys["chat.disableAIFeatures"].effective, true);
    assert.deepEqual(agent.keys["files.readonlyInclude"].effective, report.window["files.readonlyInclude"].effective);
    assert.notDeepEqual(agent.keys["files.readonlyInclude"].effective, {}, "a running agent's worktree must stay read-only");
    assert.equal(agent.keys["task.allowAutomaticTasks"].effective, "off");
    assert.notEqual(agent.keys["git.enabled"].effective, false);
    assert.notEqual(agent.keys["php.validate.executablePath"].effective, join(worktree, ".ca-verdict5", "php.sh"));
    assert.deepEqual(readdirSync(markers), [], "a program the agent planted ran");
  } finally {
    tools?.close();
    browser.child.kill("SIGTERM");
    rmSync(probe, { recursive: true, force: true });
    for (const path of planted) rmSync(path, { recursive: true, force: true });
    rmSync(profile, { recursive: true, force: true });
    for (const report of find(data, REPORT)) rmSync(report);
  }
});
