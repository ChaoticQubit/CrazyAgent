import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) {
      return nextLoad(url, context);
    }
    const result = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(result.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: result.code, shortCircuit: true };
  },
});

const client = await import("../src/engine-client.ts");

test("skill and MCP server refusals read as plain sentences", () => {
  const fallback = client.errorMessage({ code: "unknown" });
  assert.equal(client.errorMessage({ code: "skill_missing_skill_md" }), "This folder has no SKILL.md file.");
  assert.equal(client.errorMessage({ code: "server_command_not_found" }), "Can't start: command not found.");
  assert.equal(client.errorMessage({ code: "server_changed" }), "This server changed. Review it again before approving.");
  for (const code of ["skill_name_taken", "skill_link_outside", "server_url_invalid", "server_secret_invalid", "server_secret_duplicate", "plugin_switch_global_only", "plugins_failed"]) {
    assert.notEqual(client.errorMessage({ code }), fallback, code);
  }
});

test("every plugins command the client calls is registered for the main window", () => {
  const commands = ["pluginSkills", "pluginServers", "addSkill", "setSkillEnabled", "removeSkill", "addMcpServer", "updateMcpServer", "approveMcpServer", "setMcpServerEnabled", "removeMcpServer", "switchPlugin", "setMcpSecret", "removeMcpSecret", "checkMcpServer", "projectPlugins"];
  const source = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  const capability = JSON.parse(readFileSync(new URL("../src-tauri/capabilities/main-window.json", import.meta.url), "utf8"));
  const manifest = readFileSync(new URL("../src-tauri/build.rs", import.meta.url), "utf8");
  const handlers = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const name of commands) {
    assert.equal(typeof client[name], "function", name);
    const match = source.match(new RegExp(`export function ${name}\\([^)]*\\)[^{]*\\{\\s*return invoke<[^>]+>\\("(desktop_[a-z_]+)"`));
    assert.ok(match, `${name} must invoke a desktop command`);
    const command = match[1];
    assert.ok(capability.permissions.includes(`allow-${command.replaceAll("_", "-")}`), command);
    assert.ok(manifest.includes(`"${command}"`), command);
    assert.ok(handlers.includes(`            ${command},`), command);
  }
});
