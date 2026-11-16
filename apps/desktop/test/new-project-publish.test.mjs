import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const publish = await import("../src/screens/new-project/publish.ts");
const wizard = await import("../src/screens/new-project/wizard.ts");
const api = await import("../src/api-intake.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const owners = [{ login: "acme", personal: false }, { login: "me", personal: true }];
const block = (changes = {}) => ({ ...publish.emptyPublish(), ...changes });

test("a new draft publishes on GitHub, private, without the docs folder, with the standard line", () => {
  assert.deepEqual(publish.emptyPublish(), { on: true, private: true, owner: null, name: null, description: null, docs: false });
  assert.deepEqual(wizard.emptyDraft().publish, publish.emptyPublish());
  assert.equal(publish.PUBLISH_TOGGLE, "Publish this project on GitHub");
  assert.equal(publish.PUBLISH_LINE, "Standard secure setup; agents can propose changes later.");
  assert.equal(publish.PUBLISH_DOCS, "Also publish the docs folder (plans, research)");
  assert.equal(publish.SIGN_IN_WAIT, "Sign in to GitHub, or turn publishing off.");
});

test("the repository name is the project name made safe for GitHub", () => {
  assert.equal(publish.githubName("My Project"), "My-Project");
  assert.equal(publish.githubName("  hello   world!! "), "hello-world");
  assert.equal(publish.githubName("..hidden"), "hidden");
  assert.equal(publish.githubName("-dash_ok.v2"), "dash_ok.v2");
  assert.equal(publish.githubName("repo.git"), "repo");
  assert.equal(publish.githubName("x.git.git"), "x");
  assert.equal(publish.githubName("café app"), "caf-app");
  assert.equal(publish.githubName("日本"), "");
  assert.equal(publish.githubName(""), "");
  assert.equal(publish.githubName("a".repeat(150)), "a".repeat(100));
  assert.equal(publish.githubName(`${"a".repeat(96)}.gitx`), "a".repeat(96));
});

test("the description is the about text on one line within GitHub's 350 characters", () => {
  assert.equal(publish.githubDescription("A  tool\n\nfor\tthings "), "A tool for things");
  assert.equal(publish.githubDescription("bell\u0007here"), "bell here");
  assert.equal(publish.githubDescription("no\u00a0break"), "no break");
  assert.equal(publish.githubDescription("byte\ufefforder"), "byte\ufefforder");
  assert.equal(publish.githubDescription("é".repeat(400)), "é".repeat(350));
  const faces = publish.githubDescription("😀".repeat(400));
  assert.equal([...faces].length, 350);
  assert.equal(faces, "😀".repeat(350));
  assert.equal(publish.githubDescription(`${"a".repeat(349)} b`), "a".repeat(349));
  assert.equal(publish.githubDescription(""), "");
});

test("the block shows the personal account and follows the Basics name and about until they are edited", () => {
  assert.deepEqual(publish.publishShown(block(), "My Project", "A tool", owners), { owner: "me", name: "My-Project", description: "A tool" });
  assert.deepEqual(publish.publishShown(block(), "Other", "Changed", owners), { owner: "me", name: "Other", description: "Changed" });
  assert.equal(publish.publishShown(block({ owner: "acme" }), "x", "", owners).owner, "acme");
  assert.equal(publish.publishShown(block({ owner: "gone" }), "x", "", owners).owner, "me");
  assert.equal(publish.publishShown(block(), "x", "", []).owner, "");
  assert.equal(publish.publishShown(block(), "x", "", [{ login: "org", personal: false }]).owner, "org");
  const edited = block({ name: "custom", description: "" });
  assert.deepEqual(publish.publishShown(edited, "Renamed", "New about", owners), { owner: "me", name: "custom", description: "" });
});

test("the block checks what the engine checks and waits for a GitHub sign-in", () => {
  assert.deepEqual(publish.publishErrors(block({ on: false }), "", "", [], false), {});
  assert.deepEqual(publish.publishErrors(block(), "My Project", "A tool", [], false), { sign_in: publish.SIGN_IN_WAIT });
  assert.deepEqual(publish.publishErrors(block(), "My Project", "A tool", owners, true), {});
  for (const name of ["", "bad name", ".x", "-x", "x.git", "a/b", "a".repeat(101)]) {
    const errors = publish.publishErrors(block({ name }), "My Project", "", owners, true);
    assert.equal(typeof errors.name, "string", JSON.stringify(name));
    assert.ok(errors.name.length > 0);
  }
  assert.deepEqual(publish.publishErrors(block({ name: "a".repeat(100) }), "x", "", owners, true), {});
  for (const list of [[], [{ login: "-bad", personal: true }], [{ login: "a".repeat(40), personal: true }], [{ login: "a_b", personal: true }]]) {
    const errors = publish.publishErrors(block(), "My Project", "", list, true);
    assert.equal(typeof errors.owner, "string", JSON.stringify(list));
  }
  assert.deepEqual(publish.publishErrors(block(), "My Project", "", [{ login: "a".repeat(39), personal: true }], true), {});
  for (const description of ["x".repeat(351), "a\u0007b", "line\nbreak", "a\tb"]) {
    const errors = publish.publishErrors(block({ description }), "My Project", "", owners, true);
    assert.equal(typeof errors.description, "string", JSON.stringify(description));
  }
  assert.deepEqual(publish.publishErrors(block({ description: "😀".repeat(350) }), "My Project", "", owners, true), {});
});

test("the plan carries the shown values, the visibility and the docs choice, and only for a valid block", () => {
  assert.deepEqual(publish.publishPlan(block(), "My Project", "A  tool", owners), {
    owner: "me", name: "My-Project", description: "A tool", private: true, docs: false,
  });
  assert.deepEqual(publish.publishPlan(block({ private: false, docs: true, owner: "acme", name: "site" }), "My Project", "", owners), {
    owner: "acme", name: "site", description: "", private: false, docs: true,
  });
  assert.equal(publish.publishPlan(block({ on: false }), "My Project", "", owners), null);
  assert.equal(publish.publishPlan(block({ name: "bad name" }), "My Project", "", owners), null);
  assert.equal(publish.publishPlan(block(), "My Project", "", []), null);
});

test("the owners and the publish start reach the engine through their own client calls", async () => {
  const client = read("src/engine-client.ts");
  assert.match(client, /export function githubOwners\(\): Promise<GitHubOwners> \{\s*return invoke<GitHubOwners>\("desktop_github_owners"\)/);
  assert.match(client, /export function githubPublishStart\(project: string, plan: GitHubPublishPlan\): Promise<GitHubPublishJob> \{\s*return invoke<GitHubPublishJob>\("desktop_github_publish_start", \{ project, plan \}\)/);
  assert.equal(typeof api.loadGitHubOwners, "function");
  assert.equal(typeof api.startGitHubPublish, "function");
  const unreachable = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };
  assert.deepEqual(await api.loadGitHubOwners(), unreachable);
  const plan = { owner: "me", name: "site", description: "", private: true, docs: false };
  assert.deepEqual(await api.startGitHubPublish("project-1", plan), unreachable);
});

test("Basics shows the publish block", () => {
  const page = read("src/screens/new-project/index.tsx");
  assert.ok(page.includes("<PublishFields"), "index.tsx renders no PublishFields");
  const fields = read("src/screens/new-project/fields.tsx");
  assert.ok(fields.includes("export function PublishFields"), "fields.tsx has no PublishFields");
  for (const name of ["PUBLISH_TOGGLE", "PUBLISH_LINE", "PUBLISH_DOCS", "SIGN_IN_WAIT"]) assert.ok(fields.includes(name), name);
});

test("Create project waits on the publish block until it is off or valid", () => {
  assert.equal(publish.DETAILS_WAIT, "Fix the GitHub details in Basics first.");
  assert.equal(publish.createWait(block({ on: false }), "", "", [], false), null);
  assert.equal(publish.createWait(block(), "My Project", "", owners, false), publish.SIGN_IN_WAIT);
  assert.equal(publish.createWait(block({ name: "bad name" }), "My Project", "", owners, true), publish.DETAILS_WAIT);
  assert.equal(publish.createWait(block(), "My Project", "", [], true), publish.DETAILS_WAIT);
  assert.equal(publish.createWait(block(), "My Project", "A tool", owners, true), null);
  assert.equal(publish.CHECKING_WAIT, "Checking your GitHub sign-in. Create waits for it, or turn publishing off.");
  assert.equal(publish.createWait(block(), "My Project", "A tool", [], false, true), publish.CHECKING_WAIT);
  assert.equal(publish.createWait(block(), "My Project", "A tool", owners, true, true), publish.CHECKING_WAIT);
  assert.equal(publish.createWait(block({ on: false }), "My Project", "A tool", [], false, true), null);
  assert.equal(publish.createWait(block(), "My Project", "A tool", owners, true, false), null);
  const fields = read("src/screens/new-project/fields.tsx");
  assert.match(fields, /const checking = status\.kind === "loading" \|\| \(signedIn && owners\.kind === "loading"\);/);
  const page = read("src/screens/new-project/index.tsx");
  assert.match(page, /const publishWait = createWait\([^;]*github\.signedIn, github\.checking\);/);
  assert.match(page, /const canCreate = [^\n]*!publishWait/);
});

test("a made project starts its publish with the block's plan, and nothing starts while publishing is off", async () => {
  assert.equal(publish.PUBLISH_STARTED, "Publishing on GitHub has started.");
  assert.equal(publish.startPublish("project-1", block({ on: false }), "My Project", "", owners), null);
  assert.equal(publish.startPublish("project-1", block({ name: "bad name" }), "My Project", "", owners), null);
  const started = publish.startPublish("project-1", block(), "My Project", "", owners);
  assert.ok(started, "a valid block starts the publish");
  const unreachable = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };
  assert.deepEqual(await started, unreachable);
  const page = read("src/screens/new-project/index.tsx");
  assert.match(page, /startPublish\(id, /);
});

test("the made project's message follows the publish Start's reply", () => {
  assert.equal(typeof publish.publishOutcome, "function");
  assert.equal(publish.publishOutcome(null), "");
  assert.equal(publish.publishOutcome({ kind: "ok", value: { step: { step: "drafting" } } }), "Publishing on GitHub has started.");
  assert.equal(publish.publishOutcome({ kind: "error", code: "github_not_signed_in", message: "Sign in with GitHub first." }), "Publishing on GitHub didn't start. Sign in with GitHub first.");
  assert.equal(publish.publishOutcome({ kind: "not_ready" }), "Publishing on GitHub didn't start. CrazyAgent's engine is still starting.");
  const page = read("src/screens/new-project/index.tsx");
  assert.match(page, /const outcome = publishOutcome\(started\);/);
  assert.doesNotMatch(page, /PUBLISH_STARTED/);
  const client = read("src/engine-client.ts");
  assert.match(client, /\| \{ step: "drafting" \}\n/);
});

test("a Start whose reply was lost after the engine ran it says it may have started and where to look, the TUI's words", () => {
  const maybe = "Publishing on GitHub may have started. The project's Overview shows where it stands.";
  assert.equal(publish.publishOutcome({ kind: "error", code: "github_outcome_unknown", message: "We couldn't confirm what GitHub did. Check the list again." }), maybe);
  assert.equal(publish.publishOutcome({ kind: "error", code: "github_reply_bounds", message: "GitHub's reply was too large to show. Try again." }), maybe);
  assert.equal(publish.publishOutcome({ kind: "error", code: "github_publish_running", message: "A publish is already running." }), "Publishing on GitHub didn't start. A publish is already running.");
  const tui = read("../../crates/crazyagent/src/tui/screens/new_project_publish.rs");
  assert.match(tui, /"Publishing on GitHub may have started\."/);
  assert.match(tui, /"The project's Overview shows where it stands\."/);
});

test("Create marks a Start whose reply was lost, so the project's Overview reads it until it shows", () => {
  assert.equal(typeof publish.startReplyLost, "function");
  assert.equal(publish.startReplyLost(null), false);
  assert.equal(publish.startReplyLost({ kind: "error", code: "github_outcome_unknown", message: "" }), true);
  assert.equal(publish.startReplyLost({ kind: "error", code: "github_reply_bounds", message: "" }), true);
  assert.equal(publish.startReplyLost({ kind: "error", code: "github_not_signed_in", message: "" }), false);
  assert.equal(publish.startReplyLost({ kind: "not_ready" }), false);
  assert.equal(publish.startReplyLost({ kind: "ok", value: { step: { step: "drafting" } } }), false);
  const page = read("src/screens/new-project/index.tsx");
  assert.match(page, /const started = publishing \? await publishing : null;\n\s+const outcome = publishOutcome\(started\);\n\s+if \(startReplyLost\(started\)\) noteLostStart\(id, Date\.now\(\)\);/);
});
