import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, bridgeCall, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const AUTH_MS = 90000;
const UPDATE_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TOO_OLD = "native_auth_harness_too_old";
const CLAUDE = { tile: "Anthropic", method: "Sign in with your Claude subscription", start: "Open Claude sign-in", id: "claude-code", name: "Claude Code", minimum: "2.1.268" };
const CODEX = { tile: "OpenAI", method: "Sign in with ChatGPT", start: "Get sign-in code", id: "codex", name: "Codex", minimum: "0.56.0" };

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Sign-in shop\n\nKeeps the accounts.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the sign-in shop"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes one runnable shell script. Inputs: the path and the script. Output: none. */
function script(path, text) {
  writeFileSync(path, text, { mode: 0o755 });
}

/** Quotes a test path for a shell script; the test makes every path, so none holds a quote.
 * Input: the path. Output: the quoted path. */
function quoted(path) {
  assert.ok(!path.includes("'"), `a test path holds a quote: ${path}`);
  return `'${path}'`;
}

/** The `--version` arm of a stand-in: the version line, or nothing for a harness whose version
 * CrazyAgent can't read. Inputs: the line or null. Output: the shell arm. */
function versionArm(line) {
  return line === null ? "--version) exit 0 ;;" : `--version) echo "${line}" ;;`;
}

/** Puts a stand-in self-updating Claude Code where its own installer would, under the test HOME:
 * a version file and the `.local/bin/claude` link. It prints the old version (or none), `update`
 * writes the new version and moves the link, and every other call, a sign-in included, fails, so
 * a sign-in that runs it ends with a code other than too old. Inputs: the test HOME, the old and
 * new versions, the old null for an unreadable version. Output: the bin folder. */
function oldClaude(home, from, to) {
  const versions = join(home, ".local", "share", "claude", "versions");
  const bin = join(home, ".local", "bin");
  for (const folder of [versions, bin]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const first = join(versions, from ?? "0.0.0");
  const next = join(versions, to);
  script(first, `#!/bin/sh
case "$1" in
  ${versionArm(from === null ? null : `${from} (Claude Code)`)}
  update) printf '#!/bin/sh\\ncase "$1" in --version) echo "${to} (Claude Code)" ;; *) exit 1 ;; esac\\n' > ${quoted(next)} && /bin/chmod 755 ${quoted(next)} && /bin/ln -sfn ${quoted(next)} ${quoted(join(bin, "claude.next"))} && /bin/mv -f ${quoted(join(bin, "claude.next"))} ${quoted(join(bin, "claude"))} && echo "Updated to ${to}" ;;
  *) exit 1 ;;
esac
`);
  symlinkSync(first, join(bin, "claude"));
  return bin;
}

/** Puts a stand-in Codex in an npm prefix, with a stand-in npm that logs every call, answers the
 * new version to `view` and turns the harness into it on `install`. The harness prints the old
 * version (or none) and fails every other call, a sign-in included. Inputs: the test folder, the
 * prefix, and the old and new versions, the old null for an unreadable version. Output: the bin
 * folder, the harness and the npm log. */
function oldCodex(root, prefix, from, to) {
  const harness = join(prefix, "lib", "node_modules", "@openai", "codex", "bin", "codex");
  const log = join(root, "npm.log");
  mkdirSync(join(prefix, "bin"), { recursive: true, mode: 0o700 });
  mkdirSync(dirname(harness), { recursive: true, mode: 0o700 });
  script(harness, `#!/bin/sh
case "$1" in
  ${versionArm(from === null ? null : `codex-cli ${from}`)}
  *) exit 1 ;;
esac
`);
  symlinkSync("../lib/node_modules/@openai/codex/bin/codex", join(prefix, "bin", "codex"));
  writeFileSync(log, "", { mode: 0o600 });
  script(join(prefix, "bin", "npm"), `#!/bin/sh
printf '%s\\n' "$*" >> ${quoted(log)}
case "$1" in
  view) echo ${to} ;;
  install) printf '#!/bin/sh\\ncase "$1" in --version) echo "codex-cli ${to}" ;; *) exit 1 ;; esac\\n' > ${quoted(`${harness}.new`)} && /bin/chmod 755 ${quoted(`${harness}.new`)} && /bin/mv -f ${quoted(`${harness}.new`)} ${quoted(harness)} ;;
  *) exit 2 ;;
esac
`);
  return { bin: join(prefix, "bin"), harness, log };
}

/** Lines of the npm log the stand-in npm wrote. Input: the log path. Output: the lines. */
function npmCalls(log) {
  return readFileSync(log, "utf8").split("\n").filter(Boolean);
}

/** The engine's sign-in view of the account with this name, read the way the app reads it.
 * Inputs: the bridge and the account name. Output: the view, or null before the account or
 * its sign-in exists. */
async function signInOf(bridge, name) {
  const listed = await value(bridge, "desktop_list_accounts", { after: null, limit: 50 });
  const account = listed.accounts.find((one) => one.name === name);
  if (!account) return null;
  const record = await bridgeCall(bridge, "desktop_inspect_model_access", { collection: "connections", id: account.id });
  if (!("value" in record)) return null;
  const view = await bridgeCall(bridge, "desktop_read_native_auth", { ticket: record.value.ticket });
  return "value" in view ? view.value : null;
}

/** Waits until the engine's sign-in view of an account is the one wanted. Inputs: the bridge,
 * the account name, the check, what is awaited and the wait. Output: the view. */
async function settled(bridge, name, wanted, what, ms = AUTH_MS) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS * 5)) {
    const view = await signInOf(bridge, name);
    if (view !== null && wanted(view)) return view;
    assert.ok(Date.now() < deadline, `${what} never came for ${name}: ${JSON.stringify(view)}`);
  }
}

