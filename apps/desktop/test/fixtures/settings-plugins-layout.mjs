import { createElement } from "react";
import { Settings } from "../../src/screens/settings/index.tsx";
import { PluginsAndSkills } from "../../src/screens/settings/plugins.tsx";
import { mountTab, sceneRead } from "./settings-tab.mjs";

export * from "./settings-tab.mjs";

const FAILURE = { code: "engine_failed" };
const skill = (patch) => ({ id: "skill-1", name: "frontend-design", description: "Builds polished front ends.", line: "Builds polished front ends.", scope: { kind: "global" }, scope_label: "For every project",
  enabled: true, on_here: null, version: 0, source: "/Users/owner/skills/frontend-design", folder: "/Users/owner/plugins/skill-1", refused: null, ...patch });
const server = (patch) => ({ id: "server-1", name: "github", scope: { kind: "global" }, scope_label: "For every project", spec: { kind: "stdio", command: "/bin/npx", args: ["-y", "github"] }, runs: "/bin/npx -y github",
  enabled: true, status: "approved", status_label: "Approved", problem: null, tools: ["echo", "anonymous"], checks: 1, secrets: [{ name: "GITHUB_TOKEN", saved: true }], on_here: null, version: 0, ...patch });
const hook = (patch) => ({ id: "hook-1", name: "caveman", plugin: "caveman", event: "SessionStart", matcher: "startup", command: "node hook.js", scope: { kind: "global" }, scope_label: "For every project",
  enabled: true, on_here: null, follows: "the caveman skill", source: "/Users/owner/plugins/caveman", version: 0, ...patch });
const tool = (patch) => ({ server: "crazyagent-team", server_id: null, tool: "start_work", description: "Start a unit of work.", builtin: true, global: "on", here: null, ...patch });
const SKILLS = { ok: [skill({}), skill({ id: "skill-2", name: "pdf", refused: "skill_description_hidden" })], here: [skill({ on_here: false }), skill({ id: "skill-2", name: "pdf" })], empty: [] };
const SERVERS = {
  ok: [server({}), server({ id: "server-2", name: "waiting", status: "waiting_for_approval", status_label: "Waiting for approval", tools: [], runs: "/bin/wait now" }),
    server({ id: "server-3", name: "broken", status: "cant_start", status_label: "Can't start", problem: "the server exited.", tools: [], secrets: [] }),
    server({ id: "server-4", name: "remote", spec: { kind: "http", url: "https://mcp.example.invalid/sse" }, runs: "https://mcp.example.invalid/sse", status: "waiting_for_approval", status_label: "Waiting for approval", tools: [], secrets: [] })],
  here: [server({ on_here: false }), server({ id: "server-2", name: "plain" })],
  empty: [],
};
const HOOKS = { ok: [hook({}), hook({ id: "hook-2", name: "adhd", plugin: "adhd", matcher: null, follows: null })], here: [hook({ on_here: false }), hook({ id: "hook-2", name: "adhd", plugin: "adhd" })], empty: [] };
const TOOLS = { ok: [tool({}), tool({ tool: "graphify", description: "", global: "off" }), tool({ tool: "ctx_batch", global: "hidden" })], here: [tool({ here: "off" }), tool({ tool: "graphify", global: "off" })], empty: [] };
const MCP_TOOLS = [tool({ server: "files", server_id: "server-1", tool: "echo", description: "", builtin: false })];

/** Answers one engine command for a scene. Inputs: the scene, the command and its arguments.
 * Output: the answer, or a failure thrown. */
function answer(scene, command, args) {
  if (scene.hold === command) return new Promise(() => {});
  if (command === "desktop_plugin_skills") return sceneRead(scene.skills ?? "ok", SKILLS, FAILURE, command);
  if (command === "desktop_plugin_servers") return sceneRead(scene.servers ?? "ok", SERVERS, FAILURE, command);
  if (command === "desktop_plugin_hooks") return sceneRead(scene.hooks ?? "ok", HOOKS, FAILURE, command);
  if (command === "desktop_tool_switches") return args.serverId === null ? sceneRead(scene.tools ?? "ok", TOOLS, FAILURE, command) : MCP_TOOLS;
  if (/^desktop_(remove_skill|remove_mcp_server|approve_mcp_server|check_mcp_server|switch_plugin|set_skill_enabled|set_tool_switch)$/.test(command)) throw FAILURE;
  throw `Command ${command} not found`;
}

/** Mounts Plugins and skills against a fake engine, on the Settings page or, when the scene
 * names a project, as the project page shows it. Inputs: the scene, naming each read as ok,
 * empty, error, missing or pending, and the theme. Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  const element = scene.project ? createElement(PluginsAndSkills, { projectId: scene.project }) : createElement(Settings, { tab: "plugins" });
  await mountTab(element, (command, args) => answer(scene, command, args), theme);
}
