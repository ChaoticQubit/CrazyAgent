import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { click, shown } from "./settings-steps.mjs";
import { run, tab, until } from "./trial-kit.mjs";

const root = process.env.MS_ROOT;
const GIT = "/usr/bin/git";
const POOL = "local-pool";
const SETUP_MS = 120000;
const UI_MS = 30000;
let generation = 0;

/** Calls one desktop command through the app's own bridge, as the window does. Inputs: the
 * command and its arguments. Output: its answer; throws with the failure the app returned. */
export async function invoke(command, args) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: JSON.stringify(failure) }));
  }, command, args);
  if (answer.failed !== undefined) throw new Error(`${command} failed: ${answer.failed}`);
  return answer.value;
}

/** Makes an owner repository with one commit, run with no system or user git config. Input:
 * its name. Output: its absolute path. */
function ownerRepo(name) {
  const repo = join(root, "repos", name);
  mkdirSync(repo, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), `# ${name}\n`);
  const env = { PATH: "/usr/bin:/bin", HOME: repo, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" };
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "Start"]]) {
    const result = spawnSync(GIT, args, { cwd: repo, encoding: "utf8", env });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
  return repo;
}

/** Waits for the window to reach the test engine, saves the owner profile and the projects
 * home and reloads, as a first start does. Output: none. */
export async function prepareWindow() {
  await until(() => browser.execute(() => Boolean(window.__TAURI_INTERNALS__)), 120000, "the app window never loaded");
  await until(async () => (await invoke("desktop_connect").catch(() => ({}))).connected === true, 60000, "the app never connected to the test engine");
  assert.equal((await invoke("desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: ++generation })).outcome, "applied");
  mkdirSync(join(root, "projects"), { mode: 0o700 });
  const homed = await invoke("desktop_set_projects_home", { path: join(root, "projects"), generation: ++generation });
  assert.equal(homed.outcome, "applied", `the projects home is set: ${JSON.stringify(homed)}`);
  await browser.execute(() => location.reload());
  await until(() => browser.execute(() => document.readyState === "complete" && Boolean(window.__TAURI_INTERNALS__)), 60000, "the app window never loaded again");
}

/** Adds an owner repository as a project on the fixture's pool, waits for onboarding and
 * opens it. Input: the repository name. Output: the project id, also set as the kit's
 * project. */
export async function newProject(name) {
  const receipt = await invoke("desktop_start_onboarding", { path: ownerRepo(name), pool: POOL, generation: ++generation });
  assert.equal(receipt.outcome, "applied", `${name} becomes a project: ${JSON.stringify(receipt)}`);
  const id = receipt.project_id;
  for (const deadline = Date.now() + SETUP_MS; ; await pause(1000)) {
    const view = await invoke("desktop_onboarding", { id });
    if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
    assert.ok(Date.now() < deadline, `onboarding of ${name} still running: ${JSON.stringify(view)}`);
  }
  await invoke("desktop_open_project", { id });
  const pools = await invoke("desktop_model_access_page", { collection: "pools", next: false });
  const assigned = await invoke("desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project: id, pool: POOL }, generation: ++generation });
  assert.equal(assigned.outcome, "applied", `${name} gets the pool: ${JSON.stringify(assigned)}`);
  run.project = id;
  return id;
}

/** Opens the project's Master settings drawer as the owner does and waits for a control in
 * it. Input: the control's selector. Output: none. */
export async function openMasterSettings(control) {
  await tab("conversation");
  await click("button=Master settings");
  await until(() => shown(control), UI_MS, `the Master settings drawer never showed ${control}`);
}
