import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETTLE_MS = 180000;
const REPO = "plant-shop";
const ABOUT = "A small shop that sells house plants online.";

/** Reads the phase of every master conversation the engine recorded, oldest first.
 * Input: the engine database path. Output: the phases, read without writing. */
function masterPhases(database) {
  const db = new DatabaseSync(database, { readOnly: true });
  try {
    return db.prepare("SELECT phase FROM master_sessions ORDER BY created").all().map((row) => row.phase);
  } finally {
    db.close();
  }
}

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
    const skillFolder = join(root, "skills", "graphify");
    mkdirSync(skillFolder, { recursive: true });
    writeFileSync(join(skillFolder, "SKILL.md"), "---\nname: graphify\ndescription: Answers questions about the code from the project's code graph.\n---\n\nRead graphify-out/GRAPH_REPORT.md before the source.\n");
    const added = await value(bridge, "desktop_add_skill", { scope: { kind: "global" }, folder: skillFolder });
    assert.equal(added.name, "graphify", `the plugin store holds a Graphify skill, where Setup installs it: ${JSON.stringify(added)}`);
    const graphSkill = (await value(bridge, "desktop_setup_status")).pack.find((entry) => entry.id === "graphify");
    assert.equal(graphSkill?.state.state, "on", `Setup reads the Graphify skill as installed and on (LB-3): ${JSON.stringify(graphSkill)}`);

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
        assert.match(steps, /Build the code graph\s*Done/, "the Graphify skill is on and the engine found its command, so the graph step is done");
      } else {
        assert.doesNotMatch(steps, /Build the code graph\s*Done/, "the engine reports the Graphify command missing, so the graph step cannot claim done");
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
      await me.waitFor("the master conversation", (text) => text.includes("/ for commands, @ to tag"));
      await me.type(".cs textarea", "Hello master, what is in this repository?");
      await me.enter();
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
        if (!last.reason) return text.includes(words[last.phase] ?? "The master stopped");
        const code = last.cause ?? (last.reason !== "unknown" ? last.reason : last.phase);
        return text.includes(`(${code})`);
      };
      await me.waitFor("the page showing the engine's own turn state", agrees, STEP_MS);

      const remove = async () => {
        await me.follow("#tab-settings");
        await me.click("Remove project", "nav.pj-subnav button");
        await me.click("Remove project", "button.danger");
        const confirm = await me.waitFor("the remove confirmation", (text) => /Remove plant-shop from CrazyAgent\?/.test(text));
        assert.match(confirm, /folder stays on disk/, "the confirmation says the folder stays on disk");
        assert.match(confirm, /master conversation ends/, "the confirmation says removing ends the open master conversation");
        await me.click("Remove project", "dialog button");
      };
      const database = join(runtime, "engine.db");
      await me.click("Conversation actions");
      await me.click("End conversation", "[role=menuitem]");
      await me.waitFor("the master conversation ended", (text) => /\bStopped\b/.test(text));
      await me.capture("master-ended");
      await me.type(".cs textarea", "Hello again, master.");
      await me.enter();
      await me.waitFor("a new master conversation", (text) => text.includes("Hello again, master.") && line.test(text), STEP_MS);
      assert.deepEqual(masterPhases(database), ["closed", "configured"], "End conversation closed the first conversation and the next message opened a new one");
      const closes = () => log.trail.filter((entry) => entry.cmd === "desktop_close_master").length;
      const ended = closes();

      await remove();
      await me.waitFor("Home with no projects again", (text) => /No projects yet/.test(text));
      await me.capture("home-empty-again");
      assert.equal(log.answers.get("desktop_remove_project")?.value?.outcome, "applied", `one Remove project removes the project with its conversation open: ${JSON.stringify(log.answers.get("desktop_remove_project") ?? null)}`);
      assert.equal(closes(), ended, "Remove project made no separate End conversation call");
      assert.deepEqual(masterPhases(database), ["closed", "closed"], "the engine ended the open conversation in the same owner action");
      assert.ok(existsSync(join(repo, ".git")) && existsSync(join(repo, "README.md")), "the folder is still on disk");
      assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "no page errors");
    }

    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page called reached the real bridge");
    t.diagnostic(`native-only commands the page called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every command's arguments");
    assert.deepEqual(log.dropped, [], "the bridge answered every call");
    const busy = log.trail.filter((entry) => entry.answer?.failure?.code === "busy").length;
    t.diagnostic(`busy answers: ${busy} of ${log.trail.length}`);
    assert.equal(busy, 0, `the bridge queues the page's calls instead of refusing them: ${busy} of ${log.trail.length} answers were busy`);
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
