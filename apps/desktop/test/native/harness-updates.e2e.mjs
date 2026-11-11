import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, bridgeCall, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const LOOKUP_MS = 60000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const PAGE = "#/settings/harnesses";
const GROUPS = { mode: "Harness updates", when: "Check for harness updates", online: "Newer versions of self-updating installs", install: "Install from the app" };
const DEFAULTS = { mode: "check_and_update", when: "on_button", online: "on", install: "on" };
const BREW_PREFIXES = ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"];
const UNKNOWN = "CrazyAgent can't tell how this was installed, so it won't update it. Update it with its own installer.";
const NO_TOOL = "CrazyAgent installs with Homebrew or npm and found no way to install this one.";

/** The radio buttons of one update setting on the Harnesses page. Input: the setting's key.
 * Output: the selector. */
function radios(key) {
  return `[role=radiogroup][aria-label="${GROUPS[key]}"] [role=radio]`;
}

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Tool shop\n\nKeeps the tools.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes one runnable shell script that only its owner can change. Inputs: the path and the
 * script. Output: none. */
function script(path, text) {
  writeFileSync(path, text, { mode: 0o755 });
}

/** Quotes a test path for a shell script; the test makes every path, so none holds a quote.
 * Input: the path. Output: the quoted path. */
function quoted(path) {
  assert.ok(!path.includes("'"), `a test path holds a quote: ${path}`);
  return `'${path}'`;
}

/** Puts a fake Codex 0.0.1 in an npm prefix inside the test folder, with a fake npm that logs
 * every call, answers 0.0.2 to `view` and turns the harness into 0.0.2 on `install`. The engine
 * runs these with a cleared environment, so the log path is written into the script.
 * Input: the test folder. Output: the prefix's bin folder, the harness and the npm log. */
function npmCodex(root) {
  const prefix = join(root, "npm");
  const harness = join(prefix, "lib", "node_modules", "@openai", "codex", "bin", "codex");
  const log = join(root, "npm.log");
  mkdirSync(join(prefix, "bin"), { recursive: true, mode: 0o700 });
  mkdirSync(join(prefix, "lib", "node_modules", "@openai", "codex", "bin"), { recursive: true, mode: 0o700 });
  script(harness, "#!/bin/sh\necho \"codex-cli 0.0.1\"\n");
  symlinkSync("../lib/node_modules/@openai/codex/bin/codex", join(prefix, "bin", "codex"));
  writeFileSync(log, "", { mode: 0o600 });
  script(join(prefix, "bin", "npm"), `#!/bin/sh
printf '%s\\n' "$*" >> ${quoted(log)}
case "$1" in
  view) echo 0.0.2 ;;
  install) printf '#!/bin/sh\\necho "codex-cli 0.0.2"\\n' > ${quoted(`${harness}.new`)} && /bin/chmod 755 ${quoted(`${harness}.new`)} && /bin/mv -f ${quoted(`${harness}.new`)} ${quoted(harness)} ;;
  *) exit 2 ;;
esac
`);
  return { bin: join(prefix, "bin"), harness, log };
}

/** Puts a fake self-updating Claude Code 0.0.1 where its own installer would, under the test
 * HOME: a version file and the `.local/bin/claude` link. `update` writes 0.0.2 and moves the
 * link. Input: the test HOME. Output: the bin folder. */
