import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const plugins = await import("../src/screens/settings/plugins.tsx");
const { SkillsView, ServersView, serverSpecInput, serverNameError, secretNameError, skillFolderError, serverWord, pluginSwitch, serverFacts, secretsInput } = plugins;

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const skill = (patch = {}) => ({ id: "skill-1", name: "frontend-design", description: "Builds polished front ends.", scope: { kind: "global" },
  scope_label: "For every project", enabled: true, on_here: null, source: "/Users/owner/skills/frontend-design",
  folder: "/Users/owner/Library/CrazyAgent/plugins/skills/skill-1", ...patch });
const server = (patch = {}) => ({ id: "server-1", name: "github", scope: { kind: "global" }, scope_label: "For every project",
  spec: { kind: "stdio", command: "/opt/homebrew/bin/npx", args: ["-y", "@modelcontextprotocol/server-github"] },
  runs: "/opt/homebrew/bin/npx -y @modelcontextprotocol/server-github", enabled: true, status: "approved", status_label: "Approved",
  problem: null, tools: [], checks: 0, secrets: [{ name: "GITHUB_TOKEN", saved: true }], on_here: null, ...patch });
const skillsProps = (skills, patch = {}) => ({ skills, failure: "", onAdd: forbidden, onToggle: forbidden, onRemove: forbidden, onRetry: forbidden, ...patch });
const serversProps = (servers, patch = {}) => ({ servers, failure: "", busy: null, onAdd: forbidden, onToggle: forbidden, onApprove: forbidden,
  onCheck: forbidden, onSecrets: forbidden, onRemove: forbidden, onRetry: forbidden, ...patch });

test("plugin refusals from the engine read as the plugin sentences, not the generic fallback", () => {
  assert.equal(api.plainError("skill_missing_skill_md"), "This folder has no SKILL.md file.");
  assert.equal(api.plainError("server_changed"), "This server changed. Review it again before approving.");
  assert.equal(api.plainError("plugin_switch_global_only"), "Only items from global settings can be switched here.");
  assert.equal(api.classifyFailure({ code: "server_secret_invalid" }).message, "Enter the secret on one line, up to 8192 characters.");
  assert.equal(api.plainError("picker_failed"), "Couldn't open the folder chooser. Type the full path instead.");
  assert.equal(api.plainError("skillful <b>"), "Something went wrong. Try again.");
});

test("the skills list shows each skill from the engine with its folder, switch and remove action", () => {
  const markup = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [skill(), skill({ id: "skill-2", name: "pdf", enabled: false })] })));
  assert.match(markup, /frontend-design/);
  assert.match(markup, /Builds polished front ends\./);
  assert.match(markup, /~\/skills\/frontend-design/);
  assert.match(markup, /On for every project/);
  assert.match(markup, /aria-checked="true"/);
  assert.match(markup, /aria-checked="false"/);
  assert.match(markup, /Add skill/);
  assert.doesNotMatch(markup, /skill-1|skill-2/);
});

test("skills show plain empty, not ready and error states and never sample data", () => {
  const empty = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [] })));
  assert.match(empty, /No skills yet/);
  assert.match(empty, /Add skill/);
  assert.match(renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "not_ready" }))), /Not ready yet/);
  assert.match(renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "error", code: "x", message: "m" }))), /Couldn&#x27;t load skills/);
  const failed = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [] }, { failure: "This folder has no SKILL.md file." })));
  assert.match(failed, /role="alert"[^>]*>.*This folder has no SKILL\.md file\./s);
});

test("a server waiting for approval shows exactly what it runs and its secrets, with Approve and Remove", () => {
  const waiting = server({ status: "waiting_for_approval", status_label: "Waiting for approval" });
  const markup = renderToStaticMarkup(createElement(ServersView, serversProps({ kind: "ok", value: [waiting] })));
  assert.match(markup, /Needs approval/);
  assert.match(markup, /Runs: <span class="mono">\/opt\/homebrew\/bin\/npx -y @modelcontextprotocol\/server-github<\/span>/);
  assert.match(markup, /GITHUB_TOKEN \(Key saved\)/);
  assert.match(markup, />Approve</);
  assert.match(markup, />Remove</);
  assert.doesNotMatch(markup, /server-1|Waiting for approval/);
});

