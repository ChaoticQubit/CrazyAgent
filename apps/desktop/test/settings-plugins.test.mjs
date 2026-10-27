import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const plugins = await import("../src/screens/settings/plugins.tsx");
const { SkillsView, SkillDetailView, ServersView, serverSpecInput, serverNameError, secretNameError, skillFolderError, serverWord, pluginSwitch, serverFacts, secretsInput,
  skillBody, fileSize, ImportSkillsView, importable, importWord, importSentence } = plugins;
const router = await import("../src/ui/router.ts");

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const skill = (patch = {}) => ({ id: "skill-1", name: "frontend-design", description: "Builds polished front ends.", line: "Builds polished front ends.", scope: { kind: "global" },
  scope_label: "For every project", enabled: true, on_here: null, source: "/Users/owner/skills/frontend-design",
  folder: "/Users/owner/Library/CrazyAgent/plugins/skills/skill-1", ...patch });
const server = (patch = {}) => ({ id: "server-1", name: "github", scope: { kind: "global" }, scope_label: "For every project",
  spec: { kind: "stdio", command: "/opt/homebrew/bin/npx", args: ["-y", "@modelcontextprotocol/server-github"] },
  runs: "/opt/homebrew/bin/npx -y @modelcontextprotocol/server-github", enabled: true, status: "approved", status_label: "Approved",
  problem: null, tools: [], checks: 0, secrets: [{ name: "GITHUB_TOKEN", saved: true }], on_here: null, ...patch });
const skillsProps = (skills, patch = {}) => ({ skills, failure: "", onAdd: forbidden, onOpen: forbidden, onToggle: forbidden, onRemove: forbidden, onRetry: forbidden, ...patch });
const detail = (patch = {}) => ({ skill: skill(), text: "---\nname: frontend-design\ndescription: Builds polished front ends.\n---\n\n# Steps\n\n- Read the brief\n- Ship `npm run build`\n",
  files: [{ path: "SKILL.md", bytes: 812 }, { path: "references/tokens.md", bytes: 4300 }], files_more: 0, ...patch });
const detailProps = (value, patch = {}) => ({ detail: value, failure: "", onToggle: forbidden, onOpenLink: forbidden, onRetry: forbidden, ...patch });
const serversProps = (servers, patch = {}) => ({ servers, failure: "", busy: null, onAdd: forbidden, onToggle: forbidden, onApprove: forbidden,
  onCheck: forbidden, onSecrets: forbidden, onRemove: forbidden, onRetry: forbidden, ...patch });

test("plugin refusals from the engine read as the plugin sentences, not the generic fallback", () => {
  assert.equal(api.plainError("skill_missing_skill_md"), "This folder has no SKILL.md file.");
  assert.equal(api.plainError("server_changed"), "This server changed. Review it again before approving.");
  assert.equal(api.plainError("plugin_switch_global_only"), "Only items from global settings can be switched here.");
  assert.equal(api.classifyFailure({ code: "server_secret_invalid" }).message, "Enter the secret on one line, up to 8192 characters.");
  assert.equal(api.plainError("picker_failed"), "Couldn't open the folder chooser. Type the full path instead.");
  assert.equal(api.plainError("skillful <b>"), "Something went wrong. Try again. (unknown)");
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
  assert.match(markup, /aria-label="Open frontend-design"/);
  assert.match(markup, /aria-label="Open pdf"/);
  assert.doesNotMatch(markup, /skill-1|skill-2/);
});

test("a skill's detail renders its SKILL.md without the frontmatter, its files and its switch", () => {
  const markup = renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "ok", value: detail() })));
  assert.match(markup, /<h2>Steps<\/h2>/);
  assert.match(markup, /<li>Read the brief<\/li>/);
  assert.match(markup, /<code>npm run build<\/code>/);
  assert.doesNotMatch(markup, /name: frontend-design/);
  assert.match(markup, /Builds polished front ends\./);
  assert.match(markup, /For every project/);
  assert.match(markup, /~\/skills\/frontend-design/);
  assert.match(markup, /Files \(2\)/);
  assert.match(markup, /references\/tokens\.md/);
  assert.match(markup, /4\.2 KB/);
  assert.match(markup, /812 B/);
  assert.match(markup, /id="skill-detail-on"[^>]*aria-checked="true"|aria-checked="true"[^>]*id="skill-detail-on"/);
  assert.match(markup, /On for every project/);
  assert.doesNotMatch(markup, /skill-1/);
});

