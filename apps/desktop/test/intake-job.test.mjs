import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const intake = await import("../src/screens/intake/intake.ts");
const api = await import("../src/api-intake.ts");

const SOURCE = "/Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop";
const job = (state, extra = {}) => ({ id: "add-1", source: SOURCE, mode: "move", overrides: [], stopping: false, state, ...extra });
const downloading = (done, total) => ({ state: "downloading", progress: { done, total } });
const copying = (copied, checked = 0) => ({ state: "copying", copied, checked });
const failed = (code, phase) => ({ state: "failed", code, phase });

test("an intake job line says what it does now, with counts as files land", () => {
  assert.equal(intake.jobLine(job(downloading(0, 0))), "Downloading shop from iCloud...");
  assert.equal(intake.jobLine(job(downloading(12, 3000))), "Downloading shop from iCloud: 12 of 3000 files");
  assert.throws(() => intake.jobLine(job(downloading(4, 3))));
  assert.equal(intake.jobLine(job(copying(0))), "Moving shop...");
  assert.equal(intake.jobLine(job(copying(0), { mode: "copy" })), "Copying shop...");
  assert.equal(intake.jobLine(job(copying(18), { mode: "copy" })), "Copying shop: 18 files copied");
  assert.equal(intake.jobLine(job(copying(18, 5))), "Checking the copy of shop: 5 files checked");
  assert.equal(intake.jobLine(job({ state: "clearing" })), "Removing shop from its old place...");
  assert.equal(intake.jobLine(job(copying(18), { stopping: true })), "Stopping shop...");
  assert.equal(intake.jobLine(job({ state: "done" })), "shop is in your projects folder. Set it up from Home.");
  assert.equal(intake.jobLine(job({ state: "cancelled", phase: "copying" })), "Adding shop stopped. Nothing was moved or copied.");
  assert.equal(intake.jobLine(job(failed("home_copy_failed", "copying"))), "shop: Couldn't copy the folder. The original is untouched. Try again.");
  assert.equal(intake.jobLine(job(failed("home_move_source_left", "clearing"))),
    "shop: The folder was added, but part of the original couldn't be removed. Check its old place.");
  assert.equal(intake.jobLine(job({ state: "needs_answer", code: "home_move_worktrees" })), "shop needs your answer before it is moved.");
  assert.equal(intake.jobLine(job({ state: "needs_answer", code: "home_copy_too_large" }, { mode: "copy" })), "shop needs your answer before it is copied.");
});

test("an intake job offers stop while it runs, retry only when nothing landed, and an answer for a soft refusal", () => {
  assert.deepEqual(intake.jobActions(job(downloading(1, 3))), ["stop"]);
  assert.deepEqual(intake.jobActions(job(downloading(1, 3), { stopping: true })), []);
  assert.deepEqual(intake.jobActions(job(copying(4))), ["stop"]);
  assert.deepEqual(intake.jobActions(job({ state: "clearing" })), []);
  assert.deepEqual(intake.jobActions(job({ state: "done" })), ["dismiss"]);
  assert.deepEqual(intake.jobActions(job(failed("home_copy_failed", "copying"))), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(job(failed("home_icloud_not_downloaded", "downloading"))), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(job(failed("home_move_source_left", "clearing"))), ["dismiss"]);
  assert.deepEqual(intake.jobActions(job({ state: "cancelled", phase: "downloading" })), ["retry", "dismiss"]);
  assert.deepEqual(intake.jobActions(job({ state: "needs_answer", code: "home_move_worktrees" })), ["answer", "dismiss"]);
});

test("the app reads the job every second while it runs and at the found-folder pace otherwise", () => {
  assert.equal(intake.INTAKE_POLL_MS, 1000);
  assert.equal(intake.intakePollMs(null), intake.FOUND_POLL_MS);
  assert.equal(intake.intakePollMs(job(downloading(0, 0))), intake.INTAKE_POLL_MS);
  assert.equal(intake.intakePollMs(job({ state: "clearing" })), intake.INTAKE_POLL_MS);
  assert.equal(intake.intakePollMs(job({ state: "done" })), intake.FOUND_POLL_MS);
  assert.equal(intake.jobRunning(job(copying(1))), true);
  assert.equal(intake.jobRunning(job({ state: "cancelled", phase: "copying" })), false);
  assert.equal(intake.jobRunning(null), false);
});