function selfUpdatingClaude(home) {
  const versions = join(home, ".local", "share", "claude", "versions");
  const bin = join(home, ".local", "bin");
  for (const folder of [versions, bin]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  script(join(versions, "0.0.1"), `#!/bin/sh
case "$1" in
  --version) echo "0.0.1 (Claude Code)" ;;
  update) printf '#!/bin/sh\\necho "0.0.2 (Claude Code)"\\n' > ${quoted(join(versions, "0.0.2"))} && /bin/chmod 755 ${quoted(join(versions, "0.0.2"))} && /bin/ln -sfn ${quoted(join(versions, "0.0.2"))} ${quoted(join(bin, "claude.next"))} && /bin/mv -f ${quoted(join(bin, "claude.next"))} ${quoted(join(bin, "claude"))} && echo "Updated to 0.0.2" ;;
  *) exit 2 ;;
esac
`);
  symlinkSync(join(versions, "0.0.1"), join(bin, "claude"));
  return bin;
}

/** Puts a fake OpenCode in a plain folder no installer owns, so the engine can't tell how it
 * was installed. Input: the test folder. Output: the bin folder. */
function plainOpencode(root) {
  const bin = join(root, "plain", "bin");
  mkdirSync(bin, { recursive: true, mode: 0o700 });
  script(join(bin, "opencode"), "#!/bin/sh\necho \"opencode 0.0.1\"\n");
  return bin;
}

/** Listens as the engine's HTTPS proxy, notes the host each connection asks for and then
 * refuses it, so a test sees whether the engine looked a version up and can cut it off the
 * network. Input: none. Output: the port, the hosts asked for and a close. */
async function refusingProxy() {
  const asked = [];
  const server = createServer((socket) => {
    socket.once("data", (chunk) => {
      const first = chunk.toString("latin1").split("\r\n")[0];
      asked.push(/^CONNECT (\S+) /.exec(first)?.[1] ?? first.slice(0, 80));
      socket.destroy();
    });
    socket.setTimeout(STEP_MS, () => socket.destroy());
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return { port: server.address().port, asked: () => [...asked], close: () => server.close() };
}

/** Whether the engine asked the proxy for Claude Code's release pointer. Input: the proxy.
 * Output: true when it did. */
function lookedUpClaude(proxy) {
  return proxy.asked().includes("downloads.claude.ai:443");
}

/** Lines of the npm log the fake npm wrote. Input: the log path. Output: the lines. */
function npmCalls(log) {
  return readFileSync(log, "utf8").split("\n").filter(Boolean);
}

/** Polls a read until it equals what is wanted, failing with the last value after the wait.
 * Inputs: what is awaited, the wanted value, the read and the wait. Output: none. */
async function until(what, wanted, read, ms = STEP_MS) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Starts the real engine and the bridge with fake harnesses first on the engine's PATH, makes
 * one project, serves the built app to a browser page, runs the steps and stops everything.
 * Inputs: the test context, the temporary folder prefix, the steps and whether the engine's
 * HTTPS goes through a proxy that refuses every connection. The steps get the bridge, the owner
 * on the page, the page, a page opener, a harness name reader, a row reader, the fakes, the
 * proxy and the engine's pid. Output: none. */
async function onEngine(t, prefix, steps, { offline = false } = {}) {
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
  const codex = npmCodex(root);
  const fakes = { codex, claude: selfUpdatingClaude(home), opencode: plainOpencode(root), root };
  const proxy = await refusingProxy();
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
  const path = [fakes.opencode, codex.bin, fakes.claude, "/usr/bin", "/bin"].join(":");
  const through = offline ? { https_proxy: `http://127.0.0.1:${proxy.port}`, HTTPS_PROXY: `http://127.0.0.1:${proxy.port}` } : {};
  const env = { PATH: path, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8", ...through };
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
    assert.deepEqual(await value(bridge, "desktop_harness_update_settings"), DEFAULTS, "a new engine starts with the default update settings");
    const catalog = await value(bridge, "desktop_harness_catalog", { check: true });
    const named = new Map(catalog.map((entry) => [entry.id, entry]));
    for (const id of ["codex", "claude-code", "opencode"]) assert.notEqual(named.get(id)?.status.kind, "not_installed", `the engine finds the fake ${id} on its PATH`);
    const name = (id) => {
      assert.ok(named.has(id), `the catalog lists ${id}`);
      return named.get(id).name;
    };

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
        const open = async () => {
          assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${PAGE}` });
          await me.waitFor("the Harnesses page with its rows and the update settings", (text) => text.includes(GROUPS.mode) && text.includes(name("codex")) && text.includes(name("aider")));
        };
        const row = (id) => page.evaluate(`document.querySelector(${JSON.stringify(`.set-row[data-setting="${name(id)}"]`)})?.innerText ?? null`);
        const rowShows = async (id, what, words) => {
          for (const deadline = Date.now() + LOOKUP_MS; ; await pause(POLL_MS)) {
            const text = await row(id);
            if (text !== null && (words instanceof RegExp ? words.test(text) : text.includes(words))) return text;
            if (Date.now() > deadline) {
              await me.capture(`no-${id}-line`);
              assert.fail(`the ${name(id)} row never showed ${what}; it shows:\n${text}`);
            }
          }
        };
        const buttons = (id) => page.evaluate(`[...(document.querySelector(${JSON.stringify(`.set-row[data-setting="${name(id)}"]`)})?.querySelectorAll("button[aria-label]") ?? [])].map((button) => button.getAttribute("aria-label"))`);
        const dialog = () => page.evaluate(`(() => { const box = document.querySelector("dialog.dialog"); return box ? { title: box.querySelector("h2")?.innerText.trim() ?? null, line: box.querySelector("pre")?.innerText.trim() ?? null, text: box.innerText } : null; })()`);
        const shownChoice = (key) => page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${GROUPS[key]}"] [role=radio][aria-checked="true"]`)})?.innerText.trim() ?? null`);
        const choose = async (key, label, wanted) => {
          await me.click(label, radios(key));
          await until(`${GROUPS[key]}: ${label} in the engine`, wanted, async () => (await value(bridge, "desktop_harness_update_settings"))[key]);
          await until(`${GROUPS[key]}: ${label} on the page`, label, () => shownChoice(key));
        };
        await steps({ bridge, me, page, open, name, row, rowShows, buttons, dialog, shownChoice, choose, fakes, proxy, engine });
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
    proxy.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Reads one harness's versions row straight from the bridge. Inputs: the bridge and the id.
 * Output: the row. */
function versions(bridge, id) {
  return value(bridge, "desktop_harness_versions", { id, look: false });
}

/** Calls a harness action straight on the bridge, as a page that skipped its buttons would,
 * and returns the refusal code. Inputs: the bridge, the command and the id. Output: the code. */
async function refused(bridge, cmd, id) {
  const answer = await bridgeCall(bridge, cmd, { id });
  assert.ok(answer.failure, `${cmd} ${id} was not refused: ${JSON.stringify(answer)}`);
  return answer.failure.code;
}

test("Harness updates Check and update: Check shows the newer version, and Update after the confirm changes the version line with no engine restart", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-update-", async ({ bridge, me, open, name, rowShows, dialog, fakes, engine }) => {
    await open();
    await rowShows("codex", "the installed version", "Version 0.0.1.");
    assert.deepEqual(npmCalls(fakes.codex.log), [], "Only when I choose: opening the page asked npm nothing");
    await me.click(`Check ${name("codex")} for updates`, "button");
    await rowShows("codex", "the newer version", "Version 0.0.1. Version 0.0.2 is available.");
    assert.deepEqual(npmCalls(fakes.codex.log), ["view @openai/codex version"], "Check asked npm once");
    await me.click(`Update ${name("codex")}`, "button");
    await me.waitFor("the Update confirm", (text) => text.includes(`Update ${name("codex")}?`));
    const asked = await dialog();
    assert.equal(asked.title, `Update ${name("codex")}?`);
    assert.equal(asked.line, `${fakes.root}/npm/bin/npm install -g @openai/codex`, "the confirm shows exactly the line the engine runs");
    assert.match(asked.text, /Installed with npm \(@openai\/codex\)\./);
    assert.match(asked.text, /When it finishes, new agents use the new version with no restart\./);
    await me.capture("update-confirm");
    await me.click("Update", "dialog.dialog button");
    await rowShows("codex", "how the update ended", "Updated from 0.0.1 to 0.0.2. New agents use it now; no restart needed.");
    await rowShows("codex", "the new version", "Version 0.0.2.");
    assert.deepEqual(npmCalls(fakes.codex.log), ["view @openai/codex version", "install -g @openai/codex"], "Update ran npm once with the confirmed line");
    const after = await versions(bridge, "codex");
    assert.equal(after.installed, "0.0.2", "the engine reads the new binary with no restart");
    assert.equal(after.running, false);
    const catalog = await value(bridge, "desktop_harness_catalog", { check: true });
    const entry = catalog.find((item) => item.id === "codex")?.executable;
    assert.ok([join(fakes.codex.bin, "codex"), fakes.codex.harness].includes(entry), `new agents start the entry that now runs 0.0.2: ${entry}`);
    assert.equal(spawnSync(entry, ["--version"], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } }).stdout.trim(), "codex-cli 0.0.2");
    assert.deepEqual([engine.exitCode, engine.signalCode], [null, null], "the engine that started the update is still the one running");
    await me.capture("updated");
  }));

test("Harness updates Check only: Check stays, there is no Update button, and a direct bridge call answers harness_update_check_only", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-check-only-", async ({ bridge, open, name, rowShows, buttons, choose, fakes }) => {
    await open();
    await choose("mode", "Check only", "check_only");
    await rowShows("codex", "the installed version", "Version 0.0.1.");
    await until("Check only shows Check and no Update", [`Check ${name("codex")} for updates`], () => buttons("codex"));
    assert.equal(await refused(bridge, "desktop_harness_update", "codex"), "harness_update_check_only");
    assert.ok(!npmCalls(fakes.codex.log).some((line) => line.startsWith("install")), "nothing was installed");
  }));

test("Harness updates Off: no Check or Update button, and a direct bridge call answers harness_update_off", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-off-", async ({ bridge, open, row, rowShows, buttons, shownChoice, choose, fakes }) => {
    await open();
    await choose("mode", "Off", "off");
    await open();
    await until("Off on the page that opened again", "Off", () => shownChoice("mode"));
    await rowShows("aider", "its install line or reason, read once the settings are known", /Install|Homebrew or npm/);
    assert.deepEqual(await buttons("codex"), [], "Off shows no harness buttons");
    assert.doesNotMatch(await row("codex"), /Version 0\.0\.1/, "Off reads no versions");
    assert.equal(await refused(bridge, "desktop_harness_update", "codex"), "harness_update_off");
    assert.deepEqual(npmCalls(fakes.codex.log), [], "npm never ran");
  }));

test("Check for harness updates Only when I choose: opening the page runs no check", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-on-button-", async ({ open, rowShows, fakes, proxy }) => {
    for (let visit = 0; visit < 2; visit += 1) {
      await open();
      await rowShows("codex", "the installed version", "Version 0.0.1.");
      await rowShows("claude-code", "the installed version", "Version 0.0.1.");
    }
    assert.deepEqual(npmCalls(fakes.codex.log), [], "two openings asked npm nothing");
    assert.equal(lookedUpClaude(proxy), false, `nothing was looked up on Claude Code's site: ${JSON.stringify(proxy.asked())}`);
  }, { offline: true }));

