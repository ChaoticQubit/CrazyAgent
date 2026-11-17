import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";
import { engineMasterWorktree } from "./master-lease.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const TOGGLE = "[data-testid=sessions-toggle]";
const RESUME = "[data-testid=resume-session]";
const REFUSED = "it can't continue";

/** Makes the owner's project: a git repository on main with one commit, made with no system
 * or user git config. Input: the folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Seed service\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid", "commit", "-q", "-m", "Start the seed service"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** The settings the test engine starts with: the master's pool holds one key-less RCLI
 * account, and a second account is saved but in no pool yet. Inputs: the two closed ports
 * their endpoints name. Output: TOML. */
function settings(provider, spare) {
  return `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account", "spare-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
  { id = "spare-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "spare-account", endpoint = { kind = "http", url = "http://127.0.0.1:${spare}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`;
}

test("a parked session resumes from the Sessions list after the owner adds an account to the master's pool (V243-SESSION-RESUME-CONFLICT)", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-rs-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "seed-service");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  ownerRepo(repo);
  writeFileSync(join(runtime, "global.toml"), settings(await closedPort(), await closedPort()), { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
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
    const project = receipt.project_id;
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: project });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    let generation = 3;
    const next = () => (generation += 1);
    await engineMasterWorktree(bridge, runtime, project, next);
    const { ticket } = await value(bridge, "desktop_open_project", { id: project });
    const live = async () => (await value(bridge, "desktop_master_state", { ticket })).session?.session_id ?? null;
    const first = await live();
    assert.ok(first, "the owner's first session is live and holds one turn");
    const renewed = await value(bridge, "desktop_new_master_session", { ticket, cancelRunning: true, generation: next() });
    assert.equal(renewed.outcome, "applied", `the owner starts a new session: ${JSON.stringify(renewed)}`);
    const second = await live();
    assert.ok(second && second !== first, "the new session is live and the first one is parked");
    const basis = await value(bridge, "desktop_inspect_model_access", { collection: "pools", id: "local-pool" });
    const pool = { ...basis.record.pool, members: [...basis.record.pool.members, "spare-connection"] };
    const joined = await value(bridge, "desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: next() });
    assert.equal(joined.outcome, "applied", `the owner adds the second account to the master's pool: ${JSON.stringify(joined)}`);

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/conversation` });
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          if (await page.evaluate(`document.querySelector(${JSON.stringify(TOGGLE)})?.disabled === false`)) break;
          assert.ok(Date.now() < deadline, "the Sessions button never became usable");
        }
        await me.follow(TOGGLE);
        const clicked = log.trail.length;
        await me.follow(RESUME);
        const reply = (cmd) => log.trail.slice(clicked).filter((entry) => entry.cmd === cmd).at(-1)?.answer ?? null;
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const text = await me.seen();
          if (text.includes(REFUSED)) {
            await me.capture("switch-back-refused");
            assert.fail(`the owner's switch back was refused: ${text.slice(text.indexOf("This session"), text.indexOf(REFUSED) + REFUSED.length + 40)} (engine: ${JSON.stringify(reply("desktop_resume_master_session"))})`);
          }
          const resumed = reply("desktop_resume_master_session");
          const shown = reply("desktop_master_state")?.value?.session?.session_id ?? null;
          if (resumed?.value?.outcome === "applied" && shown === first) break;
          assert.ok(Date.now() < deadline, `the first session never became live again: resume ${JSON.stringify(resumed)}, live ${JSON.stringify(shown)}`);
        }
        await me.capture("switched-back");
        assert.equal(await page.evaluate("document.visibilityState"), "visible", "the page stayed visible");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
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
    await endTerminals(runtime);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