test("a failed start goes Home only for a job adding this folder that runs, or ended done and is not the job read before the start", () => {
  const read = (value) => ({ kind: "ok", value });
  const older = job({ state: "done" });
  const newer = job({ state: "done" }, { id: "add-2" });
  const unread = { kind: "error", code: "owner_timeout", message: "The engine didn't answer in time." };
  assert.equal(intake.startReached(SOURCE, job(copying(1)), read(older)), true);
  assert.equal(intake.startReached(SOURCE, job(copying(1)), unread), true);
  assert.equal(intake.startReached(SOURCE, newer, read(older)), true);
  assert.equal(intake.startReached(SOURCE, newer, read(null)), true);
  assert.equal(intake.startReached(SOURCE, older, read(older)), false, "the add that ended before this start");
  assert.equal(intake.startReached(SOURCE, newer, unread), false, "no read before: an ended job may be older");
  assert.equal(intake.startReached(SOURCE, job(failed("home_copy_failed", "copying"), { id: "add-2" }), read(null)), false);
  assert.equal(intake.startReached("/Users/mira/Desktop/other", job(copying(1)), read(null)), false);
  assert.equal(intake.startReached(SOURCE, null, read(null)), false);
});

test("a job's end is said once, when the read after a running one shows that job ended", () => {
  const running = job(copying(3));
  const done = job({ state: "done" });
  assert.equal(intake.jobEndLine(running, done), "shop is in your projects folder. Set it up from Home.");
  assert.equal(intake.jobEndLine(done, done), null);
  assert.equal(intake.jobEndLine(null, done), null);
  assert.equal(intake.jobEndLine(running, running), null);
  assert.equal(intake.jobEndLine(running, null), null);
  assert.equal(intake.jobEndLine(running, { ...done, id: "add-2" }), null);
});

const folderRead = (value = {}) => ({ kind: "ok", value: { bytes: 10, worktrees: 0, worktree_paths: [], movable: true, copyable: true, git: true, same_volume: true, ...value } });

test("answering a soft refusal on Home asks about that issue and keeps the job's own overrides", () => {
  assert.deepEqual(intake.jobQuestion(job({ state: "needs_answer", code: "home_copy_too_large" }, { mode: "copy", overrides: ["linked_checkout"] }), folderRead()),
    { issues: ["too_many"], overrides: ["linked_checkout", "too_large"] });
  assert.deepEqual(intake.jobQuestion(job({ state: "needs_answer", code: "home_move_worktrees" }, { overrides: ["too_large"] }), folderRead({ movable: false, git_link: "worktrees" })),
    { issues: ["worktrees"], overrides: ["worktrees", "too_large"] });
  assert.deepEqual(intake.jobQuestion(job({ state: "needs_answer", code: "home_copy_linked_checkout" }, { mode: "copy", overrides: ["linked_checkout"] }), folderRead()),
    { issues: ["linked_checkout"], overrides: ["linked_checkout"] });
  assert.throws(() => intake.jobQuestion(job({ state: "done" }), folderRead()));
  assert.throws(() => intake.jobQuestion(job({ state: "needs_answer", code: "intake_failed" }), folderRead()));
  assert.throws(() => intake.jobQuestion(job({ state: "needs_answer", code: "home_move_worktrees" }), { kind: "loading" }));
});

test("Home's answer to a refused move names the git link the folder read found, with its data-loss warning", () => {
  const { ADD_WARNINGS } = api;
  const refused = job({ state: "needs_answer", code: "home_move_worktrees" });
  const read = folderRead({ movable: false, git_link: "linked_worktree", worktree_paths: ["/Users/mira/Projects/shop-main"] });
  const linked = intake.jobQuestion(refused, read);
  assert.deepEqual(linked, { issues: ["linked_worktree"], overrides: ["worktrees"] });
  const blocks = intake.anywayWarnings("move", linked.issues, read);
  assert.ok(blocks.some((block) => block.loss && block.lines.includes(ADD_WARNINGS.linked_worktree)));
  assert.ok(!blocks.some((block) => block.lines.includes(ADD_WARNINGS.worktrees)));
  assert.deepEqual(intake.jobQuestion(refused, folderRead({ movable: false, git_link: "submodule" })).issues, ["submodule"]);
  const worktrees = folderRead({ movable: false, git_link: "worktrees", worktree_paths: ["/Users/mira/Projects/shop-fix"] });
  const listed = intake.anywayWarnings("move", intake.jobQuestion(refused, worktrees).issues, worktrees);
  assert.ok(listed.some((block) => block.lines.includes(ADD_WARNINGS.worktrees) && block.paths.includes("/Users/mira/Projects/shop-fix")));
  const unread = { kind: "error", code: "folder_size_failed", message: "Something went wrong. Try again." };
  assert.deepEqual(intake.jobQuestion(refused, unread), { issues: ["git_data_unread", "git_unread"], overrides: ["worktrees"] });
  assert.deepEqual(intake.jobQuestion(refused, folderRead({ movable: undefined, copyable: undefined })).issues, ["git_data_unread", "git_unread"]);
});

