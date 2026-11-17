import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const intake = await import("../src/screens/intake/intake.ts");
const api = await import("../src/api-intake.ts");

const SOURCE = "/Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop";
const job = (state, extra = {}) => ({ id: "add-1", kind: "add", source: SOURCE, mode: "move", overrides: [], stopping: false, state, ...extra });
const downloading = (done, total) => ({ state: "downloading", progress: { done, total } });
const copying = (copied, checked = 0) => ({ state: "copying", copied, checked });
const failed = (code, phase) => ({ state: "failed", code, phase });
const WAITING = { state: "waiting", behind: "garden" };

test("V243-ICLOUD-MOVE-ZERO: the download line says macOS downloads, with files and bytes landed", () => {
  const landing = (done, total, bytes_done, bytes_total) => ({ state: "downloading", progress: { done, total, bytes_done, bytes_total } });
  assert.equal(intake.jobLine(job(landing(556, 127940, 31 * 1024 * 1024, 2 * 1024 ** 3))), "macOS is downloading shop: 556 of 127940 files (31 MB of 2 GB)");
  assert.equal(intake.jobLine(job(landing(1, 2, 0, 0))), "macOS is downloading shop: 1 of 2 files");
  assert.equal(intake.jobLine(job(landing(1, 2, 9, 4))), "macOS is downloading shop: 1 of 2 files", "bytes past their total are left out");
});

test("V243-ICLOUD-MOVE-ZERO: a done add says which rebuildable folders it left in iCloud and where", () => {
  const left = [{ path: "node_modules", files: 57108 }, { path: ".venv", files: null }];
  assert.equal(intake.jobLine(job({ state: "done" }, { left })),
    `shop is in your projects folder. Set it up from Home. node_modules and .venv stayed in ${SOURCE}; reinstall them in the project.`);
  assert.equal(intake.jobLine(job({ state: "done" }, { left: left.slice(0, 1), mode: "copy" })),
    "shop is in your projects folder. Set it up from Home. node_modules was not copied; reinstall it in the project.");
  const many = ["a", "b", "c", "d", "e"].map((name) => ({ path: `${name}/node_modules`, files: 1 }));
  assert.match(intake.jobLine(job({ state: "done" }, { left: many, mode: "copy" })), / a\/node_modules, b\/node_modules, c\/node_modules and 2 more were not copied;/);
  assert.equal(intake.jobLine(job({ state: "done" }, { left: [] })), "shop is in your projects folder. Set it up from Home.");
});