test("a skill's detail in a project uses the project switch and says when Settings turned it off", () => {
  const off = detail({ skill: skill({ enabled: false, on_here: true }), files_more: 3 });
  const markup = renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "ok", value: off }, { projectId: "project-1" })));
  assert.match(markup, /On in this project/);
  assert.match(markup, /Turned off for every project in Settings\./);
  assert.match(markup, /Files \(5\)/);
  assert.match(markup, /And 3 more/);
  const html = renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "ok", value: detail({ text: "<script>alert(1)</script>" }) })));
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /&lt;script&gt;/);
});

test("a skill's detail shows plain loading, not ready and error states", () => {
  assert.match(renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "loading" }))), /skeleton/);
  assert.match(renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "not_ready" }))), /Not ready yet/);
  const failed = renderToStaticMarkup(createElement(SkillDetailView, detailProps({ kind: "error", code: "skill_not_found", message: "This skill was removed." })));
  assert.match(failed, /role="alert"/);
  assert.match(failed, /This skill was removed\./);
  assert.equal(api.plainError("skill_text_too_large"), "This SKILL.md is too large to show here.");
  assert.equal(api.plainError("skill_page_invalid"), "This skill changed while it was read. Open it again.");
});

test("the SKILL.md body drops only a closed frontmatter block, and sizes read plainly", () => {
  assert.equal(skillBody("---\nname: a\ndescription: b\n---\n\n\nBody\n"), "Body\n");
  assert.equal(skillBody("\uFEFF---\nname: a\n---\nBody"), "Body");
  assert.equal(skillBody("No frontmatter\n---\nrule"), "No frontmatter\n---\nrule");
  assert.equal(skillBody("---\nnever closed"), "---\nnever closed");
  assert.deepEqual([0, 1023, 1024, 4300, 1048576, 3 * 1048576 + 1].map(fileSize), ["0 B", "1023 B", "1.0 KB", "4.2 KB", "1.0 MB", "3.0 MB"]);
});

test("an open skill is part of the Plugins and skills route, so Settings links can open it", () => {
  const route = { name: "settings", tab: "plugins", skill: "skill-0a1b" };
  assert.equal(router.formatRoute(route), "#/settings/plugins?skill=skill-0a1b");
  assert.deepEqual(router.parseRoute("#/settings/plugins?skill=skill-0a1b"), route);
  assert.deepEqual(router.parseRoute("#/settings/plugins?skill=BAD%20id"), { name: "settings", tab: "plugins" });
  assert.deepEqual(router.parseRoute("#/settings/pools?skill=skill-0a1b"), { name: "settings", tab: "pools" });
  assert.equal(router.formatRoute({ name: "settings", tab: "pools", skill: "skill-0a1b" }), "#/settings/pools");
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
  const commands = [...source.matchAll(/call<[^(]*>\("(desktop_(?:plugin|add_skill|set_skill|remove_skill|skill_detail|skill_import|import_skill|add_mcp|approve_mcp|set_mcp|remove_mcp|check_mcp|switch_plugin|pick)[a-z_]*)"/g)].map((match) => match[1]);
  assert.equal(new Set(commands).size, 17);
  assert.ok(commands.includes("desktop_skill_import_scan") && commands.includes("desktop_import_skill"));
  for (const command of commands) {
    assert.ok(build.includes(`"${command}"`), `${command} missing from build.rs`);
    assert.ok(allowed.includes(`allow-${command.replaceAll("_", "-")}`), `${command} missing from main-window.json`);
    assert.match(lib, new RegExp(`#\\[tauri::command\\]\\s*(?:async )?fn ${command}\\(`), `${command} has no tauri command`);
    assert.match(handler, new RegExp(`\\b${command},`), `${command} missing from invoke_handler`);
  }
});

test("refusals from the native open commands read as plain sentences", () => {
  for (const code of ["link_not_allowed", "open_failed", "project_folder_missing", "context_item_not_openable", "picker_failed"]) {
    assert.doesNotMatch(api.plainError(code), / \([a-z0-9_]+\)$/, code);
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
});

const found = (patch = {}) => ({ from: "claude", folder: "/Users/owner/.claude/skills/notes", name: "notes", line: "Writes release notes.", state: { kind: "new" }, ...patch });
const importProps = (list, patch = {}) => ({ list, ticked: new Set(), outcomes: [], onTick: forbidden, onRetry: forbidden, ...patch });

test("Import my skills is offered in global settings only, next to Add skill", () => {
  const global = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [skill()] }, { onImport: forbidden })));
  assert.match(global, /Import my skills/);
  assert.match(global, /Add skill/);
  const empty = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [] }, { onImport: forbidden })));
  assert.match(empty, /Import my skills/);
  const project = renderToStaticMarkup(createElement(SkillsView, skillsProps({ kind: "ok", value: [skill()] }, { projectId: "app" })));
  assert.doesNotMatch(project, /Import my skills/);
});