test("server rows use the design status words, show tools and a plain reason with Check again", () => {
  assert.deepEqual(["approved", "waiting_for_approval", "off", "cant_start"].map(serverWord), ["Approved", "Needs approval", "Turned off", "Can't start"]);
  const broken = server({ status: "cant_start", status_label: "Can't start", problem: "command not found", checks: 1 });
  const checked = server({ id: "server-2", name: "files", tools: ["read_file", "list_dir"], checks: 2 });
  const markup = renderToStaticMarkup(createElement(ServersView, serversProps({ kind: "ok", value: [broken, checked] })));
  assert.match(markup, /Can&#x27;t start: command not found/);
  assert.match(markup, /Check again/);
  assert.match(markup, /Tools: read_file, list_dir/);
  const empty = renderToStaticMarkup(createElement(ServersView, serversProps({ kind: "ok", value: [] })));
  assert.match(empty, /No MCP servers yet/);
  assert.match(empty, /Add MCP server/);
  assert.match(renderToStaticMarkup(createElement(ServersView, serversProps({ kind: "not_ready" }))), /Not ready yet/);
});

test("a URL server's review says where it connects; its secrets that are not saved say so", () => {
  const remote = server({ spec: { kind: "http", url: "https://mcp.example.invalid/sse" }, runs: "https://mcp.example.invalid/sse",
    secrets: [{ name: "API_TOKEN", saved: false }], status: "waiting_for_approval" });
  assert.deepEqual(serverFacts(remote), ["Connects to: https://mcp.example.invalid/sse", "Secrets: API_TOKEN (Not saved)", "For every project"]);
  assert.deepEqual(serverFacts(server({ secrets: [] })).slice(1), ["Secrets: none", "For every project"]);
});

test("switches follow the engine: global items in global settings, the per-project switch in a project", () => {
  assert.deepEqual(pluginSwitch(skill({ enabled: false })), { checked: false, label: "On for every project", project: false });
  assert.deepEqual(pluginSwitch(skill({ on_here: false }), "project-1"), { checked: false, label: "On in this project", project: true });
  assert.deepEqual(pluginSwitch(skill({ scope: { kind: "project", project_id: "project-1" }, scope_label: "This project only" }), "project-1"),
    { checked: true, label: "On", project: false });
});

test("the Add MCP server form turns typed text into the engine's spec and refuses what the engine would", () => {
  assert.deepEqual(serverSpecInput({ kind: "stdio", command: " /usr/local/bin/uvx ", args: "mcp-server-git\n\n--repository\r\n/Users/owner/code\n", url: "" }),
    { spec: { kind: "stdio", command: "/usr/local/bin/uvx", args: ["mcp-server-git", "--repository", "/Users/owner/code"] } });
  assert.deepEqual(serverSpecInput({ kind: "http", command: "", args: "", url: " https://mcp.example.invalid/sse " }),
    { spec: { kind: "http", url: "https://mcp.example.invalid/sse" } });
  assert.deepEqual(serverSpecInput({ kind: "stdio", command: "uvx", args: "", url: "" }), { error: { command: "Enter the full path to the command, starting with /." } });
  const many = Array.from({ length: 33 }, (_, index) => `arg${index}`).join("\n");
  assert.deepEqual(serverSpecInput({ kind: "stdio", command: "/bin/x", args: many, url: "" }), { error: { args: "Use up to 32 arguments, each on one line." } });
  for (const url of ["http://example.com/mcp", "ftp://x", "https://user:pass@example.com", ""]) {
    assert.ok("error" in serverSpecInput({ kind: "http", command: "", args: "", url }), url);
  }
  assert.ok("spec" in serverSpecInput({ kind: "http", command: "", args: "", url: "http://127.0.0.1:8931/mcp" }));
});

test("names, secret names and skill folders are checked with the engine's rules before sending", () => {
  assert.equal(serverNameError("github"), undefined);
  assert.equal(serverNameError("GitHub tools"), "Use 1 to 64 lowercase letters, digits, - or _ for the name.");
  assert.equal(serverNameError("x".repeat(65)), "Use 1 to 64 lowercase letters, digits, - or _ for the name.");
  assert.equal(secretNameError("GITHUB_TOKEN"), undefined);
  assert.equal(secretNameError("1TOKEN"), "Use letters, digits, _ or - for the secret name, starting with a letter.");
  assert.equal(skillFolderError("/Users/owner/skills/pdf"), undefined);
  assert.equal(skillFolderError("skills/pdf"), "Enter the full path of a folder that holds SKILL.md.");
  assert.equal(skillFolderError(""), "Choose a folder that holds SKILL.md.");
});

test("secret rows become name and value pairs once, with duplicates and empty values refused", () => {
  assert.deepEqual(secretsInput([{ name: "A_TOKEN", value: "one" }, { name: "B", value: "two" }]), { secrets: [["A_TOKEN", "one"], ["B", "two"]] });
  assert.deepEqual(secretsInput([{ name: "", value: "" }]), { secrets: [] });
  assert.deepEqual(secretsInput([{ name: "A", value: "one" }, { name: "A", value: "two" }]), { error: "Each secret needs its own name." });
  assert.deepEqual(secretsInput([{ name: "A", value: "" }]), { error: "Enter a value for A." });
  assert.deepEqual(secretsInput([{ name: "", value: "orphan" }]), { error: "Use letters, digits, _ or - for the secret name, starting with a letter." });
  const seventeen = Array.from({ length: 17 }, (_, index) => ({ name: `S${index}`, value: "v" }));
  assert.deepEqual(secretsInput(seventeen), { error: "A server can have up to 16 secrets." });
});

test("every plugins and folder chooser command the settings call is registered in all four native places", async () => {
  const { readFileSync } = await import("node:fs");
  const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
  const source = read("src/api.ts");
  const build = read("src-tauri/build.rs");
  const lib = read("src-tauri/src/lib.rs");
  const handler = lib.slice(lib.indexOf("generate_handler!["));
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json")).permissions;
  const commands = [...source.matchAll(/call<[^(]*>\("(desktop_(?:plugin|add_skill|set_skill|remove_skill|add_mcp|approve_mcp|set_mcp|remove_mcp|check_mcp|switch_plugin|pick)[a-z_]*)"/g)].map((match) => match[1]);
  assert.equal(new Set(commands).size, 14);
  for (const command of commands) {
    assert.ok(build.includes(`"${command}"`), `${command} missing from build.rs`);
    assert.ok(allowed.includes(`allow-${command.replaceAll("_", "-")}`), `${command} missing from main-window.json`);
    assert.match(lib, new RegExp(`#\\[tauri::command\\]\\s*(?:async )?fn ${command}\\(`), `${command} has no tauri command`);
    assert.match(handler, new RegExp(`\\b${command},`), `${command} missing from invoke_handler`);
  }
});

test("refusals from the native open commands read as plain sentences", () => {
  for (const code of ["link_not_allowed", "open_failed", "project_folder_missing", "context_item_not_openable", "picker_failed"]) {
    assert.notEqual(api.plainError(code), "Something went wrong. Try again.", code);
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
});
