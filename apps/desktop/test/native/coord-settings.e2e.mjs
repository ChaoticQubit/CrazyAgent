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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const FIXTURE = readFileSync(new URL("../../../../crates/crazyagent/tests/fixtures/mcp_fixture.py", import.meta.url), "utf8");
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TOOL = "inspect_work";
const SECTIONS = '[role=radiogroup][aria-label="Plugins and skills"] [role=radio]';
const MENUS = "button[aria-haspopup=menu]";
const EVERY = "On for every project";
const HERE = "On in this project";
const SWITCHES = `[...document.querySelectorAll('[role=switch][id^="tool-"]')]`;
const ROWS = `${SWITCHES}.map((button) => [button.closest(".item-row")?.querySelector(".mono")?.innerText.trim() ?? null,
  button.getAttribute("aria-checked"), document.getElementById(button.getAttribute("aria-labelledby"))?.innerText.trim() ?? null])`;
const HARNESSES = ["claude-code", "codex", "rcli"];
const DEFAULT_COUNTED = ["start_work", "inspect_work", "update_work", "transfer_work", "finish_work", "ctx_*", "graphify", "Skill"];
const COUNTED = ["start_work", "ctx_*", "review_notes"];

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Tool shop\n\nKeeps the tools.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to a browser page, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps, given the project, the bridge,
 * the owner on the page, the page, a route opener, a page reload, the team status reader and
 * the test's root folder. Output: none. */
async function onEngine(t, prefix, steps) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? join(process.env.E2E_EVIDENCE, prefix) : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "tool-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  ownerRepo(repo);
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
    const status = () => value(bridge, "desktop_team_status", { id: project });

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
        let visits = 0;
        const clean = async () => assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
        const open = async (hash, what, check) => {
          await clean();
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${hash}` });
          await me.waitFor(what, check);
        };
        const reload = async (words) => {
          await clean();
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(words)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, `the reloaded page never showed ${words}`);
          }
        };
        await steps({ project, bridge, me, page, open, reload, status, root });
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
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Clicks the switch of the tool row that names a tool, as the owner does. Inputs: the page,
 * the owner and the tool. Output: none. */
async function flip(page, me, tool) {
  const id = await page.evaluate(`${SWITCHES}.find((button) => button.closest(".item-row")?.querySelector(".mono")?.innerText.trim() === ${JSON.stringify(tool)})?.id ?? null`);
  assert.ok(id, `no switch for ${tool} on the page`);
  await me.follow(`[id=${JSON.stringify(id)}]`);
}

/** Picks one choice of a select on the Agents tab, as the owner's pick in its list does: the
 * select takes the value and tells the page it changed. Inputs: the page, the select's id
 * and the choice's value. Output: none. */
async function choose(page, id, choice) {
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(id)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(choice)} && !option.disabled)) return false;
    select.focus();
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(select, ${JSON.stringify(choice)});
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${choice} to pick in ${id}`);
}