test("Check for harness updates Each time this page opens: opening the page runs one check", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-on-open-", async ({ open, rowShows, choose, fakes }) => {
    await open();
    await choose("when", "Each time this page opens", "on_open");
    await rowShows("codex", "the check the new choice ran on this page", "Version 0.0.1. Version 0.0.2 is available.");
    const before = npmCalls(fakes.codex.log).length;
    await open();
    await rowShows("codex", "the newer version found on opening", "Version 0.0.1. Version 0.0.2 is available.");
    assert.deepEqual(npmCalls(fakes.codex.log).slice(before), ["view @openai/codex version"], "one opening ran one check");
  }));

test("Cancel in the Update confirm runs nothing", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-cancel-", async ({ bridge, me, open, name, rowShows, dialog, fakes }) => {
    await open();
    await rowShows("codex", "the installed version", "Version 0.0.1.");
    await me.click(`Update ${name("codex")}`, "button");
    await me.waitFor("the Update confirm", (text) => text.includes(`Update ${name("codex")}?`));
    await me.click("Cancel", "dialog.dialog button");
    await until("the confirm to close", null, dialog);
    await pause(POLL_MS * 20);
    assert.deepEqual(npmCalls(fakes.codex.log), [], "Cancel ran nothing");
    const after = await versions(bridge, "codex");
    assert.deepEqual([after.installed, after.running, after.last], ["0.0.1", false, null], "the engine started no update");
  }));

