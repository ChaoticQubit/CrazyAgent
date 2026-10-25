import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "vite";

const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const STARTUP_MS = 15000;
const CALL_MS = 15000;
const appRoot = fileURLToPath(new URL("..", import.meta.url));

/** Bundles one test fixture and the production modules it imports into a script that defines
 * `Fixture` in the page. Input: the fixture path. Output: the script text. */
export async function bundle(fixture) {
  const result = await build({
    configFile: false, root: appRoot, logLevel: "silent",
    define: { "process.env.NODE_ENV": JSON.stringify("development") },
    build: { write: false, minify: false, emptyOutDir: false, lib: { entry: fixture, formats: ["iife"], name: "Fixture", fileName: "fixture" } },
  });
  const outputs = (Array.isArray(result) ? result : [result]).flatMap((item) => item.output);
  const chunks = outputs.filter((item) => item.type === "chunk");
  assert.equal(chunks.length, 1, "one fixture script");
  return chunks[0].code;
}

async function devtoolsPort(profile, chrome) {
  const file = join(profile, "DevToolsActivePort");
  const deadline = Date.now() + STARTUP_MS;
  while (!existsSync(file) || !readFileSync(file, "utf8").includes("\n")) {
    assert.equal(chrome.exitCode, null, "Chrome exited before it was ready");
    assert.ok(Date.now() < deadline, "Chrome did not start in time");
    await pause(50);
  }
  const port = Number(readFileSync(file, "utf8").split("\n")[0]);
  assert.ok(Number.isInteger(port) && port > 0 && port < 65536, "Chrome debugging port");
  return port;
}

/** Runs a check in a real headless Google Chrome page and always stops Chrome afterwards.
 * Input: the check, given `evaluate(expression)` for page scripts (awaited, returned by value)
 * and `send(method, params)` for DevTools input events. Output: what the check returns. */
export async function withPage(check) {
  assert.ok(existsSync(CHROME), `these tests need Google Chrome at ${CHROME}`);
  const profile = mkdtempSync(join(tmpdir(), "ca-chrome-"));
  const chrome = spawn(CHROME, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
    "--no-first-run", "--no-default-browser-check", "--disable-extensions", "about:blank"], { stdio: "ignore" });
  const exited = new Promise((resolve) => chrome.once("exit", resolve));
  let socket = null;
  try {
    const port = await devtoolsPort(profile, chrome);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const page = targets.find((target) => target.type === "page");
    assert.ok(page, "Chrome page target");
    socket = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = () => reject(new Error("DevTools connection failed")); });
    const waiting = new Map();
    let next = 0;
    socket.onmessage = (message) => {
      const reply = JSON.parse(message.data);
      const pending = waiting.get(reply.id);
      if (!pending) return;
      waiting.delete(reply.id);
      if (reply.error) pending.reject(new Error(reply.error.message));
      else pending.resolve(reply.result);
    };
    const send = (method, params = {}) => new Promise((resolve, reject) => {
      next += 1;
      const id = next;
      const timer = setTimeout(() => { waiting.delete(id); reject(new Error(`${method} timed out`)); }, CALL_MS);
      waiting.set(id, { resolve: (value) => { clearTimeout(timer); resolve(value); }, reject: (error) => { clearTimeout(timer); reject(error); } });
      socket.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async (expression) => {
      const reply = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
      if (reply.exceptionDetails) throw new Error(reply.exceptionDetails.exception?.description ?? reply.exceptionDetails.text);
      return reply.result.value;
    };
    await send("Emulation.setFocusEmulationEnabled", { enabled: true });
    assert.equal(await evaluate("document.hasFocus()"), true, "the page has focus, as the app window does");
    return await check({ evaluate, send });
  } finally {
    socket?.close();
    if (chrome.exitCode === null) chrome.kill("SIGTERM");
    await exited;
    rmSync(profile, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
}
