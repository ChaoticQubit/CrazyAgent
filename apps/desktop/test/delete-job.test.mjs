import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const intake = await import("../src/screens/intake/intake.ts");
const api = await import("../src/api-intake.ts");
const projects = await import("../src/api-projects.ts");
const settings = await import("../src/screens/project/settings.tsx");
const { IntakeJobView } = await import("../src/screens/intake/found.tsx");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const noop = () => {};
const html = (type, props) => renderToStaticMarkup(createElement(type, props));
const buttons = (markup) => [...markup.matchAll(/<button[^>]*>(?:<[^>]+>)*([^<]*)/g)].map((match) => match[1]);
const NAME = "Notes/2026 plans";
const deleteJob = (state, extra = {}) => ({ id: "delete-1", kind: "delete", project_id: "notes", source: NAME, mode: "move", overrides: [], stopping: false, state, ...extra });
const moveJob = (state) => ({ id: "move-1", kind: "move_project", project_id: "reading", source: "/Users/mira/Code/reading-list", mode: "move", overrides: [], stopping: false, state });
const trashing = (done, total) => ({ state: "trashing", done, total });
const cutShort = (trashed, total) => ({ state: "cut_short", trashed, total });
const failed = (code, phase) => ({ state: "failed", code, phase });
const row = (state, extra = {}) => html(IntakeJobView, { job: deleteJob(state, extra), busy: false, onStop: noop, onRetry: noop, onAnswer: noop, onDismiss: noop });

test("a delete job names the project whole and says it waits, moves folders to the Trash and ends in the delete's words (DEC-177 add.1)", () => {
  assert.equal(intake.jobLine(deleteJob({ state: "stopping", agents_left: 2 })), `Waiting for 2 agents of ${NAME} to stop...`);
  assert.equal(intake.jobLine(deleteJob({ state: "copying", copied: 0, checked: 0 })), `Deleting ${NAME}...`);
  assert.equal(intake.jobLine(deleteJob(trashing(1, 2))), `Moving ${NAME} to the Trash: 1 of 2 folders`);
  assert.equal(intake.jobLine(deleteJob(trashing(0, 1))), `Moving ${NAME} to the Trash: 0 of 1 folder`);
  assert.equal(intake.jobLine(deleteJob({ state: "clearing" })), `Removing ${NAME} from CrazyAgent...`);
  assert.equal(intake.jobLine(deleteJob({ state: "done" })), `${NAME} is deleted. Its files are in the Trash.`);
  assert.equal(intake.jobLine(deleteJob({ state: "cancelled", phase: "stopping" })), `Deleting ${NAME} stopped. Nothing was moved to the Trash.`);
  assert.equal(intake.jobLine(deleteJob(failed("delete_agents_running", "stopping"))),
    `${NAME}: Its agents didn't stop in time, so nothing was deleted. They stay stopped; try again.`);
  assert.equal(intake.jobLine(deleteJob(failed("home_trash_failed", "trashing"))), `${NAME}: The folder couldn't be moved to the Trash. Nothing was deleted.`);
  assert.equal(intake.jobLine(deleteJob(cutShort(1, 2))), `Deleting ${NAME} was cut short with 1 of 2 folders in the Trash. Finish delete or keep the project.`);
  assert.equal(intake.jobLine(deleteJob(trashing(1, 2), { stopping: true })), `Stopping ${NAME}...`);
  assert.throws(() => intake.jobLine(deleteJob(trashing(3, 2))));
  assert.throws(() => intake.jobLine(deleteJob(cutShort(3, 2))));
});

test("a delete offers Stop only until its first Trash move, and Finish delete or Keep the project once cut short", () => {
  assert.deepEqual(intake.jobActions(deleteJob({ state: "stopping", agents_left: 1 })), ["stop"]);
  assert.deepEqual(intake.jobActions(deleteJob({ state: "copying", copied: 0, checked: 0 })), ["stop"]);
  assert.deepEqual(intake.jobActions(deleteJob(trashing(1, 2))), []);
  assert.deepEqual(intake.jobActions(deleteJob({ state: "clearing" })), []);
  assert.deepEqual(intake.jobActions(deleteJob({ state: "done" })), ["dismiss"]);
  assert.deepEqual(intake.jobActions(deleteJob({ state: "cancelled", phase: "stopping" })), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(deleteJob(failed("delete_trash_partial", "trashing"))), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(deleteJob(cutShort(1, 2))), ["retry", "dismiss"]);
  assert.equal(intake.jobRunning(deleteJob(trashing(0, 2))), true);
  assert.equal(intake.jobRunning(deleteJob(cutShort(1, 2))), false);
  assert.equal(intake.intakePollMs(deleteJob(trashing(0, 2))), intake.INTAKE_POLL_MS);
  assert.equal(intake.busyLine(deleteJob(trashing(1, 2))), `Deleting ${NAME} is running. Wait for it to finish on Home.`);
  assert.deepEqual(intake.sidebarFolders({ kind: "ok", value: [] }, deleteJob(trashing(0, 2)), new Set()), [], "a project being deleted is still a project in the sidebar");
});