test("a built-in tool off for every project and on in one project keeps both, Delete and Restore defaults bring it back, and a checked MCP server's tool switches", { timeout: 600000 }, (t) => onEngine(t, "ca-tools-", async ({ project, bridge, me, page, open, root }) => {
  const held = async (projectId, serverId, tool) => {
    const row = (await value(bridge, "desktop_tool_switches", { projectId, serverId })).find((item) => item.tool === tool);
    return row ? [row.global, row.here] : null;
  };
  const row = async (tool) => (await page.evaluate(ROWS)).find((item) => item[0] === tool) ?? null;
  const globalTools = async () => {
    await open("#/settings/plugins", "the global Plugins and skills sections", (text) => ["Skills", "MCP servers", "Hooks", "Tools"].every((word) => text.includes(word)));
    await me.click("Tools", SECTIONS);
    await me.waitFor("the global Tools section", (text) => text.includes("Tools every agent can call.") && text.includes("CrazyAgent tools"));
  };

  await globalTools();
  await until(`${TOOL} on for every project in the engine`, ["on", null], () => held(null, null, TOOL));
  await until(`${TOOL} shown on for every project`, [TOOL, "true", EVERY], () => row(TOOL));
  await me.capture("tools-global-start");
  await flip(page, me, TOOL);
  await until(`${TOOL} off for every project in the engine`, ["off", null], () => held(null, null, TOOL));
  await until(`${TOOL} shown off for every project`, [TOOL, "false", EVERY], () => row(TOOL));

  await open(`#/projects/${encodeURIComponent(project)}/settings`, "the project's settings", (text) => text.includes("Plugins and skills"));
  await me.click("Plugins and skills", 'nav[aria-label="Project settings"] button');
  await me.waitFor("the project's Plugins and skills", (text) => text.includes("What you add here applies to this project only."));
  await me.click("Tools", SECTIONS);
  await me.waitFor("the project's Tools section", (text) => text.includes("Tools agents in this project can call."));
  await until(`${TOOL} following the every-project Off in the project`, [TOOL, "false", HERE], () => row(TOOL));
  await flip(page, me, TOOL);
  await until(`${TOOL} on in the project and still off for every project in the engine`, ["off", "on"], () => held(project, null, TOOL));
  await until(`${TOOL} shown on in the project`, [TOOL, "true", HERE], () => row(TOOL));
  await me.capture("tools-project-on");

  await globalTools();
  await until(`${TOOL} still shown off for every project`, [TOOL, "false", EVERY], () => row(TOOL));
  assert.deepEqual(await held(null, null, TOOL), ["off", null], "the project's switch leaves the every-project switch Off");

  await me.click(`More for ${TOOL}`, MENUS);
  await me.click("Delete", "[role=menuitem]");
  await until(`${TOOL} deleted for every project in the engine`, ["hidden", null], () => held(null, null, TOOL));
  await until(`${TOOL} gone from the switches`, null, () => row(TOOL));
  await me.waitFor("the deleted count", (text) => text.includes("1 deleted. Restore defaults brings it back."));
  await me.capture("tools-deleted");
  await me.click("Restore defaults", "button");
  await until(`${TOOL} back on for every project in the engine`, ["on", null], () => held(null, null, TOOL));
  await until(`${TOOL} shown on for every project again`, [TOOL, "true", EVERY], () => row(TOOL));
  await until("no Restore defaults once nothing is stored", false, () => page.evaluate(`[...document.querySelectorAll("button")].some((button) => button.innerText.trim() === "Restore defaults")`));
  assert.deepEqual(await held(project, null, TOOL), ["on", "on"], "Restore defaults for every project keeps the project's own switch");

  const command = join(root, "servers", "mcp_fixture.py");
  mkdirSync(join(root, "servers"), { recursive: true, mode: 0o700 });
  writeFileSync(command, FIXTURE, { mode: 0o700 });
  const spec = { kind: "stdio", command, args: [] };
  const added = await value(bridge, "desktop_add_mcp_server", { scope: { kind: "global" }, name: "fixture", spec, secrets: [] });
  assert.equal((await value(bridge, "desktop_approve_mcp_server", { id: added.id, spec })).status, "approved", "the owner approves the fixture server");
  await open("#/settings/plugins", "the global Plugins and skills sections", (text) => ["Skills", "MCP servers", "Hooks", "Tools"].every((word) => text.includes(word)));
  await me.click("MCP servers", SECTIONS);
  await me.waitFor("the fixture server", (text) => text.includes("fixture"));
  await me.click("More for fixture", MENUS);
  await me.click("Check", "[role=menuitem]");
  await me.waitFor("the check's answer", (text) => text.includes("fixture answered with 2 tools."));
  await me.click("2 tools", "button");
  await until("the fixture's checked tools shown on for every project", [["echo", "true", EVERY], ["anonymous", "true", EVERY]], () => page.evaluate(ROWS));
  await me.capture("tools-server-expanded");
  await flip(page, me, "echo");
  await until("echo off for every project in the engine", ["off", null], () => held(null, added.id, "echo"));
  await until("the fixture's echo shown off", [["echo", "false", EVERY], ["anonymous", "true", EVERY]], () => page.evaluate(ROWS));
  assert.deepEqual(await held(null, added.id, "anonymous"), ["on", null], "switching echo leaves the server's other tool on");
  assert.deepEqual(await held(null, null, TOOL), ["on", null], "a server's switch leaves the built-in tools alone");
  await me.capture("tools-server-echo-off");
}));

