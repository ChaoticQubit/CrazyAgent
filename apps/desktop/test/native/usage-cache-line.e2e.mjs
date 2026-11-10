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
const TASK_MS = 120000;
const USAGE_MS = 60000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TASK = "Write fruit.md with one line about apples.";
const HEAD = ["Name", "Tokens", "Input", "Cache write", "Output", "Cache read", "Runs", "Last used"];
const COMPACT = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

/** What Settings, Usage shows for runs that each counted the stand-in's one token line:
 * 1,200 new input, no cache write, 300 output and 8,000 re-read from cache per run, in the
 * page's compact form. Input: the number of runs. Output: { tile, reread, parts, merged },
 * merged being the totals that would wrongly add the re-reads in. */
function expected(runs) {
  assert.ok(Number.isSafeInteger(runs) && runs > 0, `runs counted: ${runs}`);
  const k = (count) => COMPACT.format(count * runs);
  return {
    tile: ["Tokens", k(1500), `${k(1200)} input, 0 cache write, ${k(300)} output`],
    reread: ["Re-read from cache", k(8000), "87% of everything read; billed far lower, not in Tokens"],
    parts: [k(1500), k(1200), "0", k(300), k(8000)],
    merged: [k(9500), k(9200)],
  };
}

/** The Codex stand-in the engine runs every team agent with. It keeps a conversation file as
 * Codex does and answers each turn with "done", after one token count as Codex writes it:
 * 9,200 input tokens of which 8,000 were cached, and 300 output tokens. The engine keeps
 * that as 1,200 new input, 8,000 re-read from cache and 300 output. */
