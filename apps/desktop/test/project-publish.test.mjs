import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const publishJob = await import("../src/screens/project/publish-job.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const snake = (name) => name.replace(/[A-Z]/g, (letter, at) => `${at ? "_" : ""}${letter.toLowerCase()}`);

test("every engine rule has its own plain label, the same words as the TUI", () => {
  const rules = read("../../crates/crazyagent/src/github/rules.rs");
  const body = rules.slice(rules.indexOf("pub enum GitHubRule {"));
  const variants = [...body.slice(0, body.indexOf("}")).matchAll(/^\s+([A-Z]\w+),$/gm)].map((match) => snake(match[1]));
  assert.equal(variants.length, 25);
  assert.deepEqual(Object.keys(publishJob.RULE_LABELS).sort(), [...variants].sort());
  const labels = Object.values(publishJob.RULE_LABELS);
  assert.equal(new Set(labels).size, labels.length);
  for (const label of labels) {
    assert.ok(label.length > 0 && !label.includes("_") && !label.endsWith("."), label);
  }
  const tui = read("../../crates/crazyagent/src/tui/screens/project_publish.rs");
  const shown = Object.fromEntries([...tui.matchAll(/GitHubRule::(\w+) => "([^"]+)"/g)].map((match) => [snake(match[1]), match[2]]));
  assert.deepEqual(shown, publishJob.RULE_LABELS);
});

const api = await import("../src/api-intake.ts");
const job = (step, changes = {}) => ({ project: "/p", plan: { owner: "me", name: "tool", description: "", private: true, docs: false }, lines: [], written: [], step, stopped: null, made: null, ...changes });
const unreachable = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };

test("the publish actions reach the engine through six bridge commands and come back as plain results", async () => {
  const client = read("src/engine-client.ts");
  for (const command of ["status", "approve", "exclude", "recheck", "continue", "discard"]) {
    assert.match(client, new RegExp(`invoke<[^>]+>\\("desktop_github_publish_${command}", \\{ project`), command);
  }
  for (const name of ["loadGitHubPublish", "approveGitHubPublish", "excludeFromGitHubPublish", "recheckGitHubPublish", "continueGitHubPublish", "discardGitHubPublish"]) {
    assert.equal(typeof api[name], "function", name);
  }
  assert.deepEqual(await api.loadGitHubPublish("p"), unreachable);
  assert.deepEqual(await api.approveGitHubPublish("p", [".env*"]), unreachable);
  assert.deepEqual(await api.excludeFromGitHubPublish("p", "keys.txt"), unreachable);
  assert.deepEqual(await api.recheckGitHubPublish("p"), unreachable);
  assert.deepEqual(await api.continueGitHubPublish("p"), unreachable);
  assert.deepEqual(await api.discardGitHubPublish("p"), unreachable);
  const native = read("src-tauri/src/lib.rs");
  const build = read("src-tauri/build.rs");
  const allowed = read("src-tauri/capabilities/main-window.json");
  const routes = read("src-tauri/e2e_bridge/routes.rs");
  for (const command of ["status", "approve", "exclude", "recheck", "continue", "discard"]) {
    assert.match(native, new RegExp(`async fn desktop_github_publish_${command}\\(\\n    webview: Webview,`), command);
    assert.match(native, new RegExp(`\\n    desktop_github_publish_${command},\\n`), command);
    assert.ok(build.includes(`"desktop_github_publish_${command}",`), command);
    assert.ok(allowed.includes(`"allow-desktop-github-publish-${command}",`), command);
    assert.ok(routes.includes(`"desktop_github_publish_${command}" => {`), command);
  }
});

test("every code a publish can stop on passes the bridge and has a plain sentence", () => {
  const engine = ["github/job.rs", "github/publish.rs", "github/api.rs", "github/draft.rs", "github/prompt.rs"].map((path) => read(`../../crates/crazyagent/src/${path}`)).join("\n");
  const transport = read("../../crates/crazyagent/src/engine/owner/transport/github.rs");
  const named = [...engine.matchAll(/"(github_publish_[a-z_]+|github_repo_[a-z_]+)"/g)].map((match) => match[1]);
  const failed = [...`${engine}\n${transport}`.matchAll(/failure\("([a-z_]+)"\)/g)].map((match) => match[1])
    .filter((code) => !code.startsWith("owner_"));
  const codes = [...new Set([...named, ...failed])].filter((code) => !code.startsWith("github_publish_draft_"));
  assert.ok(codes.length >= 40, codes.join(","));
  for (const code of ["project_folder_missing", "github_publish_project_unknown", "github_publish_unavailable", "github_store_unavailable"]) {
    assert.ok(codes.includes(code), code);
  }
  const bridge = read("src-tauri/src/bridge/intake.rs");
  const passed = bridge.slice(bridge.indexOf("const GITHUB_CODES"), bridge.indexOf("];", bridge.indexOf("const GITHUB_CODES")));
  for (const code of codes) {
    assert.ok(passed.includes(`"${code}",`), `${code} reaches the page as github_failed`);
    const sentence = api.intakeCopy(code);
    assert.ok(sentence.endsWith(".") && !sentence.includes("_") && !sentence.startsWith("Something went wrong"), `${code}: ${sentence}`);
  }
});

test("the Overview shows where a publish stands, step by step", () => {
  assert.deepEqual(publishJob.publishView(null), { kind: "none" });
  assert.deepEqual(publishJob.publishView(job({ step: "drafting" })), { kind: "drafting" });
  assert.deepEqual(publishJob.publishView(job({ step: "lines", lines: [".env*", "node_modules/"], draft: { by: "agent" } })),
    { kind: "lines", lines: [".env*", "node_modules/"], note: publishJob.DRAFT_AGENT });
  assert.deepEqual(publishJob.publishView(job({ step: "lines", lines: [".env*"], draft: { by: "baseline", reason: "github_publish_draft_stopped" } })).note, publishJob.DRAFT_STOPPED);
  assert.deepEqual(publishJob.publishView(job({ step: "lines", lines: [".env*"], draft: { by: "baseline", reason: "instructions_cleanup_no_account" } })).note, publishJob.DRAFT_BASELINE);
  const blocked = publishJob.publishView(job({ step: "blocked", scan: { findings: [{ path: "keys.txt", line: 3, kind: "token" }, { path: "keys.txt", line: 9, kind: "private_key" }, { path: "a/.env", line: 1, kind: "named_value" }], truncated: true } }));
  assert.deepEqual(blocked, { kind: "blocked", findings: ["keys.txt, line 3: looks like an access token", "keys.txt, line 9: looks like a private key", "a/.env, line 1: looks like a secret value"], paths: ["keys.txt", "a/.env"], truncated: true });
  const committed = { head: "abc", branch: "main" };
  assert.deepEqual(publishJob.publishView(job({ step: "creating", committed, sent_at: 1 }, { stopped: "github_repo_name_taken" })),
    { kind: "stopped", reason: api.intakeCopy("github_repo_name_taken") });
  assert.deepEqual(publishJob.publishView(job({ step: "pushed", full_name: "me/tool", private: true, workflows: false })),
    { kind: "stopped", reason: publishJob.PART_WAY });
  const outcomes = [
    { rule: "squash_only", on: true, state: { state: "applied" } },
    { rule: "secret_scanning", on: true, state: { state: "refused", status: 403, message: "nope" } },
    { rule: "signed_commits", on: false, state: { state: "not_applicable", reason: "off" } },
    { rule: "codeql_default_setup", on: true, state: { state: "waiting", reason: "later" } },
  ];
  assert.deepEqual(publishJob.publishView(job({ step: "done", full_name: "me/tool", outcomes })),
    { kind: "done", line: "Published as me/tool. 1 of 3 rules set.", missed: ["Secret scanning", "CodeQL code scanning"] });
});

test("a discarded publish says what it left on GitHub, and Discard is offered only before the push", () => {
  assert.equal(publishJob.leftLine(null), "");
  assert.equal(publishJob.leftLine({ left: "made", full_name: "me/tool" }), "The empty repository me/tool stays on GitHub. Delete it there if you don't want it.");
  assert.equal(publishJob.leftLine({ left: "maybe", full_name: "me/tool" }), "GitHub may have made me/tool. Check GitHub and delete it if you don't want it.");
  assert.equal(publishJob.discardable(job({ step: "lines", lines: [], draft: { by: "agent" } })), true);
  assert.equal(publishJob.discardable(job({ step: "created", committed: { head: "a", branch: "main" }, full_name: "me/tool", private: true })), true);
  assert.equal(publishJob.discardable(job({ step: "pushed", full_name: "me/tool", private: true, workflows: false })), false);
  assert.equal(publishJob.discardable(job({ step: "done", full_name: "me/tool", outcomes: [] })), false);
  assert.equal(publishJob.discardable(null), false);
});

test("the Overview mounts the publish card, which drives every publish action", () => {
  const overview = read("src/screens/project/overview.tsx");
  assert.match(overview, /<GitHubPublishCard projectId=\{data\.id\}[ />]/);
  const card = read("src/screens/project/github-publish.tsx");
  for (const name of ["loadGitHubPublish", "approveGitHubPublish", "excludeFromGitHubPublish", "recheckGitHubPublish", "continueGitHubPublish", "discardGitHubPublish", "ConfirmDialog", "publishView", "leftLine", "discardable", "DRAFT_POLL_MS"]) {
    assert.ok(card.includes(name), name);
  }
  assert.equal(publishJob.DRAFT_POLL_MS, 2000);
});

test("a failed publish read keeps the last job shown with its problem and Try again, and a failed answer reads the job again", () => {
  const drafting = { kind: "ok", value: job({ step: "drafting" }) };
  const lines = { kind: "ok", value: job({ step: "lines", lines: [".env*"], draft: { by: "agent" } }) };
  const lost = { kind: "error", code: "github_outcome_unknown", message: "We couldn't confirm what GitHub did." };
  assert.deepEqual(publishJob.readKept(drafting, lost), { read: drafting, failed: lost.message });
  assert.deepEqual(publishJob.readKept(null, unreachable), { read: null, failed: unreachable.message });
  assert.deepEqual(publishJob.readKept(drafting, { kind: "not_ready" }), { read: drafting, failed: publishJob.ENGINE_STARTING });
  assert.deepEqual(publishJob.readKept(drafting, lines), { read: lines, failed: null });
  const card = read("src/screens/project/github-publish.tsx");
  assert.match(card, /readKept\(/);
  assert.match(card, /setAnswered\(\(count\) => count \+ 1\);/, "a failed read counts as an answer");
  assert.match(card, /\}, \[view\.kind, answered\]\);/, "the draft poll goes on after a failed read");
  assert.match(card, /if \(result\.kind !== "ok"\) \{\n\s+failed\(result\);\n\s+setTick\(\(value\) => value \+ 1\);/, "a failed answer reads the job again");
  assert.match(card, /id="pj-github-reread"/, "a failed read offers Try again");
});

test("a failed Start from the Overview form reads the publish again, so a Start that ran shows where it stands", () => {
  const card = read("src/screens/project/github-publish.tsx");
  assert.match(card, /if \(!made\) \{\n\s+setProblem\(line\);\n\s+setTick\(\(value\) => value \+ 1\);\n\s+if \(lost\) \{\n\s+const at = Date\.now\(\);\n\s+noteLostStart\(projectId, at\);\n\s+setLostSince\(at\);\n\s+\}\n\s+return;/, "a lost Start from the form is kept across a remount, as Create's is (GSB2-3)");
  assert.match(card, /\{form && view\.kind === "none" && <PublishForm /, "a job read after the Start closes the form");
});

test("a Start whose reply was lost keeps the Overview reading every two seconds until a publish shows or the Start's budget passes, then says so, the TUI's words", () => {
  assert.equal(typeof publishJob.lostStartOver, "function");
  assert.equal(publishJob.START_LOST_WAIT_MS, 140_000);
  assert.equal(publishJob.lostStartOver(1_000, 1_000 + 139_999), false);
  assert.equal(publishJob.lostStartOver(1_000, 1_000 + 140_000), true);
  assert.equal(publishJob.PUBLISH_NOT_SEEN, "No publish has shown up. Try Publish again.");
  assert.equal(publishJob.lostStartSince("project-lost"), null);
  publishJob.noteLostStart("project-lost", 5);
  assert.equal(publishJob.lostStartSince("project-lost"), 5);
  publishJob.forgetLostStart("project-lost");
  assert.equal(publishJob.lostStartSince("project-lost"), null);
  const card = read("src/screens/project/github-publish.tsx");
  assert.match(card, /useState<number \| null>\(\(\) => lostStartSince\(projectId\)\)/, "a Start Create lost is waited for here");
  assert.match(card, /onStarted\(result\.kind === "ok" \? result\.value : null, publishOutcome\(result\), startReplyLost\(result\)\)/);
  assert.match(card, /const step = lostStartStep\(lostSince, answered, read, readFailed !== null, Date\.now\(\)\);\n\s+if \(step === "idle"\) return;/);
  assert.match(card, /if \(step === "read"\) \{\n\s+const timer = setTimeout\(\(\) => setTick\(\(value\) => value \+ 1\), DRAFT_POLL_MS\);\n\s+return \(\) => clearTimeout\(timer\);\n\s+\}/, "each read schedules the next");
  assert.match(card, /forgetLostStart\(projectId\);\n\s+setLostSince\(null\);\n\s+if \(step === "over"\) setProblem\(PUBLISH_NOT_SEEN\);\n\s+\}, \[projectId, lostSince, answered, read, readFailed\]\);/);
  assert.match(card, /if \(!noJob \|\| lostSince !== null\) return;/, "no question while the Start may still land");
  const tui = read("../../crates/crazyagent/src/tui/screens/project_publish.rs");
  assert.match(tui, /"No publish has shown up\."/);
  assert.match(tui, /"Try Publish again\."/);
});

test("an Overview opened again after a lost Start waits for its own first read and says no publish showed only when a read finds none (GSB2-2)", () => {
  const none = { kind: "ok", value: null };
  const drafting = { kind: "ok", value: job({ step: "drafting" }) };
  const over = 1_000 + publishJob.START_LOST_WAIT_MS;
  assert.equal(publishJob.lostStartStep(null, 1, none, false, over), "idle", "no lost Start");
  assert.equal(publishJob.lostStartStep(1_000, 0, null, false, over), "idle", "nothing before this mount's first read");
  assert.equal(publishJob.lostStartStep(1_000, 1, drafting, false, over), "seen", "the publish shows");
  assert.equal(publishJob.lostStartStep(1_000, 1, none, false, over), "over", "the latest read found no publish after the budget");
  assert.equal(publishJob.lostStartStep(1_000, 1, none, false, over - 1), "read", "the Start may still land");
  assert.equal(publishJob.lostStartStep(1_000, 1, null, true, over), "read", "a failed first read says nothing about the publish");
  assert.equal(publishJob.lostStartStep(1_000, 2, none, true, over), "read", "a failed read keeps an older read, which says nothing about now (rv-ds241 L1)");
  assert.equal(publishJob.lostStartStep(1_000, 2, drafting, true, over), "seen", "a publish an older read showed has landed");
  assert.throws(() => publishJob.lostStartStep(1_000, -1, none, false, over), /below zero/);
  assert.throws(() => publishJob.lostStartStep(1_000, 0.5, none, false, over), /not a count/);
});

test("the Overview asks to publish a project with no publish through two bridge commands, and Publish opens the New Project form", async () => {
  assert.equal(publishJob.PROMPT_LINE, "This project is not on GitHub. Publish it?");
  assert.deepEqual([publishJob.PROMPT_PUBLISH, publishJob.PROMPT_NOT_NOW, publishJob.PROMPT_NEVER], ["Publish", "Not now", "Don't ask again"]);
  const none = { kind: "ok", value: null };
  const ask = { kind: "ok", value: true };
  assert.equal(publishJob.noticeShown(none, ask), true);
  assert.equal(publishJob.noticeShown(none, { kind: "ok", value: false }), false);
  assert.equal(publishJob.noticeShown({ kind: "ok", value: job({ step: "drafting" }) }, ask), false, "never over a publish");
  assert.equal(publishJob.noticeShown(unreachable, ask), false);
  assert.equal(publishJob.noticeShown(none, unreachable), false);
  assert.equal(publishJob.noticeShown(null, ask), false);
  assert.equal(publishJob.noticeShown(none, null), false);
  const client = read("src/engine-client.ts");
  assert.match(client, /export type GitHubPromptAnswer = "not_now" \| "never";/);
  assert.match(client, /invoke<boolean>\("desktop_github_publish_prompt", \{ project \}\)/);
  assert.match(client, /invoke<null>\("desktop_github_publish_prompt_set", \{ project, answer \}\)/);
  for (const name of ["loadGitHubPublishPrompt", "answerGitHubPublishPrompt"]) assert.equal(typeof api[name], "function", name);
  assert.deepEqual(await api.loadGitHubPublishPrompt("p"), unreachable);
  assert.deepEqual(await api.answerGitHubPublishPrompt("p", "not_now"), unreachable);
  const native = read("src-tauri/src/lib.rs");
  const build = read("src-tauri/build.rs");
  const allowed = read("src-tauri/capabilities/main-window.json");
  const routes = read("src-tauri/e2e_bridge/routes.rs");
  const bridge = read("src-tauri/src/bridge/intake.rs");
  for (const command of ["prompt", "prompt_set"]) {
    assert.match(native, new RegExp(`async fn desktop_github_publish_${command}\\(\\n    webview: Webview,`), command);
    assert.match(native, new RegExp(`\\n    desktop_github_publish_${command},\\n`), command);
    assert.ok(build.includes(`"desktop_github_publish_${command}",`), command);
    assert.ok(allowed.includes(`"allow-desktop-github-publish-${command.replace("_", "-")}",`), command);
    assert.ok(routes.includes(`"desktop_github_publish_${command}" => {`), command);
    assert.match(bridge, new RegExp(`pub fn github_publish_${command}\\(`), command);
  }
  const card = read("src/screens/project/github-publish.tsx");
  for (const name of ["loadGitHubPublishPrompt", "noticeShown", "PROMPT_LINE", "PublishFields", "useGitHubPublish", "createWait", "startPublish", "publishOutcome"]) {
    assert.ok(card.includes(name), name);
  }
  assert.match(card, /answerGitHubPublishPrompt\(projectId, "not_now"\)/);
  assert.match(card, /answerGitHubPublishPrompt\(projectId, "never"\)/);
  const overview = read("src/screens/project/overview.tsx");
  assert.match(overview, /<GitHubPublishCard projectId=\{data\.id\} name=\{data\.name\} about=\{setup\?\.about \?\? ""\} \/>/);
});

test("the Overview form says the TUI's sign-in words, offers Try again after a failed read and keeps New project's waits", async () => {
  const publish = await import("../src/screens/new-project/publish.ts");
  const tui = read("../../crates/crazyagent/src/tui/screens/project_publish.rs");
  const tuiLine = (name) => tui.match(new RegExp(`const ${name}: &str = "([^"]+)";`))?.[1];
  assert.equal(publish.SIGN_IN_CHECKING, tuiLine("CHECKING"));
  assert.equal(publish.OVERVIEW_SIGNED_OUT, tuiLine("SIGNED_OUT"));
  assert.equal(publish.SIGN_IN_UNREAD, tuiLine("STATUS_FAILED"));
  assert.equal(publish.SIGN_IN_WAIT, "Sign in to GitHub, or turn publishing off.", "New project keeps its own wait");
  assert.equal(publish.OVERVIEW_DETAILS_WAIT, "Fix the GitHub details above first.");
  assert.equal(publish.overviewWait(publish.CHECKING_WAIT), null, "the fields already say the sign-in is being read");
  assert.equal(publish.overviewWait(publish.SIGN_IN_WAIT), null, "the fields already say to sign in");
  assert.equal(publish.overviewWait(publish.DETAILS_WAIT), publish.OVERVIEW_DETAILS_WAIT);
  assert.equal(publish.overviewWait(null), null);
  assert.throws(() => publish.overviewWait("A wait createWait never gives."), /overviewWait got a wait/);
  const fields = read("src/screens/new-project/fields.tsx");
  assert.match(fields, /place === "overview" \? OVERVIEW_SIGNED_OUT : SIGN_IN_WAIT/);
  assert.match(fields, /\{SIGN_IN_CHECKING\}/);
  assert.match(fields, /\{SIGN_IN_UNREAD\}/);
  assert.match(fields, /onClick=\{\(\) => reloadStatus\(\)\}>Try again</);
  const card = read("src/screens/project/github-publish.tsx");
  assert.match(card, /<PublishFields [^>]*place="overview"/);
  assert.match(card, /const why = overviewWait\(wait\);/);
  const page = read("src/screens/new-project/index.tsx");
  assert.doesNotMatch(page, /place="overview"/, "New project keeps its words");
});
