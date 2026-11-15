import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const intake = await import("../src/screens/intake/intake.ts");
const api = await import("../src/api-intake.ts");
const { IntakeJobView } = await import("../src/screens/intake/found.tsx");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const noop = () => {};
const buttons = (markup) => [...markup.matchAll(/<button[^>]*>(?:<[^>]+>)*([^<]*)/g)].map((match) => match[1]);
const NAME = "Notes/2026 plans";
const setupJob = (state, extra = {}) => ({ id: "setup-1", kind: "setup", project_id: "notes", source: NAME, mode: "move", overrides: [], stopping: false, state, ...extra });
const building = (done, total) => ({ state: "building", done, total });
const failed = (code) => ({ state: "failed", code, phase: "building" });
const cancelled = { state: "cancelled", phase: "building" };
const row = (state) => renderToStaticMarkup(createElement(IntakeJobView, { job: setupJob(state), busy: false, onStop: noop, onRetry: noop, onAnswer: noop, onDismiss: noop }));
const DONE_LINE = `${NAME} is set up. Choose its pool and agent access in its Settings.`;

test("a New Project job names the project whole, counts its documents as they are copied and ends in New Project's words (DEC-177 add.2)", () => {
  assert.equal(intake.jobName(setupJob(building(0, 0))), NAME);
  assert.equal(intake.jobLine(setupJob(building(0, 0))), `Setting up ${NAME}...`);
  assert.equal(intake.jobLine(setupJob(building(1, 3))), `Setting up ${NAME}: 1 of 3 documents copied`);
  assert.equal(intake.jobLine(setupJob(building(1, 1))), `Setting up ${NAME}: 1 of 1 document copied`);
  assert.equal(intake.jobLine(setupJob({ state: "done" })), DONE_LINE);
  assert.equal(intake.jobLine(setupJob(cancelled)), `Setting up ${NAME} stopped. Nothing was kept.`);
  assert.equal(intake.jobLine(setupJob(failed("document_too_large"))), `${NAME}: Files can be up to 50 MB.`);
  assert.equal(intake.jobLine(setupJob(failed("home_new_project_interrupted"))),
    `${NAME}: Setting up the project stopped part way. Nothing was kept; set it up again from New Project.`);
  assert.equal(intake.jobLine(setupJob(building(1, 3), { stopping: true })), `Stopping ${NAME}...`);
  assert.throws(() => intake.jobLine(setupJob(building(4, 3))));
});

test("a New Project offers Stop while it builds, Try again on New Project once it fails or stops, and never starts again in one click", () => {
  assert.deepEqual(intake.jobActions(setupJob(building(0, 2))), ["stop"]);
  assert.deepEqual(intake.jobActions(setupJob(building(0, 2), { stopping: true })), []);
  assert.deepEqual(intake.jobActions(setupJob({ state: "done" })), ["dismiss"]);
  assert.deepEqual(intake.jobActions(setupJob(failed("project_folder_unwritable"))), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(setupJob(cancelled)), ["retry", "dismiss"]);
  assert.equal(intake.jobRunning(setupJob(building(0, 2))), true);
  assert.equal(intake.intakePollMs(setupJob(building(0, 2))), intake.INTAKE_POLL_MS);
  assert.equal(intake.busyLine(setupJob(building(1, 2))), `Setting up ${NAME} is running. Wait for it to finish on Home.`);
  assert.equal(intake.retryRoute(setupJob(failed("project_folder_unwritable"))), "/projects/new");
  assert.equal(intake.retryRoute(setupJob(cancelled)), "/projects/new");
  const steps = { add: () => assert.fail("a New Project never starts as an add"), move: () => assert.fail("a New Project never starts as a move") };
  assert.throws(() => intake.restartJob(setupJob(cancelled), [], steps), /New Project/);
  assert.equal(intake.jobEndLine(setupJob(building(1, 1)), setupJob({ state: "done" })), DONE_LINE);
  assert.deepEqual(intake.sidebarFolders({ kind: "ok", value: [] }, setupJob(building(0, 1)), new Set()), [], "a New Project shows in the sidebar once it is a project");
});

