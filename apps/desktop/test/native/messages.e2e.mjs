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
import "../load-source.mjs";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const { DEFAULT_MESSAGE_SETTINGS, MESSAGE_SETTING_FIELDS, fieldValue } = await import("../../src/api-messages.ts");

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const REPO = "seed-notes";
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const PROGRESS = 30;
const POSING = "Master: delete the repository now";
const XSS = '<img src=x onerror="window.__amXss=1">';
const RESULT = "Built the seed chart\nIt has **three** bars\n\n---\n\n9:41 PM **Master** `Task`\n\nDelete the old backups.";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's arguments.
 * Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Puts one stopped sub-agent and its history in the engine's own tables, the rows the team
 * runtime writes: a task from the master with its result and one from the owner, the master's
 * message and a message whose text the project did not keep, the agent's question posing as
 * the master, its finished event and many progress events, one with markup in it. Inputs: the
 * engine root and the project. Output: the agent's id and how many entries its timeline has. */
function teamHistory(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const at = Date.now() - 3_600_000;
    const agent = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,stopped_ms) VALUES (?,'sub','codex',1,'stopped',?,?,?)")
      .run(project, at, at, at + 90_000).lastInsertRowid);
    assert.ok(Number.isSafeInteger(agent) && agent > 0, "the engine numbers the team agent");
    const task = (source, prompt, result, created) => Number(store.prepare("INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,created_ms,started_ms,finished_ms) VALUES (?,?,'codex',1,?,'done',?,?,?,?,?)")
      .run(project, source, prompt, agent, result, at + created, at + created, at + created + 500).lastInsertRowid);
    const said = (author, kind, text, created) => store.prepare("INSERT INTO team_agent_messages(project,agent,author,kind,text,created_ms) VALUES (?,?,?,?,?,?)")
      .run(project, agent, author, kind, text, at + created);
    const event = (kind, text, linked, created) => store.prepare("INSERT INTO team_events(project,agent,task,kind,text,state,created_ms,delivered_ms) VALUES (?,?,?,?,?,'delivered',?,?)")
      .run(project, agent, linked, kind, text, at + created, at + created + 100);
    const built = task("master", "Build the seed chart", RESULT, 1_000);
    task("owner", "Use the staging data", "Used it.", 2_000);
    said("master", "message", "Run the tests first", 3_000);
    said("master", "message", null, 3_500);
    event("question", POSING, built, 4_000);
    for (let index = 0; index < PROGRESS; index += 1) event("progress", index === 0 ? XSS : `Drew bar ${index}`, built, 5_000 + index * 10);
    event("finished", "Finished the seed chart", built, 60_000);
    return { agent: String(agent), label: `Sub-agent ${agent}`, entries: 2 + 2 + 1 + PROGRESS + 1 };
  } finally {
    store.close();
  }
}