/** Waits until the page's own call of a command was answered the given number of times. The
 * test reads the sign-in through the bridge the page uses, and a read between the page's own
 * read and its start replaces the one selection the start needs, so a check waits for the
 * page's start first. Inputs: the call log, the command, the count and the wait. Output: that
 * answer. */
async function pageAnswer(log, cmd, count, ms = AUTH_MS) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS)) {
    const answers = log.trail.filter((entry) => entry.cmd === cmd);
    if (answers.length >= count) return answers[count - 1].answer;
    assert.ok(Date.now() < deadline, `the page called ${cmd} ${answers.length} times, not ${count}`);
  }
}

/** Whether a sign-in view is one refused as too old. Input: the view. Output: the answer. */
function tooOld(view) {
  return view.phase === "failed" && view.code === TOO_OLD;
}

/** Whether a sign-in view ended after running the harness: the stand-in fails every call, so
 * it ended failed with a code other than too old. Input: the view. Output: the answer. */
function ran(view) {
  return view.phase === "failed" && typeof view.code === "string" && view.code !== TOO_OLD;
}

/** Starts the real engine and the bridge with stand-in harnesses in the test HOME's harness
 * folders, makes one project, serves the built app to a browser page, runs the steps and stops
 * all. Inputs: the test context, the folder prefix, the steps and the stand-in versions, each
 * `{ from, to }` with `from` null for a harness that prints no version. The steps get the
 * bridge, the owner, the page, the opener, the Add account helpers and the stand-ins. */
