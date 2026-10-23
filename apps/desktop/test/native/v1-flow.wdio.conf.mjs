import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createWriteStream, mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { managedCodexHome, returnSignIn } from "./v1-flow-auth.mjs";

process.umask(0o077);
managedCodexHome(process.env);
const app = process.env.WD_APP;
const engineBinary = process.env.V1_ENGINE;
const root = process.env.V1_ROOT;
const evidence = process.env.V1_EVIDENCE;
const codex = process.env.V1_CODEX;
const port = Number(process.env.WD_PORT);
const ollama = process.env.V1_OLLAMA_URL ?? "http://127.0.0.1:11434/v1";
const ollamaModel = process.env.V1_OLLAMA_MODEL ?? "qwen2.5:0.5b";
for (const [name, path] of [["WD_APP", app], ["V1_ENGINE", engineBinary], ["V1_ROOT", root], ["V1_EVIDENCE", evidence], ["V1_CODEX", codex]]) {
  assert(path && isAbsolute(path), `${name} must be an absolute path`);
}
for (const name of ["V1_CLAUDE", "V1_RCLI"]) assert(!process.env[name] || isAbsolute(process.env[name]), `${name} must be an absolute path when set`);
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");
const socket = join(root, "engine.sock");
assert(Buffer.byteLength(socket) < 100, "V1_ROOT must be short enough for the engine socket path");
const ledger = join(evidence, "borrowed-sign-in.json");
const ENGINE_READY_MS = 60000;
const ENGINE_STOP_MS = 20000;
let engine;

async function ollamaReady() {
  const blocked = `BLOCKED: start Ollama at ${ollama} with ${ollamaModel} pulled`;
  let models;
  try {
    const reply = await fetch(`${ollama}/models`, { signal: AbortSignal.timeout(5000) });
    models = reply.ok ? (await reply.json()).data : undefined;
  } catch {
    throw new Error(blocked);
  }
  if (!Array.isArray(models) || !models.some((model) => model?.id === ollamaModel)) throw new Error(blocked);
}

function startEngine() {
  const args = ["engine", root, join(root, "global.toml"), join(root, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", codex];
  if (process.env.V1_CLAUDE) args.push("--claude-executable", process.env.V1_CLAUDE);
  if (process.env.V1_RCLI) args.push("--rcli-executable", process.env.V1_RCLI);
  engine = spawn(engineBinary, args, { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
  engine.stdout.pipe(createWriteStream(join(evidence, "engine.out"), { mode: 0o600 }));
  engine.stderr.pipe(createWriteStream(join(evidence, "engine.err"), { mode: 0o600 }));
  return new Promise((resolve, reject) => {
    let seen = "";
    const timer = setTimeout(() => reject(new Error(`engine did not print ready within ${ENGINE_READY_MS} ms; see engine.err`)), ENGINE_READY_MS);
    engine.stdout.on("data", (chunk) => {
      seen = (seen + chunk).slice(-4096);
      if (/^ready$/m.test(seen)) {
        clearTimeout(timer);
        resolve();
      }
    });
    engine.once("exit", (code, signal) => {
      clearTimeout(timer);
      reject(new Error(`engine exited before ready: ${code ?? signal}; see engine.err`));
    });
  });
}

async function stopEngine() {
  if (!engine || engine.exitCode !== null || engine.signalCode !== null) return;
  const exited = new Promise((resolve) => engine.once("exit", resolve));
  engine.kill("SIGTERM");
  const timer = setTimeout(() => engine.kill("SIGKILL"), ENGINE_STOP_MS);
  await exited;
  clearTimeout(timer);
}

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("v1-flow.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: app } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded", autoInstallTauriDriver: false, embeddedPort: port,
    appArgs: [], startTimeout: 60000, captureBackendLogs: true, backendLogLevel: "info",
    env: { CRAZYAGENT_DESKTOP_SOCKET: socket },
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  outputDir: join(evidence, "webdriver"),
  connectionRetryCount: 0,
  connectionRetryTimeout: 180000,
  waitforTimeout: 15000,
  specFileRetries: 0,
  mochaOpts: { timeout: 1800000, retries: 0 },
  onPrepare: async function () {
    mkdirSync(evidence, { recursive: true, mode: 0o700 });
    await ollamaReady();
    try {
      mkdirSync(root, { mode: 0o700 });
    } catch {
      throw new Error(`V1_ROOT must name a new folder so the flow starts from first run: ${root}`);
    }
    writeFileSync(join(root, "global.toml"), "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n", { mode: 0o600 });
    try {
      await startEngine();
    } catch (failure) {
      await stopEngine();
      throw failure;
    }
  },
  before: async function () {
    await globalThis.browser.setWindowSize(1280, 900);
  },
  afterTest: async function (test, context, result) {
    if (!result.passed) await globalThis.browser.saveScreenshot(join(evidence, `failure-${test.title.replace(/[^A-Za-z0-9]+/g, "-").slice(0, 60)}.png`));
  },
  onComplete: async function () {
    await stopEngine();
    if (returnSignIn(ledger)) console.log("v1-flow: the managed Codex sign-in is back in CRAZYAGENT_MANAGED_CODEX_HOME");
  },
};