test("an intake job line says what it does now, with counts as files land", () => {
  assert.equal(intake.jobLine(job(downloading(0, 0))), "Downloading shop from iCloud...");
  assert.equal(intake.jobLine(job(downloading(12, 3000))), "macOS is downloading shop: 12 of 3000 files");
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
  assert.equal(intake.jobLine(job(failed("home_copy_names_fold", "copying"))), "shop: Two names in the folder differ only in case or accents, and your projects folder can't keep both. Rename one of them, then try again. The original is untouched.");
  assert.equal(intake.jobLine(job(failed("home_copy_entry_swapped", "copying"))), "shop: Something replaced a file or folder in it while it was being copied, so the copy stopped. The original is untouched. Try again once nothing else is changing the folder.");
  assert.equal(intake.jobLine(job(failed("home_move_source_left", "clearing"))),
    "shop: The folder was added, but part of the original couldn't be removed. What's left there may be newer than the copy, so check it before you delete anything.");
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

test("the app reads the jobs every second while one runs or waits and every 30 s otherwise", () => {
  assert.equal(intake.INTAKE_POLL_MS, 1000);
  assert.equal(intake.intakePollMs([]), 30_000);
  assert.equal(intake.intakePollMs([job(downloading(0, 0))]), intake.INTAKE_POLL_MS);
  assert.equal(intake.intakePollMs([job({ state: "clearing" })]), intake.INTAKE_POLL_MS);
  assert.equal(intake.intakePollMs([job({ state: "done" })]), 30_000);
  assert.equal(intake.intakePollMs([job({ state: "done" }), job(WAITING, { id: "add-2" })]), intake.INTAKE_POLL_MS);
  assert.equal(intake.jobRunning(job(WAITING)), true);
  assert.equal(intake.jobRunning(job(copying(1))), true);
  assert.equal(intake.jobRunning(job({ state: "cancelled", phase: "copying" })), false);
  assert.equal(intake.jobRunning(null), false);
});

test("the app reads the found folders every 3 s while Home shows them and every 30 s on other pages", () => {
  assert.equal(intake.foundPollMs(true), 3_000);
  assert.equal(intake.foundPollMs(false), 30_000);
});

test("a failed start goes Home only for a job adding this folder that runs or waits, or ended done and was not read before the start", () => {
  const read = (value) => ({ kind: "ok", value });
  const older = job({ state: "done" });
  const newer = job({ state: "done" }, { id: "add-2" });
  const garden = job(copying(1), { id: "add-3", source: "/Users/mira/Desktop/garden" });
  const unread = { kind: "error", code: "owner_timeout", message: "The engine didn't answer in time." };
  assert.equal(intake.startReached(SOURCE, [job(copying(1))], read([older])), true);
  assert.equal(intake.startReached(SOURCE, [job(copying(1))], unread), true);
  assert.equal(intake.startReached(SOURCE, [garden, job(WAITING, { id: "add-2" })], read([garden])), true, "waiting its turn");
  assert.equal(intake.startReached(SOURCE, [newer], read([older])), true);
  assert.equal(intake.startReached(SOURCE, [older, newer], read([older])), true);
  assert.equal(intake.startReached(SOURCE, [newer], read([])), true);
  assert.equal(intake.startReached(SOURCE, [older], read([older])), false, "the add that ended before this start");
  assert.equal(intake.startReached(SOURCE, [newer], unread), false, "no read before: an ended job may be older");
  assert.equal(intake.startReached(SOURCE, [job(failed("home_copy_failed", "copying"), { id: "add-2" })], read([])), false);
  assert.equal(intake.startReached("/Users/mira/Desktop/other", [job(copying(1))], read([])), false);
  assert.equal(intake.startReached(SOURCE, [garden], read([])), false, "only another folder's job runs");
  assert.equal(intake.startReached(SOURCE, [], read([])), false);
});

test("each job's end is said once, when the read after shows it ended, while the jobs beside it still run (DEC-197)", () => {
  const running = job(copying(3));
  const done = job({ state: "done" });
  const garden = job(copying(1), { id: "add-2", source: "/Users/mira/Desktop/garden" });
  const ended = (before, after) => intake.jobEnds(before, after).map((one) => intake.jobLine(one));
  const shopDone = "shop is in your projects folder. Set it up from Home.";
  assert.deepEqual(ended([running], [done]), [shopDone]);
  assert.deepEqual(ended([running, garden], [done, garden]), [shopDone]);
  assert.deepEqual(ended([running, garden], [done, { ...garden, state: { state: "done" } }]),
    [shopDone, "garden is in your projects folder. Set it up from Home."]);
  assert.deepEqual(ended([done], [done]), []);
  assert.deepEqual(ended([], [done]), []);
  assert.deepEqual(ended([running], [running]), []);
  assert.deepEqual(ended([running], []), []);
  assert.deepEqual(ended([running], [{ ...done, id: "add-9" }]), []);
  assert.deepEqual(ended([job(WAITING)], [running]), [], "a waiting job let go is still at work");
  assert.deepEqual(ended([job(WAITING)], [job({ state: "cancelled", phase: "waiting" })]), ["Adding shop stopped. Nothing was moved or copied."]);
});

test("a job waiting its turn names the job it waits for and offers Stop (DEC-197)", () => {
  assert.equal(intake.jobLine(job(WAITING)), "Waiting for garden to finish...");
  assert.equal(intake.jobLine(job(WAITING, { mode: "copy" })), "Waiting for garden to finish...");
  assert.deepEqual(intake.jobActions(job(WAITING)), ["stop"]);
  assert.deepEqual(intake.jobActions(job(WAITING, { stopping: true })), []);
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
  assert.match(running, /macOS is downloading shop: 12 of 3000 files/);
  assert.match(running, /role="status"/);
  assert.deepEqual(buttons(running), ["Stop"]);
  assert.deepEqual(buttons(jobRow({ state: "clearing" })), []);
  assert.deepEqual(buttons(jobRow({ state: "done" })), ["Dismiss"]);
  assert.deepEqual(buttons(jobRow(failed("home_copy_failed", "copying"))), ["Try again", "Dismiss"]);
  assert.deepEqual(buttons(jobRow(failed("home_move_source_left", "clearing"))), ["Dismiss"]);
  assert.deepEqual(buttons(jobRow({ state: "needs_answer", code: "home_move_worktrees" })), ["Answer", "Dismiss"]);
  assert.match(jobRow({ state: "cancelled", phase: "copying" }), /Adding shop stopped\. Nothing was moved or copied\./);
  const waiting = jobRow(WAITING);
  assert.match(waiting, /role="status">Waiting for garden to finish\.\.\.</);
  assert.match(waiting, />Waiting</);
  assert.deepEqual(buttons(waiting), ["Stop"]);
});

test("Home shows a row for every job the engine holds, in start order, and nothing without one (DEC-197)", async () => {
  const { SessionContext } = await import("../src/state.ts");
  const { IntakeJobPanel } = await import("../src/screens/intake/found.tsx");
  assert.equal(typeof IntakeJobPanel, "function");
  const panel = (jobs) => renderToStaticMarkup(createElement(SessionContext.Provider, { value: { intake: jobs, showToast: noop, refreshIntake: noop } },
    createElement(IntakeJobPanel, { onSetUp: noop })));
  const garden = job({ state: "waiting", behind: "shop" }, { id: "add-2", source: "/Users/mira/Desktop/shop-copy" });
  const both = panel([job(copying(2)), garden, job({ state: "done" }, { id: "add-3", source: "/Users/mira/Desktop/notes" })]);
  assert.deepEqual([...both.matchAll(/role="status">([^<]*)</g)].map((match) => match[1]),
    ["Moving shop: 2 files copied", "Waiting for shop to finish...", "notes is in your projects folder. Set it up from Home."]);
  assert.deepEqual(buttons(both), ["Stop", "Stop", "Dismiss"]);
  assert.equal(panel([]), "");
});

const found = (name, state, extra = {}) => ({ name, path: `/Users/mira/Projects/${name}`, git: true, known: false, state, ...extra });
const HOME = router.formatRoute({ name: "home" });

test("the sidebar lists folders not set up at once, and the folder being added, each pointing Home", () => {
  const folders = [found("zeta", "found"), found("alpha", "found"), found("beta", "onboarding", { project_id: "p2" }),
    found("gamma", "ready", { project_id: "p3" }), found("delta", "found", { project_id: "p1" }), found("shop", "found")];
  assert.deepEqual(intake.sidebarFolders({ kind: "ok", value: folders }, [], new Set(["p1"])).map((entry) => [entry.name, entry.note, entry.href]),
    [["alpha", "Not set up", HOME], ["shop", "Not set up", HOME], ["zeta", "Not set up", HOME]]);
  const adding = intake.sidebarFolders({ kind: "ok", value: folders }, [job(copying(4))], new Set(["p1"]));
  assert.deepEqual(adding.map((entry) => [entry.id, entry.name, entry.note]),
    [["intake:add-1", "shop", "Adding..."], [`found:${folders[1].path}`, "alpha", "Not set up"], [`found:${folders[0].path}`, "zeta", "Not set up"]]);
  const two = intake.sidebarFolders({ kind: "ok", value: folders }, [job(copying(4)), job(WAITING, { id: "add-2", source: "/Users/mira/Desktop/alpha" })], new Set(["p1"]));
  assert.deepEqual(two.map((entry) => [entry.id, entry.name, entry.note]),
    [["intake:add-1", "shop", "Adding..."], ["intake:add-2", "alpha", "Adding..."], [`found:${folders[0].path}`, "zeta", "Not set up"]]);
  assert.deepEqual(intake.sidebarFolders({ kind: "loading" }, [job({ state: "done" })], new Set()), []);
  const many = Array.from({ length: 40 }, (_, index) => found(`f${String(index).padStart(2, "0")}`, "found"));
  assert.equal(intake.sidebarFolders({ kind: "ok", value: many }, [], new Set()).length, intake.SIDEBAR_FOLDERS_MAX);
});

test("a sidebar folder row shows its note and opens its own page", () => {
  const markup = html(Sidebar, { route: { name: "home" }, profileName: "Mira", engine: "running", onRetry: noop,
    projects: [{ id: "found:/Users/mira/Projects/alpha", name: "alpha", note: "Not set up", href: HOME }] });
  assert.match(markup, new RegExp(`href="${HOME}"[^>]*>(?:(?!</a>).)*alpha(?:(?!</a>).)*<span class="nav-note">Not set up</span>`));
});

const MOVED = "/Users/mira/Code/reading-list";
const moveJob = (state, extra = {}) => job(state, { id: "move-1", kind: "move_project", project_id: "reading", source: MOVED, ...extra });

test("a project move job says it waits for the agents, moves, and ends in the move's words (DEC-177)", () => {
  assert.equal(intake.jobLine(moveJob({ state: "stopping", agents_left: 2 })), "Waiting for 2 agents of reading-list to stop...");
  assert.equal(intake.jobLine(moveJob({ state: "stopping", agents_left: 1 })), "Waiting for 1 agent of reading-list to stop...");
  assert.equal(intake.jobLine(moveJob(copying(18))), "Moving reading-list: 18 files copied");
  assert.equal(intake.jobLine(moveJob({ state: "done" })), "reading-list is now in your projects folder. Start the team again from Team.");
  assert.equal(intake.jobLine(moveJob({ state: "cancelled", phase: "stopping" })), "Moving reading-list stopped. Nothing was moved or copied.");
  assert.equal(intake.jobLine(moveJob(failed("move_project_busy", "stopping"))),
    "reading-list: The team is still stopping, or research or a change is still running. Move again in a few seconds.");
  assert.equal(intake.jobLine(moveJob(failed("home_move_project", "copying"))),
    "reading-list: Another project uses this folder too, so it can't be moved or copied.");
  assert.equal(intake.jobLine(moveJob(failed("move_project_interrupted", "copying"))),
    "reading-list: The move stopped part way. Check the project folder before trying again.");
  const left = intake.jobLine(moveJob(failed("move_project_old_folder_left", "clearing")));
  assert.match(left, /^reading-list is now in your projects folder\. Some items in its old folder couldn't be deleted and may be newer than the copy: check .*Code\/reading-list before you delete anything\.$/);
  assert.doesNotMatch(left, /yourself|delete it|remove it/, "what a move leaves may be the only copy of newer edits (RV-DS241-102)");
  assert.deepEqual(intake.jobActions(moveJob({ state: "stopping", agents_left: 1 })), ["stop"]);
  assert.deepEqual(intake.jobActions(moveJob({ state: "stopping", agents_left: 1 }, { stopping: true })), []);
  assert.deepEqual(intake.jobActions(moveJob(failed("move_project_old_folder_left", "clearing"))), ["dismiss"]);
  assert.equal(intake.jobRunning(moveJob({ state: "stopping", agents_left: 1 })), true);
  assert.equal(intake.intakePollMs([moveJob({ state: "stopping", agents_left: 1 })]), intake.INTAKE_POLL_MS);
  assert.equal(intake.jobLine(moveJob(WAITING)), "Waiting for garden to finish...");
});

test("Home's row names a project move's state in the move's words and offers Finishing, not Stop, while it clears", () => {
  const row = (state, extra = {}) => html(IntakeJobView, { job: moveJob(state, extra), busy: false, onStop: noop, onRetry: noop, onAnswer: noop, onDismiss: noop });
  assert.match(row({ state: "stopping", agents_left: 2 }), /Waiting/);
  assert.match(row(copying(3)), /Moving/);
  assert.match(row({ state: "done" }), /Moved/);
  assert.match(row({ state: "clearing" }), /Finishing/);
  assert.deepEqual(buttons(row({ state: "clearing" })), []);
  assert.deepEqual(buttons(row({ state: "stopping", agents_left: 1 })), ["Stop"]);
});

test("a retry or an answer starts the same kind of job again: an add by its folder, a move by its project", async () => {
  const calls = [];
  const steps = {
    add: (path, mode, overrides) => { calls.push(["add", path, mode, overrides]); return Promise.resolve({ kind: "ok", value: job({ state: "copying", copied: 0, checked: 0 }) }); },
    move: (project, mode, overrides) => { calls.push(["move", project, mode, overrides]); return Promise.resolve({ kind: "ok", value: moveJob(copying(0)) }); },
  };
  await intake.restartJob(job(failed("home_copy_failed", "copying")), ["too_large"], steps);
  await intake.restartJob(moveJob({ state: "needs_answer", code: "home_move_worktrees" }, { mode: "move" }), ["worktrees"], steps);
  assert.deepEqual(calls, [["add", SOURCE, "move", ["too_large"]], ["move", "reading", "move", ["worktrees"]]]);
  assert.deepEqual(intake.sidebarFolders({ kind: "ok", value: [] }, [moveJob(copying(4))], new Set()), [], "a project being moved is already in the sidebar as a project");
});

test("a start past the waiting list says several folders wait, and the same folder points Home, both by name from the bridge (DEC-197)", async () => {
  const { readFileSync } = await import("node:fs");
  const busy = "Several folders are already waiting. Wait for one to finish on Home.";
  const same = "This folder is already being moved or added. See it on Home.";
  for (const copy of [api.intakeCopy, api.moveCopy, api.newProjectCopy]) {
    assert.equal(copy("job_busy"), busy);
    assert.equal(copy("job_same_folder"), same);
  }
  assert.equal(intake.busyLine, undefined, "a busy start no longer names one running job");
  assert.equal(intake.refusalLine, undefined);
  const bridge = readFileSync(new URL("../src-tauri/src/bridge/intake.rs", import.meta.url), "utf8");
  const listed = bridge.slice(bridge.indexOf("pub const INTAKE_CODES"), bridge.indexOf("];", bridge.indexOf("pub const INTAKE_CODES")));
  for (const code of ["job_busy", "job_same_folder"]) assert.ok(listed.includes(`"${code}",`), `${code} reaches the window by name`);
});