test("An install CrazyAgent can't tell shows the reason and the guide link, with no Check or Update", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-unknown-", async ({ bridge, page, open, name, rowShows, buttons }) => {
    await open();
    await rowShows("opencode", "why it won't update", UNKNOWN);
    const read = await versions(bridge, "opencode");
    assert.deepEqual([read.method.kind, read.command, read.refusal], ["unknown", null, "harness_update_unknown_install"]);
    assert.deepEqual(await buttons("opencode"), [], "no Check or Update for an install it can't tell");
    const link = await page.evaluate(`(() => { const a = [...document.querySelectorAll(${JSON.stringify(`.set-row[data-setting="${name("opencode")}"] a`)})].find((item) => item.innerText.trim() === "How to update it yourself"); return a ? a.getAttribute("href") : null; })()`);
    assert.equal(link, read.guide, "the guide link is the harness's own guide");
    assert.match(link, /^https:\/\//);
  }));

test("Newer versions of self-updating installs Look them up on the vendor's site: Check reads the vendor's real pointer and Update stays", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-lookup-", async ({ me, open, name, rowShows, buttons }) => {
    await open();
    await rowShows("claude-code", "the installed version", "Version 0.0.1.");
    await me.click(`Check ${name("claude-code")} for updates`, "button");
    await rowShows("claude-code", "the vendor's newest version", /Version 0\.0\.1\. Version \d+(\.\d+)+ is available\./);
    assert.ok((await buttons("claude-code")).includes(`Update ${name("claude-code")}`), "Update stays after the check");
  }));

