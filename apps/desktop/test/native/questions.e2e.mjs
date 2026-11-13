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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const ALERT_MS = 60000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const QUESTION = "Should the menu list prices in dollars or in euros?";
const ANSWER = "Dollars, with no cents.";
const PERSONA = "Ada";
const HANDLE = "menu-prices";
const PAUSED = "The agent is paused until you answer.";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Writes one sub-agent as the engine keeps it while it rests between turns: idle, so it
 * counts as running and has a terminal name, run by no worker of this engine, with the name
 * the team keeps for it (DEC-096). Inputs: the engine root and the project. Output: its id. */
function restingAgent(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent) VALUES (?,'sub','codex',1,'idle',?,?,1,NULL)")
      .run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    store.prepare("INSERT INTO team_names(agent,project,persona,handle) VALUES (?,?,?,?) ON CONFLICT(agent) DO UPDATE SET project=excluded.project, persona=excluded.persona, handle=excluded.handle")
      .run(id, project, PERSONA, HANDLE);
    return id;
  } finally {
    store.close();
  }
}

/** Writes the stand-in Codex executable the engine is started with. The engine runs its team
 * runtime, whose tick raises question alerts, only when it has a harness executable; this one
 * exits at once and answers nothing, so no model ever runs. Input: the test root. Output: its
 * absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Reads what the engine keeps for one question: the question's receipt, the replies to it
 * and the asker's wait. Inputs: the engine root, the project, the question and the asker.
 * Output: the receipt, the replies and the wait. */
function questionRows(runtime, project, question, agent) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    const asked = store.prepare("SELECT from_kind, from_agent, to_kind, kind, q_for, q_severity, body, receipt FROM coord_messages WHERE project=? AND id=?").get(project, question);
    const replies = store.prepare("SELECT from_kind, from_agent, to_kind, to_agent, kind, body FROM coord_messages WHERE project=? AND reply_to=? ORDER BY id").all(project, question);
    const wait = store.prepare("SELECT waiting FROM coord_activity WHERE agent=?").get(agent);
    return { asked: asked ? { ...asked } : null, replies: replies.map((row) => ({ ...row })), waiting: wait?.waiting ?? null };
  } finally {
    store.close();
  }
}