test("Home's row draws a New Project as Setting up with Stop, then Set up with Dismiss", () => {
  let markup = "";
  assert.doesNotThrow(() => { markup = row(building(1, 3)); }, "Home draws a New Project job");
  assert.match(markup, /Setting up<\/span>/);
  assert.ok(markup.includes(`Setting up ${NAME}: 1 of 3 documents copied`));
  assert.deepEqual(buttons(markup), ["Stop"]);
  assert.doesNotThrow(() => { markup = row({ state: "done" }); });
  assert.match(markup, />Set up<\/span>/);
  assert.deepEqual(buttons(markup), ["Dismiss"]);
  assert.doesNotThrow(() => { markup = row(failed("home_new_project_failed")); });
  assert.deepEqual(buttons(markup), ["Try again", "Dismiss"]);
});

const SETUP_WORDS = {
  setup_cannot_stop: "The project is being saved, so it can't stop now. Wait for it to finish on Home.",
  home_new_project_interrupted: "Setting up the project stopped part way. Nothing was kept; set it up again from New Project.",
  home_new_project_failed: "The project folder couldn't be created. Nothing was kept.",
  home_add_cancelled: "Setting up the project stopped. Nothing was kept.",
  home_folder_name_invalid: "Give the project a name with letters or digits, and no folder by that name.",
  home_destination_exists: "Your projects folder already has a folder with this name. Choose another project name.",
  projects_home_unset: "Choose your projects folder first.",
  project_exists: "This project already exists.",
  pool_missing: "That pool no longer exists. Choose another pool.",
  team_harness_unavailable: "A harness in this team can't run team agents yet. Pick another harness.",
  documents_too_many: "Attach at most 16 documents.",
  document_name_duplicate: "Two documents have the same name. Rename one first.",
  document_name_invalid: "One of the documents has a name that can't be used. Rename it first.",
  document_too_large: "Files can be up to 50 MB.",
  document_type_not_allowed: "This file type isn't supported.",
  project_documents_too_large: "This project's documents would be over 200 MB. Remove some files first.",
  project_folder_unwritable: "CrazyAgent can't write the project folder. Check the disk and try again.",
  project_folder_unavailable: "Documents can't be copied into a project on this computer yet. Create it without documents.",
};

test("every code a New Project job is refused or ends with has New Project's own sentence and reaches the window by name", () => {
  const bridge = read("src-tauri/src/bridge/intake.rs");
  const listed = bridge.slice(bridge.indexOf("pub const INTAKE_CODES"), bridge.indexOf("];", bridge.indexOf("pub const INTAKE_CODES")));
  for (const [code, sentence] of Object.entries(SETUP_WORDS)) {
    assert.equal(api.newProjectCopy(code), sentence, code);
    assert.ok(listed.includes(`"${code}",`), `${code} reaches the window by name, never as intake_failed`);
  }
  assert.equal(api.newProjectCopy({ code: "job_busy" }), "Another job is running. Wait for it to finish on Home.");
});

test("the wizard reads the job it started from Home's read: running, done with its project, ended with its words, or lost", () => {
  assert.deepEqual(intake.setupFollow(null, "setup-1"), { kind: "lost" });
  assert.deepEqual(intake.setupFollow({ ...setupJob({ state: "done" }), id: "setup-0" }, "setup-1"), { kind: "lost" });
  const running = setupJob(building(1, 2));
  assert.deepEqual(intake.setupFollow(running, "setup-1"), { kind: "running", job: running });
  assert.deepEqual(intake.setupFollow(setupJob({ state: "done" }), "setup-1"), { kind: "done", project: "notes" });
  assert.deepEqual(intake.setupFollow(setupJob(failed("document_too_large")), "setup-1"), { kind: "ended", words: "Files can be up to 50 MB." });
  assert.deepEqual(intake.setupFollow(setupJob(cancelled), "setup-1"), { kind: "ended", words: "Setting up the project stopped. Nothing was kept." });
  assert.throws(() => intake.setupFollow(setupJob({ state: "done" }, { project_id: null }), "setup-1"));
});

