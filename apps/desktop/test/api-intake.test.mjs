import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const api = await import("../src/api-intake.ts");

const failed = (code, message = "Something went wrong. Try again.") => ({ kind: "error", code, message });

test("every DEC-078 intake, GitHub and setup name has a client function", () => {
  for (const name of [
    "loadProjectsHome", "setProjectsHome", "moveProjectsHome", "loadFoundProjects", "loadFolderSize", "addExistingFolder", "startOnboarding", "loadOnboarding",
    "gitHubSignIn", "gitHubToken", "gitHubUseCli", "gitHubSignOut", "loadGitHubStatus", "loadGitHubRepos", "cloneRepo",
    "loadSetupStatus", "runSetup", "setPackEntry", "removePackEntry",
  ]) assert.equal(typeof api[name], "function", name);
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  for (const [name, command] of [
    ["projectsHome", "desktop_projects_home"], ["setProjectsHome", "desktop_set_projects_home"], ["moveProjectsHome", "desktop_move_projects_home"],
    ["foundProjects", "desktop_found_projects"], ["folderSize", "desktop_folder_size"], ["addExistingFolder", "desktop_add_existing_folder"],
    ["startOnboarding", "desktop_start_onboarding"], ["onboarding", "desktop_onboarding"],
    ["githubSignIn", "desktop_github_sign_in"], ["githubToken", "desktop_github_token"], ["githubUseCli", "desktop_github_use_cli"],
    ["githubSignOut", "desktop_github_sign_out"], ["githubStatus", "desktop_github_status"], ["githubRepos", "desktop_github_repos"],
    ["cloneRepo", "desktop_clone_repo"], ["setupStatus", "desktop_setup_status"], ["runSetup", "desktop_run_setup"],
    ["setPackEntry", "desktop_set_pack_entry"], ["removePackEntry", "desktop_remove_pack_entry"],
  ]) assert.match(client, new RegExp(`export function ${name}\\([^)]*\\)[^{]*\\{\\s*return invoke<[A-Za-z\\[\\]]+>\\("${command}"`), name);
  assert.match(client, /invoke<CloneReceipt>\("desktop_clone_repo", \{ fullName \}\)/);
  assert.match(client, /invoke<SetupStatus>\("desktop_run_setup", \{ pack, generation \}\)/);
  assert.match(client, /invoke<SetupStatus>\("desktop_set_pack_entry", \{ id, on, generation \}\)/);
});

test("without the native bridge an intake read reports the engine as unreachable, never sample data", async () => {
  assert.deepEqual(await api.loadFoundProjects(), failed("engine_unreachable", "CrazyAgent's engine isn't running."));
  assert.deepEqual(await api.loadProjectsHome(), failed("engine_unreachable", "CrazyAgent's engine isn't running."));
});

test("a GitHub token is checked before it leaves the window and is never echoed back", async () => {
  assert.deepEqual(await api.gitHubToken("   "), failed("github_token_blank", "Paste a GitHub token."));
  assert.deepEqual(await api.gitHubToken("ghp_abc def"), failed("github_token_invalid", "GitHub didn't accept this token. Check it and try again."));
  assert.deepEqual(await api.gitHubToken("x".repeat(256)), failed("github_token_invalid", "GitHub didn't accept this token. Check it and try again."));
  const sent = await api.gitHubToken("github_pat_" + "a".repeat(40));
  assert.equal(sent.kind, "error");
  assert.equal(sent.code, "engine_unreachable");
  assert.doesNotMatch(JSON.stringify(sent), /github_pat_/);
});

test("repository pages start at 1 and a bad page is a programming error", async () => {
  await assert.rejects(() => api.loadGitHubRepos(0), /page/);
  await assert.rejects(() => api.loadGitHubRepos(1.5), /page/);
  assert.equal((await api.loadGitHubRepos(1)).code, "engine_unreachable");
});

test("intake failures read as plain sentences", () => {
  assert.equal(api.intakeCopy("projects_home_unset"), "Choose your projects folder first.");
  assert.equal(api.intakeCopy("home_destination_exists"), "Your projects folder already has a folder with this name. Rename one of them first.");
  assert.equal(api.intakeCopy("clone_destination_exists"), "Your projects folder already has a folder with this repository's name.");
  assert.equal(api.intakeCopy("github_sign_in_expired"), "The GitHub code expired before it was entered. Start again.");
  assert.equal(api.intakeCopy("home_move_partial"), "Some projects were moved and couldn't be moved back. Check both folders.");
  assert.equal(api.intakeCopy("home_move_worktrees"), "This folder has git worktrees linked to it, so moving it would break them. Use it where it is or copy it.");
  assert.equal(api.intakeCopy("home_copy_linked_checkout"), "This folder is a git worktree or submodule of another repository, so a copy would still share it. Use it where it is, or copy the main repository folder.");
  assert.equal(api.intakeCopy("home_move_project"), "This folder holds or sits inside a project you already added. Use it where it is or copy it.");
  assert.equal(api.intakeCopy("pending_intent"), "Another change isn't confirmed yet. Use Check again at the top of the window, then try again.");
  assert.equal(api.intakeCopy("made_up"), "Something went wrong. Try again. (made_up)");
  assert.equal(api.intakeCopy({ code: "made_up", cause: "engine_detail" }), "Something went wrong. Try again. (engine_detail)");
  assert.equal(api.intakeCopy({ code: "setup_busy", cause: "engine_detail" }), "Setup is already running. Wait for it to finish. (engine_detail)");
});

test("setup takes at most 64 pack entries and its failures read as plain sentences", async () => {
  assert.throws(() => api.runSetup(Array.from({ length: 65 }, (_, index) => `entry-${index}`)), /64/);
  assert.equal((await api.runSetup(["superpowers"])).code, "engine_unreachable");
  assert.equal(api.intakeCopy("pack_required"), "Something went wrong. Try again. (pack_required)");
  assert.equal(api.intakeCopy("setup_busy"), "Setup is already running. Wait for it to finish.");
  assert.equal(api.intakeCopy("owner_outcome_unknown_reconcile_same_identity"), "CrazyAgent didn't answer in time. Try again.");
});

test("every refusal code the native bridge can return has a plain sentence", () => {
  const bridge = readFileSync(new URL("../src-tauri/src/bridge/intake.rs", import.meta.url), "utf8");
  const codes = new Set();
  for (const name of ["INTAKE_CODES", "GITHUB_CODES", "SETUP_CODES"]) {
    const start = bridge.indexOf(`const ${name}`);
    assert.ok(start >= 0, name);
    for (const match of bridge.slice(start, bridge.indexOf("];", start)).matchAll(/"([a-z_]+)"/g)) codes.add(match[1]);
  }
  for (const match of bridge.matchAll(/failure\("([a-z_]+)"\)/g)) codes.add(match[1]);
  assert.ok(codes.size > 100);
  for (const code of codes) assert.doesNotMatch(api.intakeCopy(code), / \([a-z0-9_]+\)$/, code);
});

test("an intake code named like an object built-in gets the plain fallback sentence", () => {
  for (const code of ["constructor", "__proto__", "toString"]) {
    assert.equal(api.intakeCopy(code), `Something went wrong. Try again. (${/^[a-z0-9_]+$/.test(code) ? code : "unknown"})`, code);
  }
});
