import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { handOff, onEngine, startingWith } from "./codex-task-engine.mjs";

/** The probe every sub-agent run of the stand-in Codex makes from its worktree inside the
 * owner's project folder: whether it can read the file in the folder the owner hides. It
 * adds one line per run to .cache/hidden-probe.txt in its own home. */
const PROBE = String.raw`case "$1:$(/bin/pwd -P)" in
  app-server:*) ;;
  */.crazyagent/worktrees/agent-*)
    r="$(/bin/pwd -P)/../../.."
    /bin/mkdir -p "$CODEX_HOME/.cache"
    if /bin/cat "$r/secrets/key.txt" >/dev/null 2>&1; then echo "read secrets"; else echo "closed secrets"; fi >> "$CODEX_HOME/.cache/hidden-probe.txt"
    ;;
esac
`;

const SECRET = "E2E_HIDDEN_SECRET\n";

/** Reads every sub-agent's probe lines. Input: the engine root. Output: a map from each
 * probe file to its lines. */
function probes(runtime) {
  const agents = join(runtime, "team", "agents");
  const found = new Map();
  if (!existsSync(agents)) return found;
  for (const name of readdirSync(agents)) {
    const path = join(agents, name, "home", ".cache", "hidden-probe.txt");
    if (existsSync(path)) found.set(path, readFileSync(path, "utf8").trim().split("\n"));
  }
  return found;
}

/** The probe lines written since an earlier read. Inputs: the earlier and the later read.
 * Output: the new lines of every probe file. */
function since(before, after) {
  return [...after].flatMap(([path, lines]) => lines.slice(before.get(path)?.length ?? 0));
}

/** Polls the team status until a check passes. Inputs: the bridge, the project, what is
 * awaited and the check. Output: the status that passed. */
async function teamUntil(bridge, project, what, check) {
  for (const deadline = Date.now() + STEP_MS * 4; ; await pause(POLL_MS)) {
    const status = await value(bridge, "desktop_team_status", { id: project });
    if (check(status)) return status;
    assert.ok(Date.now() < deadline, `${what}: ${JSON.stringify(status.agents)}`);
  }
}

test("a sub-agent running when the owner hides a folder is named with Restart them, which starts it again kept out of the folder", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-hidden-seers-", async ({ project, runtime, me, page, site, bridge }) => {
    const repo = realpathSync(join(dirname(runtime), "repos", "fruit-stand"));
    const secrets = join(repo, "secrets");
    mkdirSync(secrets, { mode: 0o700 });
    writeFileSync(join(secrets, "key.txt"), SECRET, { mode: 0o600 });
    const policy = await value(bridge, "desktop_project_agent_policy", { id: project });
    assert.notEqual(policy.access, "full", "the project's agents run in the sandbox");

    await handOff(me, page, project, runtime);
    const first = probes(runtime);
    assert.deepEqual(since(new Map(), first), ["read secrets"], "nothing is hidden yet");
    const running = await teamUntil(bridge, project, "a sub-agent stays running after its task",
      (status) => status.agents.some((agent) => agent.role === "sub" && ["idle", "working"].includes(agent.status)));
    const agent = running.agents.find((one) => one.role === "sub" && ["idle", "working"].includes(one.status));
    assert.ok(running.agents.every((one) => one.sees_hidden !== true), "no one is listed before a folder is hidden");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=sandbox` });
    await me.waitFor("the hidden folders section", (text) => text.includes("Folders agents can't open") && text.includes("No hidden folders."));
    await me.type("#ps-hidden-path", secrets);
    await me.click("Hide folder", "button");
    await me.click("Save hidden folders", "button");
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
      const saved = await value(bridge, "desktop_project_hidden_paths", { id: project });
      if (saved.paths.includes(secrets)) break;
      assert.ok(Date.now() < deadline, `the engine never kept the hidden folder: ${JSON.stringify(saved)}`);
    }
    const listed = await teamUntil(bridge, project, "the running sub-agent is listed as able to open the hidden folder",
      (status) => status.agents.some((one) => one.id === agent.id && one.sees_hidden === true));
    assert.deepEqual(listed.agents.filter((one) => one.sees_hidden === true).map((one) => one.id), [agent.id], "only the agent that started before the hide");
    const line = `${agent.label} started before a folder was hidden and can still open it.`;
    await me.waitFor("the section names the agent with Restart them", (text) => text.includes(line) && text.includes("Restart them"));
    await me.capture("hidden-seers-listed");

    await me.click("Restart them", "button");
    await teamUntil(bridge, project, "after Restart them no agent can open the hidden folder",
      (status) => status.agents.every((one) => one.sees_hidden !== true));
    await me.waitFor("the section no longer names the agent", (text) => !text.includes(line));
    await me.capture("hidden-seers-restarted");

    await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
    await me.unfold("Agents and tasks");
    await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
    await handOff(me, page, project, runtime);
    const added = since(first, probes(runtime));
    assert.ok(added.length > 0, "the next task ran its probe");
    assert.deepEqual([...new Set(added)], ["closed secrets"], `every run since the restart is kept out: ${JSON.stringify(added)}`);
    assert.equal(readFileSync(join(secrets, "key.txt"), "utf8"), SECRET, "the hidden file never changed");
  }, 1, startingWith(PROBE)));