async function onEngine(t, prefix, steps, versions) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const base = realpathSync(tmpdir());
  for (let at = base; at !== dirname(at); at = dirname(at)) {
    assert((statSync(at).mode & 0o002) === 0, `${at} is writable by every user, so the engine refuses a harness under it: set TMPDIR to a private folder`);
  }
  const root = realpathSync(mkdtempSync(join(base, prefix)));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? join(process.env.E2E_EVIDENCE, prefix) : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "sign-in-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  ownerRepo(repo);
  const fakes = {
    claude: oldClaude(home, versions.claude.from, versions.claude.to),
    codex: oldCodex(root, join(home, ".npm-global"), versions.codex.from, versions.codex.to),
  };
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
  const env = { PATH: [fakes.codex.bin, fakes.claude, "/usr/bin", "/bin"].join(":"), HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
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
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: receipt.project_id });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    const catalog = await value(bridge, "desktop_harness_catalog", { check: true });
    for (const id of ["codex", "claude-code"]) {
      assert.notEqual(catalog.find((entry) => entry.id === id)?.status.kind, "not_installed", `the engine finds the stand-in ${id} in its HOME places`);
    }

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log, {});
    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        let visits = 0;
        const open = async (hash, what, check) => {
          assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${hash}` });
          await me.waitFor(what, check);
        };
        const pick = async (kind, title) => {
          const missing = await page.evaluate(`(() => {
            document.querySelectorAll("[data-e2e-pick]").forEach((element) => element.removeAttribute("data-e2e-pick"));
            const all = [...document.querySelectorAll("dialog[open] button.${kind}")];
            const hit = all.find((button) => button.querySelector(".title")?.firstChild?.textContent.trim() === ${JSON.stringify(title)});
            if (!hit) return all.map((button) => button.innerText.trim());
            hit.setAttribute("data-e2e-pick", "");
            return null;
          })()`);
          assert.equal(missing, null, `no ${kind} ${title} to pick: ${JSON.stringify(missing)}`);
          await me.follow("[data-e2e-pick]");
        };
        const signIn = async (harness, name) => {
          await open("#/settings/accounts?action=add", "the Add account tiles", (text) => text.includes(harness.tile));
          await pick("tile", harness.tile);
          await pick("method", harness.method);
          await me.waitFor(`${harness.method} with its name field`, (text) => text.includes("Account name"));
          await me.type("#account-name", name);
          await me.click(harness.start);
        };
        await steps({ bridge, me, page, open, signIn, fakes, log });
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

/** One harness below its sign-in minimum: the sign-in stops before the harness runs, the panel
 * names the version and the minimum with Update and Check again, Update runs the harness's own
 * update after its confirm, and Check again then starts a sign-in that runs the harness at its
 * minimum. Inputs: the test context, the harness, its old version, the account name and the
 * updater check.
 * Output: the steps. */
function belowThenUpdated(t, harness, old, account, updated) {
  return async ({ bridge, me, open, signIn, fakes, log }) => {
    await open("#/settings/harnesses", "the Harnesses page", (text) => text.includes(`Sign-in needs ${harness.minimum} or newer.`));
    await signIn(harness, account);
    const line = `${harness.name} ${old} is too old to sign in from CrazyAgent; it needs ${harness.minimum} or newer.`;
    await me.waitFor("the too-old line", (text) => text.includes(line));
    await settled(bridge, account, tooOld, "the engine's too-old refusal, before the harness ran");
    const shown = await me.seen();
    assert.ok(shown.includes(`Update ${harness.name}`) && shown.includes("Check again"), `the panel offers Update and Check again:\n${shown}`);
    assert.ok(!shown.includes("Try again"), "a retry would never help, so there is no Try again");
    await me.capture(`${harness.id}-too-old`);
    await me.click(`Update ${harness.name}`);
    await me.waitFor("the Update confirm", (text) => text.includes(`Update ${harness.name}?`) && text.includes("CrazyAgent runs exactly this line:"));
    await me.capture(`${harness.id}-update-confirm`);
    await me.click("Update", "dialog[open] button");
    await me.waitFor("the finished update", (text) => text.includes(`Updated from ${old} to ${harness.minimum}.`), UPDATE_MS);
    updated(fakes);
    assert.equal((await value(bridge, "desktop_harness_versions", { id: harness.id, look: false })).installed, harness.minimum, "the engine reads the new version with no restart");
    await me.click("Check again");
    const restarted = await pageAnswer(log, "desktop_start_native_auth", 2);
    assert.ok("value" in restarted, `Check again starts a new sign-in: ${JSON.stringify(restarted)}`);
    const after = await settled(bridge, account, ran, `a sign-in that runs ${harness.name} at its minimum`);
    t.diagnostic(`${harness.name} at its minimum ended with ${after.code}`);
    await me.waitFor("the panel without the too-old line", (text) => !text.includes(line) && !text.includes(`${harness.name} is too old`));
    await me.capture(`${harness.id}-at-minimum`);
  };
}

test("DEC-166 Claude Code below its sign-in minimum: refused with its version, Update after the confirm, Check again then runs it", { timeout: 900000 }, (t) =>
  onEngine(t, "cad-sign-in-min-claude-", belowThenUpdated(t, CLAUDE, "2.1.200", "Old Claude", () => {}),
    { claude: { from: "2.1.200", to: "2.1.268" }, codex: { from: "0.169.0", to: "0.169.0" } }));

test("DEC-166 Codex below its sign-in minimum: refused with its version, Update after the confirm, Check again then runs it", { timeout: 900000 }, (t) =>
  onEngine(t, "cad-sign-in-min-codex-", belowThenUpdated(t, CODEX, "0.50.0", "Old ChatGPT", (fakes) => {
    assert.ok(npmCalls(fakes.codex.log).includes("install -g @openai/codex"), "Update ran npm with the confirmed line");
  }), { claude: { from: "2.1.325", to: "2.1.325" }, codex: { from: "0.50.0", to: "0.56.0" } }));

test("DEC-166 a Claude Code and a Codex whose version CrazyAgent can't read sign in as before", { timeout: 900000 }, (t) =>
  onEngine(t, "cad-sign-in-min-unknown-", async ({ bridge, me, signIn }) => {
    for (const [harness, account] of [[CLAUDE, "Unread Claude"], [CODEX, "Unread ChatGPT"]]) {
      await signIn(harness, account);
      const after = await settled(bridge, account, ran, `a sign-in that runs ${harness.name} with an unknown version`);
      t.diagnostic(`${harness.name} with an unknown version ended with ${after.code}`);
      assert.ok(!(await me.seen()).includes("is too old to sign in"), "no too-old line for an unknown version");
      await me.capture(`${harness.id}-unknown-version`);
    }
  }, { claude: { from: null, to: "2.1.268" }, codex: { from: null, to: "0.56.0" } }));