const COUNTING_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/07"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-07T00-00-00-$id.jsonl"
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  /bin/sleep 0.3
  printf '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":9200,"cached_input_tokens":8000,"output_tokens":300,"total_tokens":9500}}}}\n' >> "$f"
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
`;

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Fruit stand\n\nSells fruit.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the fruit stand"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex the engine is started with. Input: the test root. Output: its
 * absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, COUNTING_CODEX, { mode: 0o700 });
  return path;
}

/** Reads the status the engine keeps for the owner's task. Inputs: the engine root and the
 * project. Output: the status, or null before the task exists. */
function taskStatus(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return store.prepare("SELECT status FROM team_tasks WHERE project=? AND prompt=?").get(project, TASK)?.status ?? null;
  } finally {
    store.close();
  }
}

/** Counts the runs the engine recorded tokens for in the project, and those of its team
 * agents: onboarding's jobs run the same stand-in, so they count beside the hand-off. Inputs:
 * the engine root and the project. Output: { runs, team }. */
function runsCounted(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return store.prepare("SELECT COUNT(DISTINCT agent || ' ' || session) AS runs, COUNT(DISTINCT CASE WHEN agent LIKE 'team-%' THEN session END) AS team FROM usage_tokens WHERE project=?").get(project);
  } finally {
    store.close();
  }
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, serves the built app to a browser page on the project's Agents
 * tab, runs the steps and stops everything. Inputs: the test context, the temporary folder
 * prefix and the steps, given the project, the engine root, the owner on the page and the
 * page. Output: none. */
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
  const repo = join(root, "repos", "fruit-stand");
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
    assert.equal((await value(bridge, "desktop_open_project", { id: project })).project_id, project, "the new project opens");
    const pools = await value(bridge, "desktop_model_access_page", { collection: "pools", next: false });
    assert.equal(pools.writable, true, "the test engine's settings are writable");
    const assigned = await value(bridge, "desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project, pool: "local-pool" }, generation: 4 });
    assert.equal(assigned.outcome, "applied", `the project gets the pool its sub-agents run on: ${JSON.stringify(assigned)}`);
    const stored = await value(bridge, "desktop_project_setup", { id: project });
    const subs = [...(stored.team.sub_harnesses ?? []).filter((slot) => slot.harness !== "codex"), { harness: "codex", agents: 1, tools: true }];
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: { ...stored.team, sub_harnesses: subs }, documents: [] };
    const saved = await value(bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: stored.revision, generation: 5 });
    assert.equal(saved.outcome, "applied", `the Codex sub-agent gets its team tools: ${JSON.stringify(saved)}`);
    const kept = (await value(bridge, "desktop_project_setup", { id: project })).team.sub_harnesses;
    assert.ok(kept.some((slot) => slot.harness === "codex" && slot.tools), `the project keeps a Codex sub-agent with team tools: ${JSON.stringify(kept)}`);

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
        await me.unfold("Agents and tasks");
        await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
        await steps({ project, runtime, me, page, site });
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
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Hands the task to a Codex sub-agent from the Agents tab, as the owner does, and waits
 * until the engine finished it. Inputs: the owner on the page, the page, the project and the
 * engine root. Output: the task's final status. */
async function handOff(me, page, project, runtime) {
  await me.unfold("Agents and tasks");
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(`tg-hand-to-${project}`)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === "codex")) return false;
    select.focus();
    select.value = "codex";
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, "no Codex to pick in the Send to list");
  await me.type(`[id="tg-hand-text-${project}"]`, TASK);
  await me.click("Hand off", "form.tg-hand button");
  await me.waitFor("the hand-off note", (text) => text.includes("Task handed off."));
  for (const deadline = Date.now() + TASK_MS; ; await pause(POLL_MS * 5)) {
    const status = taskStatus(runtime, project);
    if (status !== null && !["queued", "needs_pool", "running"].includes(status)) return status;
    assert.ok(Date.now() < deadline, `the engine never finished the task: ${JSON.stringify(status)}`);
  }
}

/** Reads what Settings, Usage shows: the Tokens and Re-read from cache tiles' lines and the
 * By project table's header and first row, each cell's text in screen order. Input: the
 * page. Output: { tile, reread, head, row }, each null while it is missing. */
function usageShown(page) {
  return page.evaluate(`(() => {
    const words = (node) => (node.innerText ?? "").replace(/\\s+/g, " ").trim();
    const tiled = (label) => [...document.querySelectorAll(".usage-tile")].find((node) => words(node.firstElementChild ?? node) === label);
    const tile = tiled("Tokens");
    const reread = tiled("Re-read from cache");
    const panel = [...document.querySelectorAll("section.panel > .panel-head > h2")].find((title) => words(title) === "By project")?.closest("section.panel");
    const table = panel?.querySelector("table") ?? null;
    return {
      tile: tile ? [...tile.children].map(words) : null,
      reread: reread ? [...reread.children].map(words) : null,
      head: table ? [...table.querySelectorAll("thead th")].map(words) : null,
      row: table?.querySelector("tbody tr") ? [...table.querySelector("tbody tr").children].map(words) : null,
    };
  })()`);
}

test("Settings, Usage shows the Tokens headline with its three parts and the cache re-reads on their own tile, and the By project table shows all four parts with the headline first (OS-7)", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-cache-line-", async ({ project, runtime, me, page, site }) => {
    assert.equal(await handOff(me, page, project, runtime), "done", "the Codex sub-agent finished the task");
    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/settings/usage` });
    let shown = null;
    let counted = null;
    let want = null;
    for (const deadline = Date.now() + USAGE_MS; ; await pause(POLL_MS * 20)) {
      counted = runsCounted(runtime, project);
      want = counted.team > 0 ? expected(counted.runs) : null;
      shown = await usageShown(page).catch(() => null);
      if (want && JSON.stringify(shown?.tile) === JSON.stringify(want.tile)) break;
      assert.ok(Date.now() < deadline, `the Tokens tile never showed the counted runs ${JSON.stringify(counted)}: ${JSON.stringify(shown)}`);
      await page.send("Page.reload", { ignoreCache: true });
    }
    await me.capture("usage-cache-line");
    assert.deepEqual(shown.tile, want.tile, "the Tokens tile shows the headline and its input, cache write and output");
    assert.deepEqual(shown.reread, want.reread, "the cache re-reads have their own tile with their share, never added into Tokens");
    assert.deepEqual(shown.head, HEAD, "the By project table shows Tokens, then Input, Cache write, Output and Cache read");
    assert.ok(shown.row !== null, "the By project table has the project's row");
    assert.deepEqual(shown.row.slice(1, 6), want.parts, `the project's row shows the headline and all four parts: ${JSON.stringify(shown.row)}`);
    assert.equal(shown.row[6], String(counted.runs), `the project's row counts every run: ${JSON.stringify(shown.row)}`);
    const words = await me.seen();
    assert.ok(want.merged.every((total) => !words.includes(total)), `no total adds the cache re-reads into the tokens: ${JSON.stringify(want.merged)}`);
  });
});
