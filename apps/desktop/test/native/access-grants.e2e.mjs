import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { handOff, onEngine } from "./codex-task-engine.mjs";

const REQUESTED_PORT = 6543;
const PROFILE_MS = 120000;

/** Every Apply access changes choice the owner can save, with the label Agent access shows (F08). */
const APPLY = [
  { apply: "when_idle", label: "When the agent is idle" },
  { apply: "next_start", label: "At its next start" },
];

/** A stand-in Codex that, on its first turn, calls request_access through its own
 * crazyagent-team MCP server, read from the config the engine wrote, then ends the turn as
 * the counting stand-in does (F08). */
const REQUESTING_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/11"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-11T00-00-00-$id.jsonl"
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
  if [ ! -e "$CODEX_HOME/.requested" ]; then
    : > "$CODEX_HOME/.requested"
    cfg="$CODEX_HOME/config.toml"
    cmd=$(/usr/bin/sed -n '/^\[mcp_servers\.crazyagent-team\]/,/^\[/s/^command = "\(.*\)"$/\1/p' "$cfg")
    args=$(/usr/bin/sed -n '/^\[mcp_servers\.crazyagent-team\]/,/^\[/s/^args = \[\(.*\)\]$/\1/p' "$cfg" | /usr/bin/tr -d '",')
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stand-in","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"request_access","arguments":{"kind":"port","port":6543,"reason":"the e2e database"}}}' | "$cmd" $args >> "$CODEX_HOME/request-access.log" 2>&1
  fi
  printf '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":9200,"cached_input_tokens":8000,"output_tokens":300,"total_tokens":9500}}}}\n' >> "$f"
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
`;

/** Opens the project's Agent access page and waits for its extras. Inputs: the page, the
 * site, the owner and the project. Output: once shown. */
async function openAccess(page, site, me, project) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=access` });
  await me.waitFor("the allowed extras in Agent access", (text) => text.includes("Allowed extras") && text.includes("Apply access changes"));
}

/** Waits until the engine's saved policy passes a check. Inputs: the bridge, the project, what
 * is awaited and the check. Output: the saved view. */
async function saved(bridge, project, what, check) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const view = await value(bridge, "desktop_project_agent_policy", { id: project });
    if (check(view)) return view;
    assert.ok(Date.now() < deadline, `the engine never kept ${what}: ${JSON.stringify(view)}`);
  }
}

/** Hands off one task and reads the Seatbelt profile of the agent that started with a rule.
 * Inputs: the owner, the page, the site, the project, the engine root and the rule awaited.
 * Output: that profile's text. */
async function profileAfterHandOff(me, page, site, project, runtime, rule) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
  await me.unfold("Agents and tasks");
  await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
  assert.equal(await handOff(me, page, project, runtime), "done", "the sub-agent ran with the saved extra");
  const folder = join(runtime, "team", "profiles");
  for (const deadline = Date.now() + PROFILE_MS; ; await pause(POLL_MS)) {
    const profiles = existsSync(folder) ? readdirSync(folder).filter((name) => /^agent-\d+\.sb$/.test(name)).map((name) => readFileSync(join(folder, name), "utf8")) : [];
    const found = profiles.find((text) => text.includes(rule));
    if (found) return found;
    assert.ok(Date.now() < deadline, `no agent profile holds ${rule}`);
  }
}

for (const option of APPLY) {
  test(`Apply access changes ${option.label} saves from Agent access`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-apply-${option.apply}-`, async ({ project, me, page, site, bridge }) => {
      const before = await value(bridge, "desktop_project_agent_policy", { id: project });
      assert.equal(before.apply_changes, "when_idle", "a project starts with When the agent is idle");
      assert.deepEqual(before.apply_choices.map((row) => [row.apply, row.label]), APPLY.map((row) => [row.apply, row.label]), "the engine lists every choice");
      await openAccess(page, site, me, project);
      if (option.apply !== before.apply_changes) {
        await me.click(option.label, "label.pj-radio-card strong");
        await me.click("Save agent access", "button");
      }
      await saved(bridge, project, option.apply, (view) => view.apply_changes === option.apply);
      await me.capture(`apply-${option.apply}`);
    }));
}

test("a port extra saves from Agent access, is logged as the owner's and reaches a limited network on loopback", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-grant-port-", async ({ project, runtime, me, page, site, bridge }) => {
    await openAccess(page, site, me, project);
    await me.click("None", "label.pj-radio-card strong");
    await me.click("Port", "label.pj-radio-card strong");
    await me.type(`#ps-grant-value`, "5432");
    await me.click("Add port", "button");
    await me.waitFor("the port in the extras", (text) => text.includes("Port 5432 on this computer"));
    await me.click("Save agent access", "button");
    const view = await saved(bridge, project, "the port extra", (row) => row.grants?.some((held) => held.grant.kind === "port" && held.grant.port === 5432));
    assert.equal(view.network, "none");
    assert.deepEqual([view.grant_log[0].action, view.grant_log[0].grant, view.grant_log[0].by], ["granted", "Port 5432 on this computer", "owner"]);
    await me.capture("grant-port");
    await profileAfterHandOff(me, page, site, project, runtime, '(allow network-outbound (remote ip "localhost:5432"))');
  }));