test("Create starts the engine's job; a busy refusal names the running job and any other keeps New Project's words, the answers kept", async () => {
  const flow = await import("../src/screens/new-project/index.tsx");
  const spec = { name: "Notes", repositories: [] };
  const setup = { pool: null, research: [] };
  const job = setupJob(building(0, 0));
  const sent = [];
  const start = (...args) => { sent.push(args); return Promise.resolve({ kind: "ok", value: job }); };
  assert.deepEqual(await flow.setupStart(spec, setup, start), { kind: "started", job });
  assert.deepEqual(sent, [[spec, setup]]);
  const moving = { id: "move-1", kind: "move_project", project_id: "reading", source: "/Users/mira/Code/reading-list", mode: "move", overrides: [], stopping: false, state: { state: "copying", copied: 2, checked: 0 } };
  const held = () => Promise.resolve({ kind: "ok", value: moving });
  const refused = (code) => () => Promise.resolve({ kind: "error", code, message: "raw" });
  const KEPT = "Couldn't create the project. Your answers are kept.";
  assert.deepEqual(await flow.setupStart(spec, setup, refused("job_busy"), held),
    { kind: "refused", note: `${KEPT} Moving reading-list is running. Wait for it to finish on Home.` });
  assert.deepEqual(await flow.setupStart(spec, setup, refused("home_destination_exists"), held),
    { kind: "refused", note: `${KEPT} Your projects folder already has a folder with this name. Choose another project name.` });
  assert.deepEqual(await flow.setupStart(spec, setup, () => Promise.resolve({ kind: "not_ready" }), held),
    { kind: "refused", note: "Not ready yet: creating a project with research isn't in this build." });
});

test("New Project starts the engine's job, follows it on the page and runs its follow-ups only while the page is open (DEC-177 add.2 Q1 A)", () => {
  assert.match(read("src/api-projects.ts"), /call<IntakeJob>\("desktop_setup_project_job", \{ spec, setup \}\)/);
  assert.match(read("src/engine-client.ts"), /invoke<IntakeJob>\("desktop_setup_project_job", \{ spec, setup \}\)/);
  const lib = read("src-tauri/src/lib.rs");
  const at = lib.indexOf("fn desktop_setup_project_job(");
  assert.ok(at >= 0);
  const signature = lib.slice(at, lib.indexOf(")", at));
  for (const param of ["spec: ProjectSpec", "setup: ProjectSetup"]) assert.ok(signature.includes(param), param);
  assert.ok(!signature.includes("generation"), "a job start carries no window generation");
  assert.equal((lib.match(/desktop_setup_project_job/g) ?? []).length, 2, "the command's body and its handler entry");
  assert.match(read("src-tauri/src/bridge/intake.rs"), /JobSubject::Setup \{\s*project_id,\s*spec,\s*setup,?\s*\}/);
  const wizard = read("src/screens/new-project/index.tsx");
  assert.ok(wizard.includes("await setupStart(request.spec, request.setup)"), "Create starts the engine's job");
  assert.match(wizard, /start = startSetupJob,/, "the start is the engine's job");
  assert.ok(!wizard.includes("setupProject(request.spec"), "Create is no longer a direct change of the window");
  assert.ok(wizard.includes("session?.refreshIntake(result.job)"), "the started job shows on Home at once");
  assert.ok(wizard.includes("session?.followIntake("), "the page owns the end of the job it follows");
  assert.ok(wizard.includes("dismissIntake("), "a job the page ended leaves Home");
  assert.ok(wizard.includes("mounted.current"), "follow-ups stop once the owner leaves the page");
  const state = read("src/state.ts");
  assert.match(state, /kind === "setup"[^;]*refreshProjects\(\)/, "a New Project that ended reads the projects again");
  assert.match(state, /result\.value\.id !== followed\.current/, "Home's toast stays quiet for the job the page follows");
  const client = read("src/engine-client.ts");
  assert.match(client, /export type ProjectJobKind = "add" \| "move_project" \| "delete" \| "setup";/);
  assert.match(client, /\| \{ state: "building"; done: number; total: number \}/);
});