test("found skills show their source and state, and only new or differing ones can be ticked", () => {
  const value = { found: [
    found(),
    found({ from: "codex", folder: "/Users/owner/.codex/skills/graph", name: "graph", line: "Builds a code graph.", state: { kind: "added", id: "skill-9" } }),
    found({ folder: "/Users/owner/.claude/skills/style", name: "style", line: "House style.", state: { kind: "differs", id: "skill-3" } }),
    found({ folder: "/Users/owner/.claude/skills/heavy", name: "heavy", line: "", state: { kind: "unusable", code: "skill_import_too_large" } }),
  ], more: 2 };
  const markup = renderToStaticMarkup(createElement(ImportSkillsView, importProps({ kind: "ok", value }, { ticked: new Set(["/Users/owner/.claude/skills/notes"]) })));
  assert.match(markup, /From ~\/\.claude\/skills and ~\/\.codex\/skills/);
  assert.match(markup, /Claude Code: ~\/\.claude\/skills\/notes/);
  assert.match(markup, /Codex: ~\/\.codex\/skills\/graph/);
  assert.match(markup, /Writes release notes\./);
  assert.match(markup, /Already added/);
  assert.match(markup, /Differs/);
  assert.match(markup, /Importing replaces the added copy and keeps its project switches\./);
  assert.match(markup, /Can&#x27;t import/);
  assert.match(markup, /A skill brought in from your skills folders can be up to 2 MB\./);
  assert.match(markup, /And 2 more not shown/);
  const boxes = [...markup.matchAll(/<input type="checkbox"([^>]*)\/>/g)].map((match) => match[1]);
  assert.equal(boxes.length, 4);
  assert.match(boxes[0], /checked=""/);
  assert.doesNotMatch(boxes[0], /disabled/);
  assert.match(boxes[1], /disabled=""/);
  assert.doesNotMatch(boxes[2], /disabled/);
  assert.match(boxes[3], /disabled=""/);
  assert.doesNotMatch(markup, /skill-9|skill-3/);
  assert.deepEqual([{ kind: "new" }, { kind: "differs", id: "a" }, { kind: "added", id: "a" }, { kind: "unusable", code: "x" }].map(importable), [true, true, false, false]);
  assert.deepEqual([{ kind: "new" }, { kind: "added", id: "a" }].map(importWord), ["", "Already added"]);
});

test("the import's outcomes say what was left out and why a skill was not imported", () => {
  const imported = (skipped, total) => ({ kind: "ok", value: { skill: skill({ name: "notes" }), skipped, skipped_total: total } });
  assert.equal(importSentence({ name: "notes", result: imported([], 0) }), "Imported notes.");
  assert.equal(importSentence({ name: "notes", result: imported([".env", "api_key.txt"], 2) }), "Imported notes. Left out .env, api_key.txt.");
  assert.equal(importSentence({ name: "notes", result: imported([".env"], 3) }), "Imported notes. Left out .env and 2 more.");
  const refused = { kind: "error", code: "skill_link_outside", message: api.plainError("skill_link_outside") };
  assert.equal(importSentence({ name: "leaky", result: refused }), "Couldn't import leaky: This folder links to files outside it.");
  const markup = renderToStaticMarkup(createElement(ImportSkillsView, importProps({ kind: "ok", value: { found: [], more: 0 } },
    { outcomes: [{ name: "notes", result: imported([".env"], 1) }, { name: "leaky", result: refused }] })));
  assert.match(markup, /Imported notes\. Left out \.env\./);
  assert.match(markup, /Couldn&#x27;t import leaky/);
  assert.match(markup, /No skills found in those folders/);
  assert.equal(api.plainError("skill_import_name_changed"), "This folder now holds a different skill. Look for your skills again.");
});

test("the import list shows plain loading, not ready and error states", () => {
  assert.match(renderToStaticMarkup(createElement(ImportSkillsView, importProps({ kind: "loading" }))), /skeleton/);
  assert.match(renderToStaticMarkup(createElement(ImportSkillsView, importProps({ kind: "not_ready" }))), /Importing skills/);
  const failed = renderToStaticMarkup(createElement(ImportSkillsView, importProps({ kind: "error", code: "skill_import_home_unknown", message: api.plainError("skill_import_home_unknown") })));
  assert.match(failed, /Your home folder is unknown, so your skills can&#x27;t be found\./);
});
