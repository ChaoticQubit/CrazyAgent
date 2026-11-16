import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const rules = await import("../src/screens/project/github-rules.ts");
const { RULE_LABELS } = await import("../src/screens/project/publish-job.ts");
const { GitHubRulesView } = await import("../src/screens/project/github-section.tsx");
const { GitHubDefaultsView } = await import("../src/screens/settings/github.tsx");
const settings = await import("../src/screens/project/settings.tsx");
const api = await import("../src/api-intake.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const snake = (name) => name.replace(/[A-Z]/g, (letter, at) => `${at ? "_" : ""}${letter.toLowerCase()}`);
const html = (text) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#x27;");
const counted = (markup, pattern) => (markup.match(pattern) ?? []).length;
const act = () => { throw new Error("static rendering must not act"); };
const ORDER = Object.keys(RULE_LABELS);
const applied = { state: "applied" };
const refused = { state: "refused", status: 403, message: "Advanced Security is off" };
const waiting = { state: "waiting", reason: "waiting for the first CI run to name its checks" };
const publicOnly = { state: "not_applicable", reason: "public repositories only" };
const file = { state: "not_applicable", reason: "a file in the project, written when it is published" };
const page = (states = {}, changes = {}) => ({
  full_name: "me/site",
  private: true,
  checks: [],
  rules: ORDER.map((rule) => ({ rule, on: states[rule]?.[0] ?? false, state: states[rule]?.[1] ?? applied })),
  ...changes,
});
const repository = (read, busy = null, problem = "") => renderToStaticMarkup(createElement(GitHubRulesView, { read, busy, problem, onRule: act, onRetry: act }));
const defaults = (read, problem = "") => renderToStaticMarkup(createElement(GitHubDefaultsView, { read, problem, onRule: act, onStandard: act, onRetry: act }));
const unreachable = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };

test("the labels follow the engine's rule order, and the standard setup is every rule but signed commits", () => {
  const engine = read("../../crates/crazyagent/src/github/rules.rs");
  const all = engine.slice(engine.indexOf("pub const ALL: [Self; 25] = ["));
  const order = [...all.slice(0, all.indexOf("];")).matchAll(/Self::([A-Z]\w+),/g)].map((match) => snake(match[1]));
  assert.equal(order.length, 25);
  assert.deepEqual(ORDER, order);
  assert.deepEqual(rules.STANDARD_RULES, order.filter((rule) => rule !== "signed_commits"));
});

test("one switch turns its rule on or off and keeps the engine's order", () => {
  assert.deepEqual(rules.withRule(["wiki_off", "squash_only"], "issues_on", true), ["squash_only", "issues_on", "wiki_off"]);
  assert.deepEqual(rules.withRule(["squash_only", "issues_on"], "squash_only", false), ["issues_on"]);
  assert.deepEqual(rules.withRule(["squash_only"], "squash_only", true), ["squash_only"]);
  assert.deepEqual(rules.withRule([], "signed_commits", false), []);
  assert.deepEqual(rules.withRule(["security_policy", "squash_only"], "signed_commits", true), ["squash_only", "signed_commits", "security_policy"]);
});

test("each rule state reads as one plain sentence that keeps GitHub's own words", () => {
  assert.equal(rules.ruleStateLine(applied), null);
  assert.equal(rules.ruleStateLine(refused), "GitHub refused this (HTTP 403): Advanced Security is off");
  assert.equal(rules.ruleStateLine({ state: "refused", status: 0, message: "a ruleset with this name exists that CrazyAgent did not make" }),
    "Not sent: a ruleset with this name exists that CrazyAgent did not make");
  assert.equal(rules.ruleStateLine(publicOnly), "Public repositories only.");
  assert.equal(rules.ruleStateLine(waiting), "Waiting for the first CI run to name its checks.");
  assert.equal(rules.ruleStateLine(file), "A file in the project, written when it is published.");
  const hidden = rules.ruleStateLine({ state: "refused", status: 422, message: "turn‮off" });
  assert.ok(hidden.startsWith("GitHub refused this (HTTP 422): turn"), hidden);
  assert.ok(!hidden.includes("‮"), "a hidden character in GitHub's message is shown, not obeyed");
});