test("Home's row names a delete's state, reads Can't stop now while folders go to the Trash, and offers Finish delete or Keep the project once cut short", () => {
  assert.match(row({ state: "stopping", agents_left: 1 }), /Waiting/);
  assert.match(row(trashing(1, 2)), /Deleting/);
  assert.match(row({ state: "done" }), /Deleted/);
  assert.match(row(cutShort(1, 2)), /Cut short/);
  assert.match(row(trashing(1, 2)), new RegExp(`<strong>${NAME}</strong>`), "the project's whole name, not its last path part");
  assert.deepEqual(buttons(row({ state: "stopping", agents_left: 1 })), ["Stop"]);
  assert.deepEqual(buttons(row(trashing(1, 2))), ["Can&#x27;t stop now"]);
  assert.match(row(trashing(1, 2)), /<button[^>]*disabled=""[^>]*>(?:<[^>]+>)*Can&#x27;t stop now/);
  assert.deepEqual(buttons(row({ state: "clearing" })), []);
  assert.deepEqual(buttons(row(cutShort(1, 2))), ["Finish delete", "Keep the project"]);
  assert.deepEqual(buttons(row({ state: "cancelled", phase: "stopping" })), ["Try again", "Dismiss"]);
  assert.deepEqual(buttons(row({ state: "done" })), ["Dismiss"]);
});

test("Finish delete and Try again open the project's remove ask on Delete everything; nothing deletes in one click (QUESTION-13:55 Q1 A)", () => {
  assert.equal(intake.retryRoute(deleteJob(cutShort(1, 2))), "/projects/notes/settings?section=remove&ask=delete");
  assert.equal(intake.retryRoute(deleteJob({ state: "cancelled", phase: "stopping" })), "/projects/notes/settings?section=remove&ask=delete");
  assert.equal(intake.retryRoute(deleteJob(cutShort(1, 2), { project_id: "a b" })), "/projects/a%20b/settings?section=remove&ask=delete");
  assert.equal(intake.retryRoute(moveJob({ state: "cancelled", phase: "stopping" })), null);
  const started = [];
  const steps = { add: () => { started.push("add"); return Promise.resolve({ kind: "ok", value: null }); }, move: () => { started.push("move"); return Promise.resolve({ kind: "ok", value: null }); } };
  assert.throws(() => intake.restartJob(deleteJob(cutShort(1, 2)), [], steps), /delete/);
  assert.deepEqual(started, []);
  const found = read("src/screens/intake/found.tsx");
  assert.match(found, /retryRoute\(job\)/, "Home's retry of a delete navigates to the remove ask");
});

test("Delete everything in the Remove dialog starts the engine's job and says to follow it on Home; a refusal stays in the dialog's note", async () => {
  const sent = [];
  const job = deleteJob({ state: "stopping", agents_left: 1 });
  const start = (id, name) => { sent.push([id, name]); return Promise.resolve({ kind: "ok", value: job }); };
  assert.deepEqual(await settings.deleteStart("notes", NAME, start), { kind: "started", job });
  assert.deepEqual(sent, [["notes", NAME]]);
  const busy = () => Promise.resolve({ kind: "error", code: "job_busy", message: "raw" });
  const held = () => Promise.resolve({ kind: "ok", value: moveJob({ state: "copying", copied: 2, checked: 0 }) });
  assert.deepEqual(await settings.deleteStart("notes", NAME, busy, held), { kind: "refused", note: "Moving reading-list is running. Wait for it to finish on Home." });
  const mismatch = () => Promise.resolve({ kind: "error", code: "delete_name_mismatch", message: "raw" });
  assert.deepEqual(await settings.deleteStart("notes", "notes", mismatch, held), { kind: "refused", note: "Type the project's name exactly as shown to delete it." });
  const unready = () => Promise.resolve({ kind: "not_ready" });
  assert.deepEqual(await settings.deleteStart("notes", NAME, unready, held), { kind: "refused", note: "Not ready yet: deleting a project isn't in this build." });
  const source = read("src/screens/project/settings.tsx");
  assert.match(source, /Follow it on Home; you can keep working\./);
  assert.match(source, /session\?\.refreshIntake\(started\.job\)/, "the started job shows on Home at once");
  assert.doesNotMatch(source, /removeProject\(data\.id, nextGeneration\(\), deleting \? typed : null\)/, "Delete everything is no longer a direct change of the window");
});

test("the delete command is wired from the Remove dialog to the engine's job, and a delete that ended reads the projects again", () => {
  assert.match(read("src/api-projects.ts"), /call<IntakeJob>\("desktop_delete_project_job", \{ id, name \}\)/);
  assert.match(read("src/engine-client.ts"), /invoke<IntakeJob>\("desktop_delete_project_job", \{ id, name \}\)/);
  const lib = read("src-tauri/src/lib.rs");
  const at = lib.indexOf("fn desktop_delete_project_job(");
  assert.ok(at >= 0);
  const signature = lib.slice(at, lib.indexOf(")", at));
  for (const param of ["id: String", "name: String"]) assert.ok(signature.includes(param), param);
  assert.ok(!signature.includes("generation"), "a job start carries no window generation");
  assert.equal((lib.match(/desktop_delete_project_job/g) ?? []).length, 2, "the command's body and its handler entry");
  assert.match(read("src-tauri/src/bridge/intake.rs"), /JobSubject::Delete \{ project_id, name \}/);
  assert.match(read("src/state.ts"), /kind === "delete"[^;]*refreshProjects\(\)/, "a delete that ended reads the projects again");
  assert.match(read("src/engine-client.ts"), /export type JobKind = "add" \| "move_project" \| "delete" \| "setup";/);
});

test("Delete everything has one way in, the engine's job: Remove project sends no name and the bridge's remove takes none, so nothing deletes outside the job (rv-code2 N3)", () => {
  const projectsApi = read("src/api-projects.ts");
  assert.match(projectsApi, /export async function removeProject\(id: string, generation: number\): Promise<Result<IntentReceipt>> \{/);
  assert.match(projectsApi, /call<IntentReceipt>\("desktop_remove_project", \{ id, generation \}\)/);
  assert.doesNotMatch(projectsApi, /deleteName/);
  const lib = read("src-tauri/src/lib.rs");
  const at = lib.indexOf("async fn desktop_remove_project(");
  assert.ok(at >= 0);
  assert.doesNotMatch(lib.slice(at, lib.indexOf("\n}\n", at)), /delete_name/, "the command takes no typed name");
  const bridge = read("src-tauri/src/bridge/project_setup.rs");
  const from = bridge.indexOf("pub async fn remove_project(");
  assert.ok(from >= 0);
  const remove = bridge.slice(from, bridge.indexOf("\n    }\n", from));
  assert.doesNotMatch(remove, /delete_name|DeleteProject/, "the bridge's remove sends no delete");
  assert.match(remove, /OwnerMutation::RemoveProject \{ project_id \}/);
  const routes = read("src-tauri/e2e_bridge/routes.rs");
  const route = routes.indexOf("\"desktop_remove_project\" => {");
  assert.ok(route >= 0);
  assert.doesNotMatch(routes.slice(route, routes.indexOf("\"desktop_", route + 1)), /delete_name/, "the e2e route matches the command");
  for (const path of ["test/native/terminal-wall.e2e.mjs", "test/native/ide-window.e2e.mjs"]) assert.doesNotMatch(read(path), /deleteName/, path);
});

test("a stop refused once the delete moves folders to the Trash, and each code a delete ends with, reads as a plain sentence", () => {
  assert.equal(api.intakeCopy("delete_cannot_stop"), "It's moving its folders to the Trash, so it can't stop now. Wait for it to finish on Home.");
  for (const code of ["delete_agents_running", "delete_trash_partial", "home_trash_failed", "project_delete_interrupted", "delete_in_progress", "delete_not_recorded",
    "delete_unavailable", "delete_shared_folder", "delete_research_running", "delete_platform_unqualified", "delete_outside_home", "delete_name_mismatch", "delete_cannot_stop"]) {
    const line = projects.failureCopy(code);
    assert.match(line, /^[A-Z][^_]*\.$/, code);
    assert.ok(!line.includes(code), code);
  }
});
