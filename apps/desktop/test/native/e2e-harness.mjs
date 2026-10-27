import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createWriteStream, existsSync, readFileSync, writeFileSync } from "node:fs";
import { createServer, request as httpRequest } from "node:http";
import { extname, isAbsolute, join, resolve, sep } from "node:path";
import { setTimeout as pause } from "node:timers/promises";

export const READY_MS = 60000;
export const STEP_MS = 30000;
export const POLL_MS = 100;
const BODY_BYTES_MAX = 4 * 1024 * 1024;
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2" };

/** Reads an absolute path from the environment or stops the run with what to set.
 * Input: the variable name and what it names. Output: the existing path. */
export function required(name, what) {
  const value = process.env[name];
  assert.ok(value && isAbsolute(value) && existsSync(value), `BLOCKED: set ${name} to the built ${what} (absolute path)`);
  return value;
}

/** Finds a local port nothing listens on, for the provider endpoint nobody answers.
 * Input: none. Output: the port. */
export async function closedPort() {
  const server = createServer();
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}

/** Starts a child with its output kept as evidence and waits for its ready line.
 * Input: the binary, its arguments, environment, evidence folder, name and ready pattern.
 * Output: the child and the ready line's match. */
export async function start(binary, args, env, evidence, name, ready) {
  const child = spawn(binary, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  child.stderr.pipe(createWriteStream(join(evidence, `${name}.err`), { mode: 0o600 }));
  const out = createWriteStream(join(evidence, `${name}.out`), { mode: 0o600 });
  const match = await new Promise((done, fail) => {
    let seen = "";
    const timer = setTimeout(() => fail(new Error(`${name} printed no ready line in ${READY_MS} ms; see ${name}.err`)), READY_MS);
    child.stdout.on("data", (chunk) => {
      out.write(String(chunk).replace(/[0-9a-f]{64}/g, "<token>"));
      seen = (seen + chunk).slice(-4096);
      const found = seen.match(ready);
      if (found) {
        clearTimeout(timer);
        done(found);
      }
    });
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      fail(new Error(`${name} exited before ready: ${code ?? signal}; see ${name}.err`));
    });
  });
  return { child, match };
}

/** Stops a child and waits for it to exit. Input: the child. Output: none. */
export async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((done) => child.once("exit", done));
  child.stdin.end();
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), 20000);
  await exited;
  clearTimeout(timer);
}

/** Sends one `desktop_*` command to the end-to-end bridge, as the app's invoke would.
 * Input: the bridge port and token, the command and its arguments. Output: the bridge's answer. */
export function bridgeCall(bridge, cmd, args = {}) {
  const body = JSON.stringify({ cmd, args });
  return new Promise((done, fail) => {
    const call = httpRequest({ host: "127.0.0.1", port: bridge.port, path: "/invoke", method: "POST", agent: false,
      headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body), authorization: `Bearer ${bridge.token}` } }, (reply) => {
      const chunks = [];
      reply.on("data", (chunk) => chunks.push(chunk));
      reply.on("end", () => {
        try {
          done(JSON.parse(Buffer.concat(chunks).toString("utf8")));
        } catch (error) {
          fail(error);
        }
      });
    });
    call.on("error", (error) => fail(new Error(`${cmd} got no answer from the bridge: ${error.message}`)));
    call.end(body);
  });
}

/** Calls the bridge and requires the command's value, retrying a "busy" refusal the way the
 * app's bridge call does. Input: as bridgeCall. Output: the value. */
export async function value(bridge, cmd, args) {
  let answer = await bridgeCall(bridge, cmd, args);
  for (let attempt = 1; answer.failure?.code === "busy" && attempt < 8; attempt += 1) {
    await pause(250 * attempt);
    answer = await bridgeCall(bridge, cmd, args);
  }
  assert.ok("value" in answer, `${cmd} answered ${JSON.stringify(answer)}`);
  return answer.value;
}

/** Serves the built frontend and forwards the page's invokes to the bridge. The only command
 * answered here is the native folder chooser, with the owner's next pick from `picks`.
 * Input: the built folder, the bridge, the picks and the call log. Output: the server and its port. */