test("a rule's switch can change only an applied or refused rule, and required checks name the checks the first CI run reported", () => {
  const shown = (rule, on, state, checks = []) => rules.ruleSwitch({ rule, on, state }, checks);
  const plain = { disabledReason: null, problem: null, help: null };
  assert.deepEqual(shown("squash_only", true, applied), { label: "Squash merge only", checked: true, ...plain });
  assert.deepEqual(shown("secret_scanning", false, refused), { label: "Secret scanning", checked: false, ...plain, problem: "GitHub refused this (HTTP 403): Advanced Security is off" });
  assert.deepEqual(shown("private_vulnerability_reporting", false, publicOnly), { label: "Private vulnerability reporting", checked: false, ...plain, disabledReason: "Public repositories only." });
  assert.deepEqual(shown("security_policy", true, file), { label: "Security policy (SECURITY.md)", checked: true, ...plain, disabledReason: "A file in the project, written when it is published." });
  assert.deepEqual(shown("required_checks", false, waiting), { label: "Required checks pass before merging", checked: false, ...plain, disabledReason: "Waiting for the first CI run to name its checks." });
  assert.deepEqual(shown("required_checks", false, applied, ["build", "test"]), { label: "Required checks pass before merging", checked: false, ...plain, help: "Checks the first CI run named: build, test." });
  assert.equal(shown("squash_only", false, applied, ["build"]).help, null, "only required checks name the checks");
});

test("the repository page shows each rule as a switch with its live state and GitHub's refusal", () => {
  const markup = repository({ kind: "ok", value: page({
    squash_only: [true, applied], secret_scanning: [false, refused], private_vulnerability_reporting: [false, publicOnly], required_checks: [false, waiting],
  }) });
  assert.equal(counted(markup, /role="switch"/g), 25);
  for (const label of Object.values(RULE_LABELS)) assert.ok(markup.includes(html(label)), label);
  assert.ok(markup.includes("me/site"), "the repository is named");
  assert.ok(markup.includes("Private"), "its visibility is named");
  assert.equal(counted(markup, /aria-checked="true"/g), 1);
  assert.equal(counted(markup, /aria-disabled="true"/g), 2);
  assert.match(markup, /role="alert"[^>]*>GitHub refused this \(HTTP 403\): Advanced Security is off</);
  assert.ok(markup.includes("Public repositories only."));
  assert.ok(markup.includes("Waiting for the first CI run to name its checks."));
});

test("once CI named its checks, the required checks switch can be turned on and names them", () => {
  const markup = repository({ kind: "ok", value: page({}, { checks: ["build", "lint"] }) });
  assert.ok(markup.includes("Checks the first CI run named: build, lint."));
  assert.equal(counted(markup, /aria-disabled="true"/g), 0);
});

test("a project CrazyAgent did not publish says so plainly, and any other failed read offers Try again", () => {
  const notPublished = repository({ kind: "error", code: "github_rules_not_published", message: api.intakeCopy("github_rules_not_published") });
  assert.equal(rules.RULES_NOT_PUBLISHED, "GitHub rules show here for a project CrazyAgent published on GitHub, once its publish is done.");
  assert.equal(api.intakeCopy("github_rules_not_published"), rules.RULES_NOT_PUBLISHED);
  assert.ok(notPublished.includes(html(rules.RULES_NOT_PUBLISHED)), notPublished);
  assert.ok(!notPublished.includes('role="alert"'), "not publishing is a state, not a failure");
  assert.ok(!notPublished.includes("Try again"));
  assert.equal(counted(notPublished, /role="switch"/g), 0);
  const failed = repository({ kind: "error", code: "github_unreachable", message: api.intakeCopy("github_unreachable") });
  assert.match(failed, /role="alert"/);
  assert.ok(failed.includes(html(api.intakeCopy("github_unreachable"))));
  assert.ok(failed.includes("Try again"));
  assert.equal(counted(failed, /role="switch"/g), 0);
});

