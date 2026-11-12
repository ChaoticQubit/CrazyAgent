import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { handOff, onEngine, startingWith } from "./codex-task-engine.mjs";

/** The probe every sub-agent run of the stand-in Codex makes from its worktree inside the
 * owner's project folder: which project files it reads, and whether it can add to an
 * untracked top-level folder and to an untracked file at the root. It writes one line per
 * check to shared-probe.txt in its own home. */
const PROBE = String.raw`case "$1:$(/bin/pwd -P)" in
  app-server:*) ;;
  */.crazyagent/worktrees/agent-*)
    r="$(/bin/pwd -P)/../../.."
    {
      for p in README.md program/data.csv build/out.txt notes.txt .env keys/server.pem; do
        if /bin/cat "$r/$p" >/dev/null 2>&1; then echo "read $p"; else echo "closed $p"; fi
      done
      if (printf x >> "$r/program/agent.txt") 2>/dev/null; then echo "wrote program"; else echo "kept program"; fi
      if (printf x >> "$r/notes.txt") 2>/dev/null; then echo "wrote notes.txt"; else echo "kept notes.txt"; fi
    } > "$CODEX_HOME/shared-probe.txt"
    ;;
esac
`;

const DEFAULT = { scope: "all", access: "read_only", chosen: [], secrets: [] };
const SCOPES = '[aria-label="Shared project folders"] label.pj-radio-card strong';
const ACCESS = '[aria-label="Shared folders access"] label.pj-radio-card strong';

/** Every shared folders option the owner can save from Agent access (DEC-147.9), with what
 * the next sub-agent then reads, may write and finds closed. */
const OPTIONS = [
  {
    id: "all-read-only", label: "All local folders with Read-only",
    saved: DEFAULT,
    lines: ["read README.md", "read program/data.csv", "read build/out.txt", "read notes.txt", "closed .env", "closed keys/server.pem", "kept program", "kept notes.txt"],
  },
  {
    id: "chosen", label: "Chosen folders with program picked", scope: "Chosen folders", pick: "program",
    saved: { ...DEFAULT, scope: "chosen", chosen: ["program"] },
    lines: ["read README.md", "read program/data.csv", "closed build/out.txt", "closed notes.txt", "closed .env", "closed keys/server.pem", "kept program", "kept notes.txt"],
  },
  {
    id: "none", label: "None", scope: "None",
    saved: { ...DEFAULT, scope: "none" },
    lines: ["read README.md", "closed program/data.csv", "closed build/out.txt", "closed notes.txt", "closed .env", "closed keys/server.pem", "kept program", "kept notes.txt"],
  },
  {
    id: "read-write", label: "All local folders with Read-write", access: "Read-write",
    saved: { ...DEFAULT, access: "read_write" },
    lines: ["read README.md", "read program/data.csv", "read build/out.txt", "read notes.txt", "closed .env", "closed keys/server.pem", "wrote program", "kept notes.txt"],
  },
  {
    id: "shared-back", label: "a secret shared back", secrets: ".env",
    saved: { ...DEFAULT, secrets: [".env"] },
    lines: ["read README.md", "read program/data.csv", "read .env", "closed keys/server.pem", "kept program", "kept notes.txt"],
  },
];

/** Puts the owner's local files in the project folder: an untracked program folder, a build
 * folder, a note at the root and two secrets. Input: the project folder. Output: none. */
function localFiles(repo) {
  for (const [path, text] of [["program/data.csv", "a,b\n"], ["build/out.txt", "built\n"], ["notes.txt", "notes\n"], [".env", "TOKEN=E2E_SECRET_1\n"], ["keys/server.pem", "E2E_SECRET_2\n"]]) {
    mkdirSync(dirname(join(repo, path)), { recursive: true, mode: 0o700 });
    writeFileSync(join(repo, path), text, { mode: 0o600 });
  }
}

/** Reads the probe the sub-agent left in its home. Input: the engine root. Output: its lines,
 * or null before one exists. */
function probe(runtime) {
  const agents = join(runtime, "team", "agents");
  if (!existsSync(agents)) return null;
  const found = readdirSync(agents).map((name) => join(agents, name, "home", "shared-probe.txt")).filter((path) => existsSync(path));
  assert.ok(found.length <= 1, `one sub-agent probed: ${JSON.stringify(found)}`);
  return found.length === 1 ? readFileSync(found[0], "utf8").trim().split("\n") : null;
}

for (const option of OPTIONS) {
  test(`the shared folders choice ${option.label} saves from Agent access and the next sub-agent sees only what it shares`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-shared-${option.id}-`, async ({ project, runtime, me, page, site, bridge }) => {
      const repo = join(dirname(runtime), "repos", "fruit-stand");
      localFiles(repo);
      const before = await value(bridge, "desktop_project_agent_policy", { id: project });
      assert.notEqual(before.access, "full", "the project's agents run in the sandbox");
      assert.deepEqual(before.shared_folders, DEFAULT, "a project starts with All local folders and Read-only");
      assert.ok(["build", "keys", "program"].every((name) => before.local_folders.includes(name)), `the engine offers the top-level folders: ${JSON.stringify(before.local_folders)}`);
      assert.ok(!before.local_folders.includes(".crazyagent") && !before.local_folders.includes(".git"), "never the engine's folders");
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=access` });
      await me.waitFor("the shared folders choice in Agent access", (text) => text.includes("Shared project folders") && text.includes("All local folders")
        && text.includes("On Linux and Windows agents read the whole project folder"));
      let changed = false;
      if (option.scope) {
        await me.click(option.scope, SCOPES);
        changed = true;
      }
      if (option.pick) {
        await me.waitFor("the top-level folders to pick", (text) => text.includes("Top-level folders to share"));
        await me.click(option.pick, "label.pj-chip-toggle");
      }
      if (option.access) {
        await me.click(option.access, ACCESS);
        changed = true;
      }
      if (option.secrets) {
        await me.type("#ps-shared-secrets", option.secrets);
        changed = true;
      }
      if (changed) await me.click("Save agent access", "button");
      for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
        const saved = await value(bridge, "desktop_project_agent_policy", { id: project });
        if (JSON.stringify(saved.shared_folders) === JSON.stringify(option.saved)) break;
        assert.ok(Date.now() < deadline, `the engine never kept ${option.label}: ${JSON.stringify(saved.shared_folders)}`);
      }
      await me.capture(`shared-${option.id}`);
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.unfold("Agents and tasks");
      await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
      await handOff(me, page, project, runtime);
      const lines = probe(runtime);
      assert.ok(lines !== null, "the sub-agent ran its probe from its worktree");
      for (const line of option.lines) assert.ok(lines.includes(line), `${option.label}: expected "${line}" in ${JSON.stringify(lines)}`);
      assert.equal(readFileSync(join(repo, "keys", "server.pem"), "utf8"), "E2E_SECRET_2\n", "no secret changed");
      assert.equal(readFileSync(join(repo, ".env"), "utf8"), "TOKEN=E2E_SECRET_1\n", "no secret changed");
    }, 1, startingWith(PROBE)));
}
