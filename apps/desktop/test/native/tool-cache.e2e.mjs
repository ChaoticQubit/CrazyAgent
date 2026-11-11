import assert from "node:assert/strict";
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { POLL_MS, STEP_MS, value } from "./e2e-harness.mjs";
import { handOff, onEngine } from "./codex-task-engine.mjs";

/** Every tool cache choice the owner can save, with the cache folder name it gives a
 * sub-agent: one per level and kind of folder, or one per agent (T8). */
const OPTIONS = [
  { scope: "shared", label: "Shared by agents with the same access", made: /^(sandboxed|own-branch)-(worktree|clone)$/ },
  { scope: "per_agent", label: "One per agent", made: /^(sandboxed|own-branch)-agent-[0-9a-f]{16}$/ },
];

/** Lists the tool caches the engine made for every project, priming records left out.
 * Input: the engine root. Output: the cache folder names. */
function caches(runtime) {
  const folder = join(runtime, "tool-cache");
  if (!existsSync(folder)) return [];
  return readdirSync(folder).flatMap((project) => readdirSync(join(folder, project)).filter((name) => !name.endsWith(".primed")));
}

for (const option of OPTIONS) {
  test(`the tool cache choice ${option.label} saves from Agent access and the next sub-agent starts with that cache`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-tool-cache-${option.scope}-`, async ({ project, runtime, me, page, site, bridge }) => {
      const before = await value(bridge, "desktop_project_agent_policy", { id: project });
      assert.equal(before.tool_cache, "shared", "a project starts Shared");
      assert.deepEqual(before.tool_caches.map((row) => [row.scope, row.label]), OPTIONS.map((row) => [row.scope, row.label]), "the engine lists every choice");
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=access` });
      await me.waitFor("the tool cache choice in Agent access", (text) => text.includes("Tool cache for uv and pnpm") && text.includes(option.label));
      if (option.scope !== before.tool_cache) {
        await me.click(option.label, "label.pj-radio-card strong");
        await me.click("Save agent access", "button");
      }
      for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
        const saved = await value(bridge, "desktop_project_agent_policy", { id: project });
        if (saved.tool_cache === option.scope) break;
        assert.ok(Date.now() < deadline, `the engine never kept ${option.scope}: ${JSON.stringify(saved)}`);
      }
      await me.capture(`tool-cache-${option.scope}`);
      const earlier = caches(runtime);
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.unfold("Agents and tasks");
      await me.waitFor("the Hand off form on the Agents tab", (text) => text.includes("Hand off"));
      await handOff(me, page, project, runtime);
      const made = caches(runtime).filter((name) => !earlier.includes(name));
      assert.ok(made.length > 0 && made.every((name) => option.made.test(name)), `the sub-agent started with the ${option.scope} cache: ${JSON.stringify(made)}`);
    }));
}