test("while one switch is sent the page says so, that switch waits, and a failed change shows under the page", () => {
  const busy = repository({ kind: "ok", value: page() }, "secret_scanning");
  assert.ok(busy.includes("Sending Secret scanning to GitHub. This can take a few minutes."), busy);
  assert.equal(counted(busy, /aria-disabled="true"/g), 1);
  assert.match(busy, /id="github-rule-secret_scanning"[^>]*aria-disabled="true"|aria-disabled="true"[^>]*id="github-rule-secret_scanning"/);
  const failed = repository({ kind: "ok", value: page() }, null, "Couldn't reach GitHub. Check your connection and try again.");
  assert.match(failed, /role="alert"[^>]*>Couldn&#x27;t reach GitHub\. Check your connection and try again\.</);
});

test("the defaults section has one switch per rule and marks each one that differs from the standard setup", () => {
  const chosen = rules.STANDARD_RULES.filter((rule) => rule !== "wiki_off").concat("signed_commits");
  const markup = defaults({ kind: "ok", value: chosen });
  assert.equal(rules.DEFAULTS_TITLE, "New repositories");
  assert.equal(rules.DEFAULTS_ABOUT, "The rules each project CrazyAgent publishes on GitHub starts with. A publish already started keeps its own copy.");
  assert.equal(rules.STANDARD_LABEL, "Use the standard setup");
  for (const words of [rules.DEFAULTS_TITLE, rules.DEFAULTS_ABOUT, rules.STANDARD_LABEL]) assert.ok(markup.includes(html(words)), words);
  assert.equal(counted(markup, /role="switch"/g), 25);
  assert.equal(counted(markup, /aria-checked="true"/g), 24);
  assert.equal(counted(markup, /class="set-changed"/g), 2);
  assert.match(markup, /id="github-default-signed_commits"[^>]*aria-checked="true"|aria-checked="true"[^>]*id="github-default-signed_commits"/);
  const standard = defaults({ kind: "ok", value: [...rules.STANDARD_RULES] });
  assert.equal(counted(standard, /class="set-changed"/g), 0);
});

test("defaults the engine cannot read safely say so and keep only the standard setup to replace them", () => {
  assert.equal(api.DEFAULTS_UNREADABLE, "The saved defaults for new repositories can't be read safely. Use the standard setup to replace them.");
  for (const code of ["github_state_invalid", "github_config_unsafe", "github_config_invalid"]) {
    assert.deepEqual(api.defaultsRead({ kind: "error", code, message: api.intakeCopy(code) }), { kind: "error", code, message: api.DEFAULTS_UNREADABLE }, code);
  }
  const other = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };
  assert.deepEqual(api.defaultsRead(other), other);
  assert.deepEqual(api.defaultsRead({ kind: "ok", value: ["squash_only"] }), { kind: "ok", value: ["squash_only"] });
  const markup = defaults({ kind: "error", code: "github_state_invalid", message: api.DEFAULTS_UNREADABLE });
  assert.match(markup, /role="alert"/);
  assert.ok(markup.includes(html(api.DEFAULTS_UNREADABLE)));
  assert.ok(markup.includes(html(rules.STANDARD_LABEL)));
  assert.equal(counted(markup, /role="switch"/g), 0);
});