const { createElement } = await import("react");
const { renderToStaticMarkup } = await import("react-dom/server");
const { IntakeJobView } = await import("../src/screens/intake/found.tsx");
const { Sidebar } = await import("../src/ui/shell.tsx");
const router = await import("../src/ui/router.ts");
const html = (type, props) => renderToStaticMarkup(createElement(type, props));
const noop = () => {};
const jobRow = (state, extra = {}) => html(IntakeJobView, { job: job(state, extra), busy: false, onStop: noop, onRetry: noop, onAnswer: noop, onDismiss: noop });
const buttons = (markup) => [...markup.matchAll(/<button[^>]*>(?:<[^>]+>)*([^<]*)/g)].map((match) => match[1]);

test("Home's job row shows the job's line and only the actions its state allows", () => {
  const running = jobRow(downloading(12, 3000));
  assert.match(running, /Downloading shop from iCloud: 12 of 3000 files/);
  assert.match(running, /role="status"/);
  assert.deepEqual(buttons(running), ["Stop"]);
  assert.deepEqual(buttons(jobRow({ state: "clearing" })), []);
  assert.deepEqual(buttons(jobRow({ state: "done" })), ["Dismiss"]);
  assert.deepEqual(buttons(jobRow(failed("home_copy_failed", "copying"))), ["Try again", "Dismiss"]);
  assert.deepEqual(buttons(jobRow(failed("home_move_source_left", "clearing"))), ["Dismiss"]);
  assert.deepEqual(buttons(jobRow({ state: "needs_answer", code: "home_move_worktrees" })), ["Answer", "Dismiss"]);
  assert.match(jobRow({ state: "cancelled", phase: "copying" }), /Adding shop stopped\. Nothing was moved or copied\./);
});

const found = (name, state, extra = {}) => ({ name, path: `/Users/mira/Projects/${name}`, git: true, known: false, state, ...extra });
const HOME = router.formatRoute({ name: "home" });

test("the sidebar lists folders not set up at once, and the folder being added, each pointing Home", () => {
  const folders = [found("zeta", "found"), found("alpha", "found"), found("beta", "onboarding", { project_id: "p2" }),
    found("gamma", "ready", { project_id: "p3" }), found("delta", "found", { project_id: "p1" }), found("shop", "found")];
  assert.deepEqual(intake.sidebarFolders({ kind: "ok", value: folders }, null, new Set(["p1"])).map((entry) => [entry.name, entry.note, entry.href]),
    [["alpha", "Not set up", HOME], ["shop", "Not set up", HOME], ["zeta", "Not set up", HOME]]);
  const adding = intake.sidebarFolders({ kind: "ok", value: folders }, job(copying(4)), new Set(["p1"]));
  assert.deepEqual(adding.map((entry) => [entry.id, entry.name, entry.note]),
    [["intake:add-1", "shop", "Adding..."], [`found:${folders[1].path}`, "alpha", "Not set up"], [`found:${folders[0].path}`, "zeta", "Not set up"]]);
  assert.deepEqual(intake.sidebarFolders({ kind: "loading" }, job({ state: "done" }), new Set()), []);
  const many = Array.from({ length: 40 }, (_, index) => found(`f${String(index).padStart(2, "0")}`, "found"));
  assert.equal(intake.sidebarFolders({ kind: "ok", value: many }, null, new Set()).length, intake.SIDEBAR_FOLDERS_MAX);
});

test("a sidebar folder row shows its note and opens its own page", () => {
  const markup = html(Sidebar, { route: { name: "home" }, profileName: "Mira", engine: "running", onRetry: noop,
    projects: [{ id: "found:/Users/mira/Projects/alpha", name: "alpha", note: "Not set up", href: HOME }] });
  assert.match(markup, new RegExp(`href="${HOME}"[^>]*>(?:(?!</a>).)*alpha(?:(?!</a>).)*<span class="nav-note">Not set up</span>`));
});