test("a socket extra saves canonical from Agent access and the next sub-agent may connect to it only", { timeout: 600000 }, (t) => {
  const folder = realpathSync(mkdtempSync(join(tmpdir(), "ca-sock-")));
  const path = join(folder, "app.sock");
  const server = createServer(() => {}).listen(path);
  t.after(() => { server.close(); rmSync(folder, { recursive: true, force: true }); });
  return onEngine(t, "ca-grant-socket-", async ({ project, runtime, me, page, site, bridge }) => {
    await openAccess(page, site, me, project);
    await me.click("Socket", "label.pj-radio-card strong");
    await me.type(`#ps-grant-value`, path);
    await me.click("Add socket", "button");
    await me.click("Save agent access", "button");
    await saved(bridge, project, "the socket extra", (view) => view.grants?.some((held) => held.grant.kind === "socket" && held.grant.path === path));
    await me.capture("grant-socket");
    const profile = await profileAfterHandOff(me, page, site, project, runtime, `(allow network-outbound (remote unix-socket (path-literal "${path}")))`);
    assert.ok(!profile.includes(`(allow network-bind (local unix-socket (path-literal "${path}")))`), "it may never serve on it");
  });
});

test("the temp folder extra saves from Agent access and the next sub-agent uses the owner's temp folder with its sockets kept", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-grant-temp-", async ({ project, runtime, me, page, site, bridge }) => {
    const before = await value(bridge, "desktop_project_agent_policy", { id: project });
    await openAccess(page, site, me, project);
    if (before.temp_folder.unavailable !== null) {
      await me.waitFor("why the temp folder cannot be given", (text) => text.includes(before.temp_folder.unavailable));
      return;
    }
    await me.click("Allow temp folder", "button");
    await me.click("Save agent access", "button");
    await saved(bridge, project, "the temp folder extra", (view) => view.grants?.some((held) => held.grant.kind === "temp_folder"));
    await me.capture("grant-temp-folder");
    await profileAfterHandOff(me, page, site, project, runtime, `(subpath "${before.temp_folder.path}")\n    (vnode-type SOCKET)`);
  }, 1, undefined, { ownTemp: true }));

for (const allow of [true, false]) {
  test(`an agent's access request the owner ${allow ? "allows" : "does not allow"} ${allow ? "joins Agent access" : "adds nothing"} and is logged`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-request-${allow ? "allow" : "deny"}-`, async ({ project, runtime, me, page, site, bridge }) => {
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.unfold("Agents and tasks");
      await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
      await handOff(me, page, project, runtime);
      let asked;
      for (const deadline = Date.now() + STEP_MS * 4; ; await pause(POLL_MS)) {
        const items = (await value(bridge, "desktop_questions", { projectId: project, after: null })).items;
        asked = items.find((item) => item.asker.startsWith("engine:access:") && item.status === "pending");
        if (asked) break;
        assert.ok(Date.now() < deadline, `no access request reached the owner's questions: ${JSON.stringify(items)}`);
      }
      assert.ok(asked.question.includes(`Port ${REQUESTED_PORT} on this computer`), asked.question);
      assert.deepEqual(asked.options, ["Allow", "Don't allow"]);
      assert.equal(asked.recommended, "Don't allow");
      assert.ok(asked.context.includes('"the e2e database"'), "the agent's reason shows quoted");
      await value(bridge, "desktop_answer_question", { id: asked.id, version: asked.version, picks: [allow ? 0 : 1], comment: null });
      const view = await saved(bridge, project, allow ? "the allowed port" : "the denied request", (row) => row.grant_log?.some((logged) => logged.action === (allow ? "allowed" : "denied")));
      const held = view.grants.some((row) => row.grant.kind === "port" && row.grant.port === REQUESTED_PORT);
      assert.equal(held, allow, "only an Allow adds the extra");
      assert.equal(view.grant_log[0].by, `question ${asked.id}`);
      await openAccess(page, site, me, project);
      await me.waitFor("the extras log", (text) => text.includes("Extras log") && text.includes(allow ? "Allowed on request" : "Not allowed on request"));
      await me.capture(`request-${allow ? "allow" : "deny"}`);
    }, 1, REQUESTING_CODEX));
}
