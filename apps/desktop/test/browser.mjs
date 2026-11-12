import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { chromeExecutable, startChrome } from "./chrome.mjs";

const STARTUP_MS = 15000;
const CALL_MS = 15000;
const appRoot = fileURLToPath(new URL("..", import.meta.url));
const PAGE = '<!doctype html><html lang="en"><head></head><body></body></html>';

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

async function devtoolsEndpoint(profile, chrome) {
  const file = join(profile, "DevToolsActivePort");
  const deadline = Date.now() + STARTUP_MS;
  while (!existsSync(file) || readFileSync(file, "utf8").split("\n").length < 2) {
    assert.equal(chrome.exitCode, null, "Chrome exited before it was ready");
    assert.ok(Date.now() < deadline, "Chrome did not start in time");
    await pause(50);
  }
  const [line, path] = readFileSync(file, "utf8").split("\n");
  const port = Number(line);
  assert.ok(Number.isInteger(port) && port > 0 && port < 65536, "Chrome debugging port");
  assert.match(path, /^\/devtools\/browser\/[\w-]+$/, "Chrome browser endpoint");
  return { origin: `ws://127.0.0.1:${port}`, browser: `ws://127.0.0.1:${port}${path}` };
}

/** Opens one DevTools connection. Input: its WebSocket URL. Output: `send(method, params)`, which
 * fails a call Chrome does not answer within CALL_MS or once the connection closes,
 * `on(method, listener)` for DevTools events such as `Network.loadingFailed`, and `close()`. */
async function connect(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = () => reject(new Error("DevTools connection failed")); });
  const waiting = new Map();
  const listeners = new Map();
  let next = 0;
  socket.onmessage = (message) => {
    const reply = JSON.parse(message.data);
    if (reply.method) {
      for (const listener of listeners.get(reply.method) ?? []) listener(reply.params);
      return;
    }
    const pending = waiting.get(reply.id);
    if (!pending) return;
    waiting.delete(reply.id);
    if (reply.error) pending.reject(new Error(reply.error.message));
    else pending.resolve(reply.result);
  };
  socket.onclose = () => {
    for (const pending of waiting.values()) pending.reject(new Error("DevTools connection closed"));
    waiting.clear();
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    if (socket.readyState !== WebSocket.OPEN) {
      reject(new Error(`${method}: DevTools connection closed`));
      return;
    }
    next += 1;
    const id = next;
    const timer = setTimeout(() => { waiting.delete(id); reject(new Error(`${method} timed out`)); }, CALL_MS);
    waiting.set(id, { resolve: (value) => { clearTimeout(timer); resolve(value); }, reject: (error) => { clearTimeout(timer); reject(error); } });
    socket.send(JSON.stringify({ id, method, params }));
  });
  const on = (method, listener) => listeners.set(method, [...(listeners.get(method) ?? []), listener]);
  return { send, on, close: () => socket.close() };
}

/** Starts the one headless Chrome for Testing named by CRAZYAGENT_CHROME that a test file's pages
 * run in, before any test starts, so its start counts against STARTUP_MS and never against a
 * test's own time. Chrome stops once the file has nothing left to run; `startChrome` kills it and
 * removes its profile if the process exits or is signalled first. Output: the DevTools origin and
 * browser endpoint. */
async function launch() {
  const { chrome, profile, stop } = startChrome(chromeExecutable());
  try {
    const endpoint = await devtoolsEndpoint(profile, chrome);
    const browser = await connect(endpoint.browser);
    try {
      await browser.send("Browser.getVersion");
    } finally {
      browser.close();
    }
    process.once("beforeExit", stop);
    return endpoint;
  } catch (error) {
    await stop();
    throw error;
  }
}

const chrome = await launch();

/** Runs a check in a fresh page of the file's headless Google Chrome, in its own browser context
 * so no storage, permission or cache crosses tests, and always closes that context afterwards.
 * Input: the check, given `evaluate(expression)` for page scripts (awaited, returned by value),
 * `send(method, params)` for DevTools commands, `on(method, listener)` for DevTools events such
 * as `Network.loadingFailed` and `context`, the browser context id. The page has the doctype
 * the app's page has, so a check measures the layout the app shows. A promise a script returns
 * is held by a DevTools object until it settles: awaited unheld, DevTools can answer "Promise was
 * collected" while the page goes on working. Output: what the check returns. */
export async function withPage(check) {
  const browser = await connect(chrome.browser);
  try {
    const { browserContextId } = await browser.send("Target.createBrowserContext");
    assert.ok(browserContextId, "a browser context");
    try {
      const { targetId } = await browser.send("Target.createTarget", { url: "about:blank", browserContextId });
      const page = await connect(`${chrome.origin}/devtools/page/${targetId}`);
      try {
        const evaluate = async (expression) => {
          const failed = (reply) => {
            if (reply.exceptionDetails) throw new Error(reply.exceptionDetails.exception?.description ?? reply.exceptionDetails.text);
            return reply;
          };
          const { result } = failed(await page.send("Runtime.evaluate", { expression }));
          if (result.subtype === "promise") return failed(await page.send("Runtime.awaitPromise", { promiseObjectId: result.objectId, returnByValue: true })).result.value;
          if (!result.objectId) return result.value;
          return failed(await page.send("Runtime.callFunctionOn", { objectId: result.objectId, functionDeclaration: "function () { return this; }", returnByValue: true })).result.value;
        };
        await evaluate(`document.open(); document.write(${JSON.stringify(PAGE)}); document.close();`);
        assert.equal(await evaluate("document.compatMode"), "CSS1Compat", "the page is in standards mode, as the app's page is");
        await page.send("Emulation.setFocusEmulationEnabled", { enabled: true });
        assert.equal(await evaluate("document.hasFocus()"), true, "the page has focus, as the app window does");
        return await check({ evaluate, send: page.send, on: page.on, context: browserContextId });
      } finally {
        page.close();
      }
    } finally {
      await browser.send("Target.disposeBrowserContext", { browserContextId });
    }
  } finally {
    browser.close();
  }
}