test("Newer versions of self-updating installs Look them up with the network cut: Check says it couldn't check and Update stays", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-lookup-offline-", async ({ me, open, name, rowShows, buttons, proxy }) => {
    await open();
    await rowShows("claude-code", "the installed version", "Version 0.0.1.");
    await me.click(`Check ${name("claude-code")} for updates`, "button");
    await rowShows("claude-code", "the failed check", "Version 0.0.1. Couldn't check for a newer version.");
    assert.ok(lookedUpClaude(proxy), `the check tried Claude Code's site and was cut off: ${JSON.stringify(proxy.asked())}`);
    assert.ok((await buttons("claude-code")).includes(`Update ${name("claude-code")}`), "Update stays after a failed check");
  }, { offline: true }));

test("Newer versions of self-updating installs Don't look them up: Check says the install updates itself and the engine reaches no site", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-no-lookup-", async ({ me, open, name, rowShows, buttons, choose, proxy }) => {
    await open();
    await choose("online", "Don't look them up", "off");
    await rowShows("claude-code", "the installed version", "Version 0.0.1.");
    await me.click(`Check ${name("claude-code")} for updates`, "button");
    await rowShows("claude-code", "the self-update line", "Version 0.0.1. This install updates itself. Update asks it to look now.");
    assert.equal(lookedUpClaude(proxy), false, `the engine made no connection to look the version up: ${JSON.stringify(proxy.asked())}`);
    assert.ok((await buttons("claude-code")).includes(`Update ${name("claude-code")}`), "Update stays");
  }, { offline: true }));

