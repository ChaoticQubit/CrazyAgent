import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const api = await import("../src/api-intake.ts");

const failed = (code, message = "Something went wrong. Try again.") => ({ kind: "error", code, message });

test("every DEC-078 intake, GitHub and setup name has a client function", () => {
  for (const name of [
    "loadProjectsHome", "setProjectsHome", "loadFoundProjects", "loadFolderSize", "addExistingFolder", "startOnboarding", "loadOnboarding",
    "gitHubSignIn", "gitHubToken", "gitHubUseCli", "loadGitHubStatus", "loadGitHubRepos", "cloneRepo", "loadSetupStatus", "runSetup",
  ]) assert.equal(typeof api[name], "function", name);
  const source = readFileSync(new URL("../src/api-intake.ts", import.meta.url), "utf8");
  for (const command of [
    "desktop_projects_home", "desktop_set_projects_home", "desktop_found_projects", "desktop_folder_size", "desktop_add_existing_folder",
    "desktop_start_onboarding", "desktop_onboarding", "desktop_setup_status", "desktop_run_setup",
  ]) assert.match(source, new RegExp(`"${command}"`), command);
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  for (const [name, command] of [
    ["githubSignIn", "desktop_github_sign_in"], ["githubToken", "desktop_github_token"], ["githubUseCli", "desktop_github_use_cli"],
    ["githubStatus", "desktop_github_status"], ["githubRepos", "desktop_github_repos"], ["cloneRepo", "desktop_clone_repo"],
  ]) assert.match(client, new RegExp(`export function ${name}\\([^)]*\\)[^{]*\\{\\s*return invoke<[A-Za-z]+>\\("${command}"`), name);
  assert.match(client, /invoke<CloneReceipt>\("desktop_clone_repo", \{ fullName \}\)/);
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
  assert.equal(api.intakeCopy("folder_name_taken"), "Your projects folder already has a folder with this name. Rename one of them first.");
  assert.equal(api.intakeCopy("repo_already_cloned"), "This repository is already in your projects folder.");
  assert.equal(api.intakeCopy("pending_intent"), "Another change is still being saved. Wait for it to finish, then try again.");
  assert.equal(api.intakeCopy("made_up"), "Something went wrong. Try again.");
});