test("the four rule calls reach the engine through their own client functions, with the argument names the bridge takes", async () => {
  const client = read("src/engine-client.ts");
  assert.match(client, /export function githubRuleDefaults\(\): Promise<string\[\]> \{\s*return invoke<string\[\]>\("desktop_github_rule_defaults"\)/);
  assert.match(client, /export function githubRuleDefaultsSet\(rules: string\[\]\): Promise<string\[\]> \{\s*return invoke<string\[\]>\("desktop_github_rule_defaults_set", \{ rules \}\)/);
  assert.match(client, /export function githubRules\(project: string\): Promise<GitHubRepoRules> \{\s*return invoke<GitHubRepoRules>\("desktop_github_rules", \{ project \}\)/);
  assert.match(client, /export function githubRuleSet\(project: string, rule: string, on: boolean\): Promise<GitHubRepoRules> \{\s*return invoke<GitHubRepoRules>\("desktop_github_rule_set", \{ project, rule, on \}\)/);
  assert.match(client, /export interface GitHubRepoRules \{\s*full_name: string;\s*private: boolean;\s*checks: string\[\];\s*rules: GitHubRuleOutcome\[\];\s*\}/);
  assert.match(client, /export interface GitHubPublishJob \{[^}]*\n  rules\?: string\[\];/);
  const lib = read("src-tauri/src/lib.rs");
  for (const [command, params, output] of [
    ["desktop_github_rule_defaults", [], "BTreeSet<GitHubRule>"],
    ["desktop_github_rule_defaults_set", ["rules: BTreeSet<GitHubRule>"], "BTreeSet<GitHubRule>"],
    ["desktop_github_rules", ["project: String"], "GitHubRepoRules"],
    ["desktop_github_rule_set", ["project: String", "rule: GitHubRule", "on: bool"], "GitHubRepoRules"],
  ]) {
    const start = lib.indexOf(`async fn ${command}(`);
    assert.ok(start >= 0, command);
    const signature = lib.slice(start, lib.indexOf("{", start));
    for (const param of params) assert.ok(signature.includes(param), `${command} takes ${param}`);
    assert.ok(signature.includes(`-> Result<${output}, BridgeError>`), `${command} answers ${output}`);
  }
  const routes = read("src-tauri/e2e_bridge/routes.rs");
  for (const command of ["desktop_github_rule_defaults", "desktop_github_rule_defaults_set", "desktop_github_rules", "desktop_github_rule_set"]) {
    assert.ok(routes.includes(`"${command}" => {`), `${command} has an e2e route`);
  }
  for (const name of ["loadGitHubRuleDefaults", "saveGitHubRuleDefaults", "loadGitHubRules", "setGitHubRule"]) assert.equal(typeof api[name], "function", name);
  assert.deepEqual(await api.loadGitHubRuleDefaults(), unreachable);
  assert.deepEqual(await api.saveGitHubRuleDefaults([...rules.STANDARD_RULES]), unreachable);
  assert.deepEqual(await api.loadGitHubRules("project-1"), unreachable);
  assert.deepEqual(await api.setGitHubRule("project-1", "wiki_off", true), unreachable);
});

test("every code the rule calls can return has its own sentence and passes the desktop bridge", () => {
  const engine = ["github/settings.rs", "github/rules.rs", "github/state.rs"].map((path) => read(`../../crates/crazyagent/src/${path}`)).join("\n");
  const transport = read("../../crates/crazyagent/src/engine/owner/transport/github.rs");
  const start = transport.indexOf("fn project_folder(");
  const folder = transport.slice(start, transport.indexOf("\n}\n", start));
  const codes = new Set([...`${engine}\n${folder}`.matchAll(/"(github_[a-z_]+)"/g)].map((match) => match[1]));
  for (const match of transport.matchAll(/failure\("(github_store_unavailable)"\)/g)) codes.add(match[1]);
  codes.delete("github_owned_allowed");
  for (const code of ["github_rules_not_published", "github_rule_invalid", "github_repo_unreadable", "github_publish_project_unknown", "github_publish_unavailable", "github_store_unavailable"]) {
    assert.ok(codes.has(code), `${code} is read from the engine`);
  }
  const intake = read("src/api-intake.ts");
  const bridge = read("src-tauri/src/bridge/intake.rs");
  const allowed = bridge.slice(bridge.indexOf("const GITHUB_CODES"), bridge.indexOf("];", bridge.indexOf("const GITHUB_CODES")));
  const listed = [...allowed.matchAll(/"([a-z_]+)",/g)].map((match) => match[1]);
  assert.match(allowed, new RegExp(`^const GITHUB_CODES: \\[&str; ${listed.length}\\] = \\[`));
  for (const code of codes) {
    assert.match(intake, new RegExp(`\\n  ${code}: "[^"]+",`), `${code} has a sentence`);
    assert.ok(listed.includes(code), `${code} passes the bridge`);
  }
});

test("Project Settings has a GitHub section that reads on open and writes only from a switch, and Settings > GitHub shows the defaults", () => {
  assert.equal(settings.settingsSection("github"), "github");
  const project = read("src/screens/project/settings.tsx");
  assert.match(project, /\{ id: "github", label: "GitHub" \},\n  \{ id: "remove", label: "Remove project" \},/);
  assert.match(project, /\{active === "github" && <GitHubRulesSection data=\{data\} \/>\}/);
  const section = read("src/screens/project/github-section.tsx");
  const opened = section.slice(section.indexOf("export function GitHubRulesSection("));
  const effect = opened.slice(opened.indexOf("useEffect("), opened.indexOf("}, [data.id]);"));
  assert.ok(effect.includes("loadGitHubRules(data.id)"), "opening the section reads the live rules");
  assert.ok(!effect.includes("setGitHubRule("), "opening the section sends no change");
  assert.equal(counted(section, /setGitHubRule\(/g), 1, "one change call, from the switch handler");
  const github = read("src/screens/settings/github.tsx");
  assert.ok(!github.includes("only to list and clone"), "the sign-in now also publishes and sets rules");
  assert.ok(!github.includes("you push"), "a publish pushes for the owner");
  assert.match(github, /<GitHubDefaultsSection \/>/);
});