test("Agents tab coordination: each harness's mode and delivery, questions, overlaps, nudges and counted tools are kept and shown after a reload, and Use defaults resets only the list", { timeout: 600000 }, (t) => onEngine(t, "ca-coord-", async ({ project, me, page, open, reload, status }) => {
  const mode = (harness) => `tg-mode-${harness}-${project}`;
  const delivery = (harness) => `tg-delivery-${harness}-${project}`;
  const counted = `tg-counted-${project}`;
  const radios = (group) => `[role=radiogroup][aria-label="${group}"] [role=radio]`;
  const kept = async () => {
    const view = (await status()).coordination;
    assert.ok(view, "the engine sends the coordination settings");
    return {
      harnesses: view.harnesses.map((item) => [item.harness, item.mode, item.delivery]),
      questions: view.settings.questions_to, overlaps: view.settings.overlaps, nudges: view.settings.nudges, counted: view.settings.counted_tools,
    };
  };
  const shown = () => page.evaluate(`(() => {
    const field = (id) => document.getElementById(id)?.value ?? null;
    const radio = (group) => document.querySelector('[role=radiogroup][aria-label="' + group + '"] [role=radio][aria-checked="true"]')?.innerText.trim() ?? null;
    return {
      harnesses: ${JSON.stringify(HARNESSES.map((harness) => [harness, mode(harness), delivery(harness)]))}.map(([harness, modeId, deliveryId]) => [harness, field(modeId), field(deliveryId)]),
      questions: radio("Sub-agent questions go to"), overlaps: radio("Overlapping edits"), nudges: radio("Workflow nudges"), counted: field(${JSON.stringify(counted)}),
    };
  })()`);
  const engineWords = { questions: { lead: "Their lead", me: "Me directly" }, overlaps: { warn: "Warn", block: "Block", off: "Off", wait: "Wait" }, nudges: { off: "Off", ten_minutes: "Every 10 minutes", first_edit: "First edit" } };
  const both = async (what, wanted) => {
    await until(`the engine keeping ${what}`, wanted, kept);
    const words = { ...wanted, questions: engineWords.questions[wanted.questions], overlaps: engineWords.overlaps[wanted.overlaps], nudges: engineWords.nudges[wanted.nudges], counted: wanted.counted.join("\n") };
    await until(`the Agents tab showing ${what}`, words, shown);
  };

  await open(`#/projects/${encodeURIComponent(project)}/agents`, "the Coordination settings on the Agents tab", (text) => text.includes("Coordination") && text.includes("Counted tools"));
  const wanted = {
    harnesses: [["claude-code", "app_spawned", "auto"], ["codex", "app_spawned", "auto"], ["rcli", "app_spawned", "auto"]],
    questions: "lead", overlaps: "warn", nudges: "off", counted: DEFAULT_COUNTED,
  };
  await both("the plan defaults", wanted);
  await me.capture("coord-defaults");

  await choose(page, mode("claude-code"), "native");
  wanted.harnesses[0] = ["claude-code", "native", "auto"];
  await both("Claude Code on native teammates", wanted);
  await choose(page, delivery("codex"), "typing_only");
  wanted.harnesses[1] = ["codex", "app_spawned", "typing_only"];
  await both("Codex messages by typing only", wanted);
  await me.click("Me directly", radios("Sub-agent questions go to"));
  wanted.questions = "me";
  await both("questions going to the owner", wanted);
  await me.click("Block", radios("Overlapping edits"));
  wanted.overlaps = "block";
  await both("overlapping edits blocked", wanted);
  await me.click("Off", radios("Overlapping edits"));
  wanted.overlaps = "off";
  await both("overlapping edits off", wanted);
  await me.click("Wait", radios("Overlapping edits"));
  wanted.overlaps = "wait";
  await both("overlapping starts waiting", wanted);
  await me.click("Warn", radios("Overlapping edits"));
  wanted.overlaps = "warn";
  await both("overlapping edits warned again", wanted);
  await me.click("Every 10 minutes", radios("Workflow nudges"));
  wanted.nudges = "ten_minutes";
  await both("nudges every 10 minutes", wanted);
  await me.type(`[id=${JSON.stringify(counted)}]`, COUNTED.join(", "));
  await me.click("Save", 'form[aria-label="Counted tools"] button');
  wanted.counted = COUNTED;
  await both("the owner's counted tools", wanted);

  await reload("Counted tools");
  await both("every choice after a reload", wanted);
  await me.capture("coord-after-reload");

  await me.click("Off", radios("Workflow nudges"));
  wanted.nudges = "off";
  await both("nudges off", wanted);
  await me.click("Use defaults", 'form[aria-label="Counted tools"] button');
  wanted.counted = DEFAULT_COUNTED;
  await both("the default counted tools with every other choice kept", wanted);
  await reload("Counted tools");
  await both("the defaults list and every other choice after a second reload", wanted);
  await me.capture("coord-use-defaults");
}));
