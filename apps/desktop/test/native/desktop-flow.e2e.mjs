import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer, request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { extname, isAbsolute, join, resolve, sep } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const READY_MS = 60000;
const STEP_MS = 30000;
const SETTLE_MS = 180000;
const POLL_MS = 100;
const BODY_BYTES_MAX = 4 * 1024 * 1024;
const REPO = "plant-shop";
const ABOUT = "A small shop that sells house plants online.";
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2" };

/** Reads an absolute path from the environment or stops the run with what to set.
 * Input: the variable name and what it names. Output: the existing path. */
function required(name, what) {
  const value = process.env[name];
  assert.ok(value && isAbsolute(value) && existsSync(value), `BLOCKED: set ${name} to the built ${what} (absolute path)`);
  return value;
}

/** Finds a local port nothing listens on, for the provider endpoint nobody answers.
 * Input: none. Output: the port. */
async function closedPort() {
  const server = createServer();
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}

/** Starts a child with its output kept as evidence and waits for its ready line.
 * Input: the binary, its arguments, environment, evidence folder, name and ready pattern.
 * Output: the child and the ready line's match. */
async function start(binary, args, env, evidence, name, ready) {
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
async function stop(child) {
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
function bridgeCall(bridge, cmd, args = {}) {
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
async function value(bridge, cmd, args) {
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
async function serveApp(dist, bridge, picks, log) {
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
const INTERNALS = `(() => {
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

/** Makes the owner's existing project: a git repository with one commit.
 * Input: the folder. Output: none. */
function gitRepo(folder) {
  mkdirSync(join(folder, "src"), { recursive: true });
  writeFileSync(join(folder, "README.md"), "# Plant shop\n\nSells house plants.\n");
  writeFileSync(join(folder, "src", "main.rs"), "fn main() {\n    println!(\"plants\");\n}\n");
  const git = (...args) => {
    const result = spawnSync("git", ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8" });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  };
  git("init", "-q", "-b", "main");
  git("add", ".");
  git("commit", "-q", "-m", "Start the plant shop");
}

/** Page helpers over DevTools: what the owner sees, waiting for it, and real clicks and typing.
 * Input: the page's evaluate and send. Output: the helpers. */
function owner({ evaluate, send }, evidence) {
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
  return { seen, waitFor, click, follow, type, capture };
}

test("the owner adds a folder, sets it up, opens its team, then removes it, on the real engine", { timeout: 900000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-e2e-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  mkdirSync(evidence, { recursive: true, mode: 0o700 });
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  for (const folder of [runtime, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  gitRepo(repo);
  const provider = await closedPort();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
  try {
    ({ child: engine } = await start(engineBinary,
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
      { ...env }, evidence, "engine", /^ready$/m));
    const started = await start(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    const applied = (receipt) => receipt?.outcome;
    const profiled = await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 });
    assert.equal(applied(profiled), "applied", `the owner profile is saved: ${JSON.stringify(profiled)}`);
    const homed = await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 });
    assert.equal(applied(homed), "applied", `the projects home is set: ${JSON.stringify(homed)}`);
    assert.equal((await value(bridge, "desktop_projects_home")).path, projects);

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [repo], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await flow(page, me);
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    async function flow(page, me) {
      await page.send("Page.enable");
      await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/` });

      const empty = await me.waitFor("Home with no projects", (text) => /No projects yet/.test(text));
      assert.doesNotMatch(empty, /Continue setup|being set up/i, "Home starts with nothing being set up");
      await me.capture("home-empty");

      await me.click("Add folder");
      await me.waitFor("the Add folder page", (text) => text.includes("Add an existing folder"));
      await me.click("Choose folder");
      await me.waitFor("the chosen folder", (text) => text.includes(`${REPO} stays where it is`) || text.includes(REPO) && text.includes("stays where it is"));
      assert.equal(log.picked, 1, "the owner picked the folder once");
      await me.click("Set up here");
      await me.waitFor("the Set up dialog with the pool", (text) => text.includes(`Set up ${REPO}`) && text.includes("local-pool"));
      await me.capture("set-up-dialog");
      await me.click("Start setup");

      await me.waitFor("the setup page", (text) => /Build the code graph/.test(text) && /Map the project/.test(text));
      let graphify;
      for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
        graphify = (await value(bridge, "desktop_setup_status")).dependencies.find((tool) => tool.name === "graphify");
        assert.ok(graphify, "the engine reports Graphify");
        if (graphify.state.state !== "checking") break;
        assert.ok(Date.now() < deadline, "the engine never finished checking Graphify");
      }
      const settled = await me.waitFor("both setup steps settled", (text) => !/Running|Waiting|Starting/.test(text.split("Overview")[0] ?? text), SETTLE_MS);
      await me.capture("setup-settled");
      const steps = settled.split("Overview")[0];
      if (graphify.state.state === "found") {
        assert.match(steps, /Build the code graph\s*Done/, "the engine found Graphify, so the graph step is done");
      } else {
        assert.doesNotMatch(steps, /Build the code graph\s*Done/, "the engine reports Graphify missing, so the graph step cannot claim done");
      }
      if (/Map the project[\s\S]*Failed/.test(steps)) {
        assert.match(settled, /Retry mapping/, "a failed mapping offers Retry");
        assert.match(settled, /Skip mapping/, "a failed mapping offers Skip");
        assert.doesNotMatch(steps, /Agent stopped with an error/, "a failed mapping shows its own reason");
      }

      await me.type("#ob-about", ABOUT);
      await me.click("Confirm and finish setup");
      const setUp = await me.waitFor("the project page with its folder", (text) => text.includes(`${REPO} is set up.`) && text.includes("Project folder"));
      await me.capture("set-up");
      assert.match(setUp, new RegExp(`Project folder\\s+${repo.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s`), "the project page shows the owner's folder");
      assert.doesNotMatch(setUp, /\.crazyagent/, "the project page does not show CrazyAgent's own folder as the project");
      assert.doesNotMatch(setUp, /Couldn't give/, "the chosen pool went to the project's master");
      await me.click("Home", "nav a, aside a, a");
      const ready = await me.waitFor("the project card on Home", (text) => /Projects/.test(text) && !/No projects yet/.test(text) && text.split("Get set up")[0].includes(REPO));
      await me.capture("home-ready");
      assert.doesNotMatch(ready, /Continue setup|being set up|Setting up/i, "nothing is left being set up (LB-10)");
      const cards = await page.evaluate(`[...document.querySelectorAll(".cards a[href*='/projects/']")].map((card) => card.innerText.replace(/\\s+/g, " ").trim())`);
      assert.equal(cards.length, 1, `Home shows one project card: ${JSON.stringify(cards)}`);
      assert.match(cards[0], new RegExp(REPO), "the card is the project"); 
      const found = await value(bridge, "desktop_found_projects");
      assert.equal(found.filter((entry) => entry.path === repo && entry.state === "onboarding").length, 0, "no onboarding entry is left for the folder");

      await me.follow(`.cards a[href*="/projects/"]`);
      await me.waitFor("the project page", (text) => text.includes(REPO) && /Conversation/.test(text) && /Agents/.test(text));
      await me.click("Agents", "a, button, [role=tab]");
      await me.waitFor("the team graph", () => true);
      for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
        const graph = await page.evaluate(`(() => { const section = document.querySelector("section.tg"); return section ? { busy: section.getAttribute("aria-busy") === "true", text: section.innerText } : null; })()`);
        if (graph && !graph.busy) {
          assert.doesNotMatch(graph.text, /Couldn't|isn't in this build/i, "the team graph renders");
          assert.match(graph.text, /Master/, "the team graph shows the master");
          break;
        }
        assert.ok(Date.now() < deadline, "the team graph never rendered");
      }
      await me.capture("team");

      await me.click("Conversation", "a, button, [role=tab]");
      await me.waitFor("the master conversation", (text) => text.includes("Message the master") || text.includes("Send"));
      await me.type("#pj-composer", "Hello master, what is in this repository?");
      await me.click("Send");
      const line = /Waiting to start|Thinking\.\.\.|The master stopped|Not signed in|Sign-in is out of date|Couldn't send|isn't installed/;
      const turned = await me.waitFor("the master's first turn", (text) => line.test(text), STEP_MS);
      await me.capture("master-turn");
      assert.match(turned, /Hello master, what is in this repository\?/, "the owner's message is shown");
      assert.doesNotMatch(turned, /master repository missing/i, "the project has its repository (LB-13)");
      const turns = log.answers.get("desktop_master_turns")?.value?.page?.turns ?? [];
      assert.ok(turns.length > 0, `the engine admitted the master's first turn: ${JSON.stringify(log.answers.get("desktop_master_turns") ?? null)}`);
      const words = { queued: "Waiting to start", prepared: "Waiting to start", delivering: "Waiting to start", running: "Thinking...", waiting_approval: "Waiting for your approval", succeeded: "Replied", cancelled: "Stopped" };
      const engineTurn = () => log.answers.get("desktop_master_turns")?.value?.page?.turns?.at(-1);
      const agrees = (text) => {
        const last = engineTurn();
        if (last.reason) return text.includes(last.reason.replaceAll("_", " ")) || /Not signed in|Sign-in is out of date|Settings changed/.test(text);
        return text.includes(words[last.phase] ?? "The master stopped");
      };
      await me.waitFor("the page showing the engine's own turn state", agrees, STEP_MS);

      const remove = async () => {
        await me.follow("#tab-settings");
        await me.click("Remove project", "nav.pj-subnav button");
        await me.click("Remove project", "button.danger");
        const confirm = await me.waitFor("the remove confirmation", (text) => /Remove plant-shop from CrazyAgent\?/.test(text));
        assert.match(confirm, /folder stays on disk/, "the confirmation says the folder stays on disk");
        await me.click("Remove project", "dialog button");
      };
      await remove();
      await me.waitFor("why the project stays while its master is open", (text) => text.includes("End this project's master conversation first, then remove the project."));
      assert.equal(log.answers.get("desktop_remove_project")?.value?.rejection, "project_master_open", "the engine refused the removal while the master conversation is open");
      await me.capture("remove-refused");

      await me.follow("#tab-conversation");
      await me.click("Conversation actions");
      await me.click("End conversation", "[role=menuitem]");
      await me.waitFor("the master conversation ended", (text) => /\bStopped\b/.test(text));
      await me.capture("master-ended");

      await remove();
      await me.waitFor("Home with no projects again", (text) => /No projects yet/.test(text));
      await me.capture("home-empty-again");
      assert.ok(existsSync(join(repo, ".git")) && existsSync(join(repo, "README.md")), "the folder is still on disk");
      assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "no page errors");
    }

    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page called reached the real bridge");
    t.diagnostic(`native-only commands the page called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every command's arguments");
    assert.deepEqual(log.dropped, [], "the bridge answered every call");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(log.trail, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