export async function serveApp(dist, bridge, picks, log) {
  const server = createServer((request, reply) => {
    if (request.method === "POST" && request.url === "/__invoke") {
      const chunks = [];
      let size = 0;
      request.on("data", (chunk) => {
        size += chunk.length;
        if (size > BODY_BYTES_MAX) request.destroy();
        else chunks.push(chunk);
      });
      request.on("end", async () => {
        const { cmd, args } = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        log.calls.push(cmd);
        let answer;
        if (cmd === "desktop_pick_folder") {
          assert.ok(picks.length > 0, "the page opened the folder chooser more often than the owner picks");
          answer = { value: picks.shift() };
          log.picked += 1;
        } else {
          const asked = Date.now();
          try {
            answer = await bridgeCall(bridge, cmd, args ?? {});
          } catch (error) {
            if (!log.closing) log.dropped.push(error.message);
            answer = { dropped: error.message };
          }
          log.answers.set(cmd, answer);
          log.trail.push({ at: Date.now(), ms: Date.now() - asked, cmd, args, answer });
          if ("unknown" in answer) log.unrouted.add(cmd);
          if ("invalid" in answer) log.invalid.push(`${cmd}: ${answer.invalid}`);
        }
        reply.writeHead(200, { "content-type": "application/json" });
        reply.end(JSON.stringify(answer));
      });
      return;
    }
    const path = decodeURIComponent((request.url ?? "/").split("?")[0]);
    const file = resolve(dist, `.${path === "/" ? "/index.html" : path}`);
    if (!file.startsWith(dist + sep) || !existsSync(file)) {
      reply.writeHead(404);
      reply.end();
      return;
    }
    reply.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
    reply.end(readFileSync(file));
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return { server, port: server.address().port };
}

/** The page script that stands in for Tauri's injected internals: each invoke goes to the file
 * server, which hands it to the real bridge, and page errors are kept for the final check. */
export const INTERNALS = `(() => {
  let next = 0;
  window.__e2eErrors = [];
  window.addEventListener("error", (event) => window.__e2eErrors.push(String(event.message)));
  window.addEventListener("unhandledrejection", (event) => window.__e2eErrors.push(String(event.reason?.message ?? event.reason)));
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args = {}) => {
      const reply = await fetch("/__invoke", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ cmd, args }) });
      const answer = await reply.json();
      if ("value" in answer) return answer.value;
      if ("failure" in answer) throw answer.failure;
      if ("invalid" in answer) throw answer.invalid;
      if ("dropped" in answer) throw new TypeError(answer.dropped);
      throw "Command " + cmd + " not found";
    },
    transformCallback: () => { next += 1; return next; },
    unregisterCallback: () => {},
    convertFileSrc: (path) => path,
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
  };
})();`;

/** Page helpers over DevTools: what the owner sees, waiting for it, and real clicks and typing.
 * Input: the page's evaluate and send. Output: the helpers. */
export function owner({ evaluate, send }, evidence) {
  const seen = () => evaluate("document.body.innerText");
  let shots = 0;
  const capture = async (label) => {
    shots += 1;
    const { data } = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(evidence, `${String(shots).padStart(2, "0")}-${label}.png`), Buffer.from(data, "base64"));
    writeFileSync(join(evidence, `${String(shots).padStart(2, "0")}-${label}.txt`), await seen());
  };
  const waitFor = async (what, check, ms = STEP_MS) => {
    const deadline = Date.now() + ms;
    for (;;) {
      const text = await seen();
      if (check(text)) return text;
      if (Date.now() > deadline) {
        await capture(`timeout-${what.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
        assert.fail(`the owner never saw ${what}; the page shows:\n${text.slice(0, 3000)}`);
      }
      await pause(POLL_MS);
    }
  };
  const center = async (selector, label) => {
    const name = label ?? selector;
    const deadline = Date.now() + STEP_MS;
    let last = null;
    for (;;) {
      const box = await find(selector, label);
      if (!box.missing && !box.disabled && box.covered === null && last?.x === box.x && last?.y === box.y) return box;
      if (Date.now() > deadline) {
        await capture(`no-${name.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
        assert.ok(!box.missing, `no "${name}" to click; visible: ${JSON.stringify(box.missing)}`);
        assert.equal(box.disabled, false, `"${name}" stays disabled`);
        assert.equal(box.covered, null, `"${name}" stays covered`);
        assert.fail(`"${name}" kept moving`);
      }
      last = box;
      await pause(POLL_MS);
    }
  };
  const find = async (selector, label) => {
    const box = await evaluate(`(() => {
      const wanted = ${JSON.stringify(label)};
      const all = [...document.querySelectorAll(${JSON.stringify(selector)})];
      const element = wanted === null ? all[0] : all.find((item) => (item.innerText ?? item.textContent ?? "").replace(/\\s+/g, " ").trim() === wanted
        || item.getAttribute("aria-label") === wanted);
      if (!element) return { missing: all.map((item) => (item.innerText ?? "").trim()).filter(Boolean).slice(0, 40) };
      element.scrollIntoView({ block: "center" });
      const rect = element.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      const y = rect.top + rect.height / 2;
      const hit = document.elementFromPoint(x, y);
      const covered = hit && (hit === element || element.contains(hit)) ? null : (hit ? hit.tagName + "." + hit.className : "nothing");
      return { x, y, covered, disabled: element.disabled === true || element.getAttribute("aria-disabled") === "true" };
    })()`);
    return box;
  };
  const press = async ({ x, y }) => {
    for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
      await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: type === "mouseMoved" ? 0 : 1 });
    }
  };
  const click = async (label, selector = "button, a, [role=radio], [role=tab], label") => press(await center(selector, label));
  const follow = async (selector) => press(await center(selector, null));
  const type = async (selector, text) => {
    const focused = await evaluate(`(() => { const element = document.querySelector(${JSON.stringify(selector)}); if (!element) return false; element.focus(); element.select?.(); return document.activeElement === element; })()`);
    assert.ok(focused, `no ${selector} to type into`);
    await send("Input.insertText", { text });
  };
  const enter = async () => {
    for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  };
  return { seen, waitFor, click, follow, type, enter, capture };
}