test("Install from the app Offer Install: a missing harness shows Install with the exact line, or the guide when no installer is here, and Cancel runs nothing", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-install-offer-", async ({ bridge, me, open, name, rowShows, buttons, dialog }) => {
    await open();
    const read = await versions(bridge, "aider");
    if (read.install === null) {
      assert.equal(read.refusal, "harness_install_no_tool", "no Homebrew on this Mac is the only reason for no install line");
      await rowShows("aider", "why it can't install", NO_TOOL);
      assert.deepEqual(await buttons("aider"), [], "no Install without an installer");
      t.diagnostic("this Mac has no Homebrew in a usual prefix; the confirm part ran on the refusal");
      return;
    }
    assert.ok(BREW_PREFIXES.some((prefix) => read.install === `${prefix}/bin/brew install --formula aider`), `the install line names Homebrew's own path: ${read.install}`);
    await me.click(`Install ${name("aider")}`, "button");
    await me.waitFor("the Install confirm", (text) => text.includes(`Install ${name("aider")}?`));
    const asked = await dialog();
    assert.equal(asked.line, read.install, "the confirm shows exactly the line the engine runs");
    assert.match(asked.text, /It never uses sudo and never runs a script from a website\./);
    assert.ok(asked.text.includes(`When it finishes, new agents can use ${name("aider")} with no engine restart.`));
    await me.capture("install-confirm");
    await me.click("Cancel", "dialog.dialog button");
    await until("the confirm to close", null, dialog);
    const after = await versions(bridge, "aider");
    assert.deepEqual([after.installed, after.running, after.last], [null, false, null], "Cancel installed nothing");
  }));

test("Install from the app: fx by Vercel and OpenHands CLI have no installer, so no Install button and the guide link", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-install-guide-", async ({ bridge, page, open, name, rowShows, buttons }) => {
    await open();
    for (const id of ["fx", "openhands"]) {
      const read = await versions(bridge, id);
      assert.deepEqual([read.install, read.refusal], [null, "harness_install_no_tool"], id);
      await rowShows(id, "why it can't install", NO_TOOL);
      assert.deepEqual(await buttons(id), [], `${id}: no Install button`);
      const link = await page.evaluate(`(() => { const a = [...document.querySelectorAll(${JSON.stringify(`.set-row[data-setting="${name(id)}"] a`)})].find((item) => item.innerText.trim() === "How to install it yourself"); return a ? a.getAttribute("href") : null; })()`);
      assert.equal(link, read.guide, `${id}: the guide link is its own install guide`);
    }
  }));

test("Install from the app Don't offer Install: no Install button anywhere, and a direct bridge call answers harness_install_off", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-harness-install-off-", async ({ bridge, page, open, rowShows, shownChoice, choose }) => {
    await open();
    await choose("install", "Don't offer Install", "off");
    await open();
    await until("Don't offer Install on the page that opened again", "Don't offer Install", () => shownChoice("install"));
    for (const id of ["aider", "fx"]) await rowShows(id, "why there is no Install", "Install from the app is off in settings.");
    const installs = await page.evaluate(`[...document.querySelectorAll("button[aria-label^='Install ']")].map((button) => button.getAttribute("aria-label"))`);
    assert.deepEqual(installs, [], "no Install button on any row");
    assert.equal(await refused(bridge, "desktop_harness_install", "aider"), "harness_install_off");
  }));

const NO_TEST_PREFIX = "the engine takes Homebrew and npm for an install only from the fixed Homebrew prefixes (Roots::system), so a fake brew or npm on a test prefix is never used; running the install would run the owner's real Homebrew";

test("Install from the app with Homebrew: after the confirm the row shows a version and Ready with no engine restart", { skip: NO_TEST_PREFIX }, () => {});

test("Install from the app with npm only: the confirm shows <prefix>/bin/npm install -g @continuedev/cli", { skip: NO_TEST_PREFIX }, () => {});

test("A Homebrew-installed Aider then shows Check for updates and Update", { skip: NO_TEST_PREFIX }, () => {});