test("an agent's urgent question toasts, opens the Questions tab and its answer reaches the engine", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-qs-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "menu-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Menu shop\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the menu shop");
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
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", standInHarness(root)],
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
    assert.ok(existsSync(join(runtime, "engine.db")), "the engine keeps its database in its root");
    const agent = restingAgent(runtime, project);
    const graph = await value(bridge, "desktop_team_graph", { id: project });
    const node = graph.nodes.find((item) => item.id === `team-${agent}`) ?? assert.fail(`no card for agent ${agent}: ${JSON.stringify(graph.nodes)}`);
    const label = node.label;
    assert.equal(label, `${PERSONA} (@${HANDLE})`, "the asker has the name the team keeps");
    assert.deepEqual(await value(bridge, "desktop_coord_questions", { id: project }), { project_id: project, questions: [], more: 0, waiting: [] }, "no agent has asked anything yet");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    let question = 0;
    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/overview` });
        await me.waitFor("the project's Overview", (text) => text.includes("Overview") && text.includes("Agents"));
        for (const deadline = Date.now() + STEP_MS; !("value" in (log.answers.get("desktop_team_events") ?? {})); await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, `the app never read the team events: ${JSON.stringify(log.answers.get("desktop_team_events"))}`);
        }
        await me.capture("questions-before");

        const asked = spawnSync(bridgeBinary, ["ask-owner", runtime, project, String(agent), "high", QUESTION], { encoding: "utf8", env: { ...env } });
        assert.equal(asked.status, 0, `the agent's question reaches the engine: ${asked.stderr}`);
        question = Number(asked.stdout.trim());
        assert.ok(Number.isSafeInteger(question) && question > 0, `the engine numbers the question: ${asked.stdout}`);
        assert.deepEqual(questionRows(runtime, project, question, agent).asked,
          { from_kind: "agent", from_agent: agent, to_kind: "owner", kind: "question", q_for: "me", q_severity: "high", body: QUESTION, receipt: "sent" },
          "the engine keeps the agent's urgent question to the owner");

        const alert = `${label} asks you, urgent: ${QUESTION}`;
        await me.waitFor("the urgent question's toast", (text) => text.includes(alert), ALERT_MS);
        const toast = await page.evaluate("document.querySelector('div.toast[role=status]')?.innerText ?? null");
        assert.ok(toast?.includes(alert) && toast.includes("Open"), `the toast names the asker and offers Open: ${toast}`);
        assert.equal(questionRows(runtime, project, question, agent).waiting, "question", "the engine paused the asker until the owner answers");
        await me.capture("questions-toast");
        await me.click("Open", "div.toast[role=status] button");
        await me.waitFor("the question on the Questions tab Open opened", (text) => text.includes(QUESTION) && text.includes("Your answer"));
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const focus = await page.evaluate(`document.activeElement?.closest("section.qs-card")?.innerText ?? null`);
          if (focus?.includes(QUESTION)) break;
          assert.ok(Date.now() < deadline, `Open puts the keyboard on the urgent question (V241-QUESTION-UX P2): ${focus}`);
        }
        assert.ok((await page.evaluate("location.hash")).endsWith(`/projects/${encodeURIComponent(project)}/questions`), "Open goes to the project's Questions tab, where a linked team question waits (S4-R1), and the keyboard goes to it once");
        const cards = await page.evaluate(`[...document.querySelectorAll("section.qs-card")].map((card) => ({ name: card.getAttribute("aria-label"), text: card.innerText }))`);
        assert.equal(cards.length, 1, `one question card: ${JSON.stringify(cards)}`);
        assert.equal(cards[0].name, `Question from ${label}`, "the card names the agent that asked by its name (V241-QUESTION-UX P1)");
        for (const part of [label, "Urgent", "Waiting for you", PAUSED, QUESTION, "Your answer", "Answer", "Dismiss"]) {
          assert.ok(cards[0].text.includes(part), `the question card shows "${part}": ${cards[0].text}`);
        }
        await me.capture("questions-inbox");

        await me.type("section.qs-card textarea", ANSWER);
        await me.click("Answer", "section.qs-card button");
        await me.waitFor("the answered question", (text) => text.includes(`Answer: ${ANSWER}`) && !text.includes("Your answer"));
        const answered = await page.evaluate(`document.querySelector("section.qs-card")?.innerText ?? ""`);
        assert.ok(!answered.includes("Urgent") && !answered.includes(PAUSED), `an answered question holds neither mark (V241-QUESTION-UX P3): ${answered}`);
        await me.capture("questions-answered");
        const sent = log.trail.filter((call) => call.cmd === "desktop_answer_question");
        assert.equal(sent.length, 1, `one answer was sent: ${JSON.stringify(sent)}`);
        assert.equal(sent[0].args.comment, ANSWER, "the answer carries the owner's words");
        assert.ok("value" in sent[0].answer, `the engine took the answer: ${JSON.stringify(sent[0].answer)}`);
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });

    assert.deepEqual(await value(bridge, "desktop_coord_questions", { id: project }), { project_id: project, questions: [], more: 0, waiting: [] }, "the engine lists no open question after the answer");
    const rows = questionRows(runtime, project, question, agent);
    assert.equal(rows.asked.receipt, "replied", "the answered question is closed");
    assert.deepEqual(rows.replies, [{ from_kind: "owner", from_agent: null, to_kind: "agent", to_agent: agent, kind: "reply", body: ANSWER }], "the engine keeps the owner's answer as the one reply to the asker");
    for (const deadline = Date.now() + STEP_MS; questionRows(runtime, project, question, agent).waiting !== null; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `answering lifts the pause the question set, but the asker still waits on: ${questionRows(runtime, project, question, agent).waiting}`);
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
    await endTerminals(runtime);
  }
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