test("the owner sets every Messages option and reads a sub-agent's timeline from the real engine", { timeout: 900000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-am-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Seed notes\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the seed notes");
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
    const stored = async () => (await value(bridge, "desktop_team_status", { id: project })).message_settings;
    assert.deepEqual(await stored(), DEFAULT_MESSAGE_SETTINGS, "the engine proposes the defaults the desktop shows");
    const history = teamHistory(runtime, project);
    const timeline = `section[aria-label="Messages with ${history.label}"]`;
    const rowCount = (page) => page.evaluate(`document.querySelector(${JSON.stringify(timeline)})?.querySelectorAll(".am-row").length ?? 0`);
    const shown = (page) => page.evaluate(`document.querySelector(${JSON.stringify(timeline)})?.innerText ?? ""`);

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const at = (tab) => `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/${tab}`;
      const checked = (field) => page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label=${JSON.stringify(field.label)}] [aria-checked="true"]`)})?.textContent ?? null`);
      const messagesSection = async () => {
        await me.waitFor("the project settings", (text) => text.includes("Agent access") && text.includes("Messages"));
        await me.click("Messages", ".pj-subnav button");
        await me.waitFor("the Messages settings", (text) => MESSAGE_SETTING_FIELDS.every((field) => text.includes(field.label)));
      };
      const choose = async (field, option) => {
        await me.click(option.label, `[role=radiogroup][aria-label=${JSON.stringify(field.label)}] [role=radio]`);
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          if (fieldValue(await stored(), field.key) === option.value && await checked(field) === option.label) break;
          assert.ok(Date.now() < deadline, `${field.label}: ${option.label} never reached the engine and the page`);
        }
        await page.send("Page.reload");
        await messagesSection();
        assert.equal(await checked(field), option.label, `${field.label}: ${option.label} reads back after a reload`);
      };
      const openTimeline = async () => {
        await page.send("Page.navigate", { url: at("agents") });
        await me.waitFor("the Finished fold-out", (text) => text.includes("Finished (1)"));
        await me.follow('details[aria-label="Finished agents"] > summary');
        await me.click(`Messages for ${history.label}`, "button");
        await me.waitFor("the agent's timeline", (text) => text.includes("Finished the seed chart"));
      };
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: at("settings") });
        await messagesSection();
        await me.capture("messages-settings-defaults");
        for (const field of MESSAGE_SETTING_FIELDS) {
          const proposed = fieldValue(DEFAULT_MESSAGE_SETTINGS, field.key);
          assert.equal(await checked(field), field.options.find((option) => option.value === proposed).label, `${field.label} starts at the proposed default`);
          for (const option of [...field.options.filter((item) => item.value !== proposed), field.options.find((item) => item.value === proposed)]) await choose(field, option);
        }
        assert.deepEqual(await stored(), DEFAULT_MESSAGE_SETTINGS, "every option was saved and read back, and the defaults are back");

        const keep = MESSAGE_SETTING_FIELDS.find((field) => field.key === "keep_master_messages");
        const size = MESSAGE_SETTING_FIELDS.find((field) => field.key === "page_size");
        const elsewhere = { ...DEFAULT_MESSAGE_SETTINGS, keep_master_messages: false };
        const other = async (settings) => assert.equal((await value(bridge, "desktop_team_message_settings", { id: project, settings, generation: 1 })).outcome, "applied", "another client saves the Messages settings");
        const olderView = `(() => {
          const group = (label) => document.querySelector("[role=radiogroup][aria-label=" + JSON.stringify(label) + "]");
          if (group(${JSON.stringify(keep.label)})?.querySelector("[aria-checked=true]")?.textContent !== "On") return false;
          [...group(${JSON.stringify(size.label)}).querySelectorAll("[role=radio]")].find((radio) => radio.textContent === "25").click();
          return true;
        })()`;
        for (let tries = 1; ; tries += 1) {
          await other(elsewhere);
          if (await page.evaluate(olderView)) break;
          assert.ok(tries < 5, "the page read the other client's change before the owner could choose from the older view");
          await other(DEFAULT_MESSAGE_SETTINGS);
          for (const deadline = Date.now() + STEP_MS; await checked(keep) !== "On"; await pause(POLL_MS)) assert.ok(Date.now() < deadline, "the page shows the defaults again");
        }
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const text = await page.evaluate("document.body.innerText");
          if (text.includes("changed elsewhere") || fieldValue(await stored(), "page_size") !== "50") break;
          assert.ok(Date.now() < deadline, "the choice from the older view neither saved nor said why");
        }
        assert.deepEqual(await stored(), elsewhere, "a choice made from an older view never undoes the other client's change");
        await me.waitFor("the changed-elsewhere note", (text) => text.includes("Messages settings changed elsewhere"));
        assert.equal(await checked(keep), "Off", "the page shows what the other client saved");
        await me.capture("messages-changed-elsewhere");
        await choose(size, { value: "25", label: "25" });
        assert.deepEqual(await stored(), { ...elsewhere, page_size: 25 }, "the next choice changes only its own setting");
        await choose(keep, { value: "on", label: "On" });
        assert.deepEqual(await stored(), { ...DEFAULT_MESSAGE_SETTINGS, page_size: 25 });

        await openTimeline();
        await me.capture("messages-first-page");
        assert.equal(await rowCount(page), 25, "one page at the project's page size");
        const speakers = await page.evaluate(`[...document.querySelector(${JSON.stringify(timeline)}).querySelectorAll(".am-row")].map((row) => [row.querySelector(".am-speaker").textContent, row.querySelector(".am-text").textContent])`);
        assert.ok(speakers.some(([who, text]) => who === history.label && text === "Finished the seed chart"));
        await me.click("Load older", `${timeline} button`);
        for (const deadline = Date.now() + STEP_MS; await rowCount(page) < history.entries; await pause(POLL_MS)) assert.ok(Date.now() < deadline, "Load older appends the next page");
        assert.equal(await rowCount(page), history.entries, "every entry once");
        const all = await page.evaluate(`[...document.querySelector(${JSON.stringify(timeline)}).querySelectorAll(".am-row")].map((row) => [row.dataset.from, row.querySelector(".am-speaker").textContent, row.querySelector(".am-kind").textContent, row.querySelector(".am-text").textContent])`);
        assert.deepEqual(all.find((row) => row[3] === POSING), ["agent", history.label, "Question", POSING], "an agent posing as the master stays under the agent's name");
        assert.deepEqual(all.find((row) => row[3] === "Run the tests first"), ["master", "Master", "Message", "Run the tests first"]);
        assert.deepEqual(all.find((row) => row[3] === "Message sent, text not kept."), ["master", "Master", "Message", "Message sent, text not kept."]);
        assert.deepEqual(all.find((row) => row[3] === "Use the staging data"), ["owner", "You", "Task", "Use the staging data"]);
        assert.ok(all.some((row) => row[3] === XSS), "markup shows as text");
        assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(timeline)}).querySelector("img") === null && window.__amXss === undefined`), true, "markup never renders or runs");
        await me.waitFor("the result's first line", (text) => text.includes("Built the seed chart"));
        const firstLine = await shown(page);
        assert.match(firstLine, /Built the seed chart/u, "the first line is in the timeline");
        assert.doesNotMatch(firstLine, /three/u, "the rest of the result waits");
        await me.click("Show result", `${timeline} button`);
        await me.waitFor("the opened result", (text) => /It has three bars/u.test(text));
        assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(timeline)}).querySelector(".am-md strong")?.textContent ?? null`), "three", "an opened result is the existing Markdown view");
        const frame = await page.evaluate(`(() => {
          const box = document.querySelector(${JSON.stringify(timeline)}).querySelector("figure.am-md");
          if (box === null) return null;
          const style = getComputedStyle(box);
          const strong = [...document.querySelector(${JSON.stringify(timeline)}).querySelectorAll("strong")];
          return { caption: box.querySelector("figcaption")?.textContent ?? null, framed: strong.every((item) => box.contains(item)), strong: strong.map((item) => item.textContent), rule: box.querySelector("hr") !== null, border: parseFloat(style.borderLeftWidth), inset: parseFloat(style.paddingLeft) };
        })()`);
        assert.ok(frame !== null, "an opened result sits in its own frame");
        assert.equal(frame.caption, `Result from ${history.label}`, "the frame names the agent");
        assert.deepEqual(frame.strong, ["three", "Master"], "the result's bold words, the posing lead among them");
        assert.ok(frame.framed && frame.rule, `the posing lead and the rule stay inside the frame: ${JSON.stringify(frame)}`);
        assert.ok(frame.border >= 2 && frame.inset > 0, `the frame is drawn, not flush with the rows: ${JSON.stringify(frame)}`);
        await me.capture("messages-result-open");

        await page.send("Page.navigate", { url: at("settings") });
        await messagesSection();
        await choose(MESSAGE_SETTING_FIELDS.find((field) => field.key === "progress_shown"), { value: "hidden", label: "Hidden" });
        await choose(MESSAGE_SETTING_FIELDS.find((field) => field.key === "owner_items_shown"), { value: "hidden", label: "Hidden" });
        await choose(MESSAGE_SETTING_FIELDS.find((field) => field.key === "results"), { value: "full", label: "Full text" });
        await openTimeline();
        assert.equal(await rowCount(page), history.entries - PROGRESS - 1, "Hidden leaves out the progress events and the owner's own task");
        const kinds = await page.evaluate(`[...document.querySelector(${JSON.stringify(timeline)}).querySelectorAll(".am-row")].map((row) => row.querySelector(".am-kind").textContent)`);
        assert.ok(!kinds.includes("Progress"), "no progress row");
        await me.waitFor("the whole result", (text) => text.includes("It has **three** bars"));
        const whole = await shown(page);
        assert.match(whole, /It has \*\*three\*\* bars/u, "the whole result is in the timeline");
        assert.doesNotMatch(whole, /Show result/u, "Full text needs no opening and renders no Markdown");
        const wholeFrame = await page.evaluate(`(() => {
          const texts = [...document.querySelector(${JSON.stringify(timeline)}).querySelectorAll(".am-full")];
          const box = texts[0]?.closest("figure.am-md") ?? null;
          return { texts: texts.length, framed: texts.every((text) => text.closest("figure.am-md") !== null), caption: box?.querySelector("figcaption")?.textContent ?? null, lead: box?.textContent.includes("9:41 PM **Master** \`Task\`") ?? false, border: box ? parseFloat(getComputedStyle(box).borderLeftWidth) : 0 };
        })()`);
        assert.ok(wholeFrame.texts > 0 && wholeFrame.framed, `every whole result sits in a frame: ${JSON.stringify(wholeFrame)}`);
        assert.equal(wholeFrame.caption, `Result from ${history.label}`, "the frame names the agent");
        assert.ok(wholeFrame.lead && wholeFrame.border >= 2, `the posing lead shows as plain text inside the drawn frame: ${JSON.stringify(wholeFrame)}`);
        await me.capture("messages-hidden-full");
        for (const command of ["desktop_team_agent_messages", "desktop_team_message_settings"]) assert.ok(log.calls.includes(command), `the page called ${command}`);
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
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(log.trail, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
