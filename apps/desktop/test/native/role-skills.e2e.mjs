import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SKILLS = ["meal-tips", "release-notes"];
const EVERY = SKILLS.join(",");
const WORKER_RADIOS = '[role=radiogroup][aria-label="Worker skills"] [role=radio]';
const WORKER_BOXES = '.pj-row:has([aria-label="Worker skills"]) label.checkbox';

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Writes a skill folder with its SKILL.md, the shape the owner's Add skill takes.
 * Inputs: the parent folder and the skill name. Output: the skill folder. */
function skillFolder(parent, name) {
  const folder = join(parent, name);
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "SKILL.md"), `---\nname: ${name}\ndescription: Teaches ${name}.\n---\n\nSteps for ${name}.\n`);
  return folder;
}

/** Reads the skills per role the engine keeps for a project. Inputs: the engine root and the
 * project. Output: the stored role_skills, or null while no coordination row is saved. */
function storedRoleSkills(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const row = store.prepare("SELECT json FROM coord_settings WHERE project=?").get(project);
    return row ? JSON.parse(row.json).role_skills ?? null : null;
  } finally {
    store.close();
  }
}

test("the owner picks All approved skills, Chosen skills with a tick and Chosen skills with none for workers, and the engine keeps and gives exactly that", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-rs-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "role-shop");
  const homes = join(root, "agent-homes");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, homes]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Role shop\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the role shop");
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
  let project;
  const given = (role) => {
    const agentHome = mkdtempSync(join(homes, `${role}-`));
    const result = spawnSync(bridgeBinary, ["role-skills", runtime, project, role, agentHome], { encoding: "utf8", env: { ...env } });
    assert.equal(result.status, 0, `the engine installs the ${role}'s launch: ${result.stderr}`);
    return result.stdout.trim();
  };
  const settled = async (what, expected) => {
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
      const stored = storedRoleSkills(runtime, project);
      if (JSON.stringify(stored) === JSON.stringify(expected)) return;
      assert.ok(Date.now() < deadline, `the engine never kept ${what}: ${JSON.stringify(stored)}`);
    }
  };
  try {
    ({ child: engine } = await launch(engineBinary,
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
      { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
    assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
    project = receipt.project_id;
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: project });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    for (const name of SKILLS) {
      const added = await value(bridge, "desktop_add_skill", { scope: { kind: "global" }, folder: skillFolder(join(root, "picked"), name) });
      assert.equal(added.name, name, `the engine approves ${name}: ${JSON.stringify(added)}`);
    }
    assert.equal(storedRoleSkills(runtime, project), null, "a new project keeps no coordination row");
    assert.deepEqual(["master", "lead", "worker"].map(given), [EVERY, EVERY, EVERY], "All approved skills is every role's default");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const radios = () => page.evaluate(`[...document.querySelectorAll(${JSON.stringify(WORKER_RADIOS)})].map((radio) => [radio.innerText.trim(), radio.getAttribute("aria-checked")])`);
      const boxes = () => page.evaluate(`[...document.querySelectorAll(${JSON.stringify(WORKER_BOXES)})].map((box) => [box.innerText.trim(), box.querySelector("input").checked, box.querySelector("input").disabled])`);
      const shows = async (what, wanted, read) => {
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const seen = await read();
          if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
          if (Date.now() > deadline) {
            await me.capture(`timeout-${what.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
            assert.fail(`the Agents tab never showed ${what}: ${JSON.stringify(seen)}`);
          }
        }
      };
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=tools` });
        await me.waitFor("the skills per role", (text) => ["Master skills", "Lead skills", "Worker skills"].every((label) => text.includes(label)), STEP_MS);
        await shows("workers on All approved skills", [["All approved skills", "true"], ["Chosen skills", "false"]], radios);
        assert.deepEqual(await boxes(), [], "All approved skills shows no skill boxes");
        await me.capture("role-skills-all");

        await me.click("Chosen skills", WORKER_RADIOS);
        await settled("workers on Chosen skills with every approved skill ticked", { master: null, lead: null, worker: SKILLS });
        await shows("every approved skill ticked for workers", [["meal-tips", true, false], ["release-notes", true, false]], boxes);
        assert.deepEqual([given("master"), given("lead"), given("worker")], [EVERY, EVERY, EVERY], "Chosen skills starts with every approved skill ticked");
        await me.capture("role-skills-chosen-all-ticked");

        await me.click("release-notes", WORKER_BOXES);
        await settled("workers on Chosen skills with one tick", { master: null, lead: null, worker: ["meal-tips"] });
        await shows("only meal-tips ticked for workers", [["meal-tips", true, false], ["release-notes", false, false]], boxes);
        assert.deepEqual([given("master"), given("lead"), given("worker")], [EVERY, EVERY, "meal-tips"], "a worker gets only its ticked skill; the master and lead keep every skill");
        await me.capture("role-skills-chosen-one-tick");

        await me.click("meal-tips", WORKER_BOXES);
        await settled("workers on Chosen skills with no tick", { master: null, lead: null, worker: [] });
        await shows("no skill ticked for workers", [["meal-tips", false, false], ["release-notes", false, false]], boxes);
        assert.deepEqual([given("master"), given("lead"), given("worker")], [EVERY, EVERY, "none"], "a worker with no tick gets no skill");
        await shows("workers still on Chosen skills", [["All approved skills", "false"], ["Chosen skills", "true"]], radios);
        await me.capture("role-skills-chosen-no-tick");

        await me.click("All approved skills", WORKER_RADIOS);
        await settled("workers back on All approved skills", { master: null, lead: null, worker: null });
        await shows("workers back on All approved skills", [["All approved skills", "true"], ["Chosen skills", "false"]], radios);
        await shows("no skill boxes on All approved skills", [], boxes);
        assert.deepEqual([given("master"), given("lead"), given("worker")], [EVERY, EVERY, EVERY], "back on All approved skills, a worker gets every approved skill again");
        await me.capture("role-skills-all-again");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    for (const command of ["desktop_plugin_skills", "desktop_team_coordination"]) {
      assert.ok(log.calls.includes(command), `the page called ${command}: ${JSON.stringify(log.calls)}`);
    }
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    t.diagnostic(`native-only commands the page called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
