import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const view = await import("../src/screens/project/move-home.tsx");
const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const ok = (value) => Promise.resolve({ kind: "ok", value });
const applied = { outcome: "applied", pending: false, rejection: null };
const idle = () => {};

test("Move into CrazyAgent folder shows only for a project outside the projects folder", () => {
  const outside = renderToStaticMarkup(createElement(view.MoveHomeRow, { folder: "/Users/o/Desktop/TessyAI", home: "/Users/o/CrazyAgent", onOpen: idle }));
  const inside = renderToStaticMarkup(createElement(view.MoveHomeRow, { folder: "/Users/o/CrazyAgent/TessyAI", home: "/Users/o/CrazyAgent", onOpen: idle }));
  const unset = renderToStaticMarkup(createElement(view.MoveHomeRow, { folder: "/Users/o/Desktop/TessyAI", home: null, onOpen: idle }));
  assert.match(outside, /Move into CrazyAgent folder/);
  assert.match(outside, /CrazyAgent\/TessyAI/);
  assert.equal(inside, "");
  assert.equal(unset, "");
  assert.equal(view.insideHome("/Users/o/CrazyAgent", "/Users/o/CrazyAgent"), true);
  assert.equal(view.insideHome("/Users/o/CrazyAgent2/app", "/Users/o/CrazyAgent"), false);
});

test("Stop the team and move stops the team before the move, and a refused stop moves nothing", async () => {
  const calls = [];
  const done = await view.stopThenMove("alpha", "move", ["worktrees"], {
    stop: (id, stopped) => { calls.push(["stop", id, stopped]); return ok(applied); },
    move: (id, mode, overrides) => { calls.push(["move", id, mode, overrides]); return ok(applied); },
  });
  assert.deepEqual(calls, [["stop", "alpha", true], ["move", "alpha", "move", ["worktrees"]]]);
  assert.equal(done.stopped, true);
  assert.equal(done.result.kind, "ok");
  const moved = [];
  const failed = await view.stopThenMove("alpha", "move", [], {
    stop: () => Promise.resolve({ kind: "error", code: "team_busy", message: "" }),
    move: (id) => { moved.push(id); return ok(applied); },
  });
  assert.deepEqual(moved, []);
  assert.deepEqual(failed, { stopped: false, result: { kind: "error", code: "team_busy", message: "" } });
  const rejected = await view.stopThenMove("alpha", "copy", [], {
    stop: () => ok({ outcome: "rejected", pending: false, rejection: "stale_revision" }),
    move: (id) => { moved.push(id); return ok(applied); },
  });
  assert.deepEqual(moved, []);
  assert.equal(rejected.stopped, false);
  const unconfirmed = await view.stopThenMove("alpha", "copy", [], {
    stop: () => ok({ outcome: "unknown", pending: true, rejection: null }),
    move: (id) => { moved.push(id); return ok(applied); },
  });
  assert.deepEqual(moved, []);
  assert.equal(unconfirmed.stopped, false);
});

test("#383 code F1: the move question lists the folders a move leaves in iCloud, with Download everything, and the move sends it", () => {
  const skipped = [{ path: "node_modules", files: 57108 }, { path: "web/.venv", files: null }];
  const props = { open: true, name: "TessyAI", folder: "/Users/o/Desktop/TessyAI", home: "/Users/o/CrazyAgent", way: "move",
    size: { kind: "ok", value: { bytes: 2048, same_volume: true, skipped, skipped_more: { folders: 3, files: 9 } } },
    busy: false, problem: "", everything: false, onEverything: idle, onWay: idle, onConfirm: idle, onCancel: idle };
  const page = renderToStaticMarkup(createElement(view.MoveHomeDialog, props));
  for (const text of [/Left in iCloud, as they can be rebuilt:/, /node_modules \(57108 files\)/, /web\/\.venv \(files not counted\)/,
    /and 3 more folders \(9 files\)/, /They stay in [^<]*TessyAI; reinstall them in the project after the move\./, /role="switch"/, /Download everything \(slow\)/]) {
    assert.match(page, text);
  }
  assert.match(renderToStaticMarkup(createElement(view.MoveHomeDialog, { ...props, everything: true })), /Downloaded too:/);
  assert.doesNotMatch(renderToStaticMarkup(createElement(view.MoveHomeDialog, { ...props, size: { kind: "ok", value: { bytes: 2048 } } })), /role="switch"|Left in iCloud/);
  assert.match(read("src/screens/project/move-home.tsx"), /stopThenMove\(data\.id, chosen, moveOverrides\(overrides, everything, size\)/);
});

test("#383 code F1 r2 (leader 16:30, B): a failed folder read makes Move and Copy download everything, and the question says so", () => {
  const failed = { kind: "error", code: "folder_size_timeout", message: "Reading the folder took too long." };
  const read = { kind: "ok", value: { bytes: 2048, same_volume: true, skipped: [{ path: "node_modules", files: 3 }] } };
  assert.deepEqual(view.moveOverrides([], false, failed), ["download_everything"]);
  assert.deepEqual(view.moveOverrides(["too_large"], false, { kind: "not_ready" }), ["too_large", "download_everything"]);
  assert.deepEqual(view.moveOverrides([], true, failed), ["download_everything"]);
  assert.deepEqual(view.moveOverrides([], false, read), []);
  assert.deepEqual(view.moveOverrides([], true, read), ["download_everything"]);
  assert.deepEqual(view.moveOverrides([], false, { kind: "loading" }), []);
  assert.deepEqual(view.moveOverrides([], false, null), []);
  const props = { open: true, name: "TessyAI", folder: "/Users/o/Desktop/TessyAI", home: "/Users/o/CrazyAgent", way: "copy", size: failed,
    busy: false, problem: "", everything: false, onEverything: idle, onWay: idle, onConfirm: idle, onCancel: idle };
  const page = renderToStaticMarkup(createElement(view.MoveHomeDialog, props));
  assert.match(page, /CrazyAgent couldn(&#x27;|')t read this folder\. Anything in it only in iCloud is downloaded first, rebuildable folders too, which can be slow\./);
  assert.doesNotMatch(page, /role="switch"/);
  assert.doesNotMatch(renderToStaticMarkup(createElement(view.MoveHomeDialog, { ...props, size: read })), /couldn(&#x27;|')t read this folder/);
});

test("V243-MOVE-SIZE-PROBLEM-DESKTOP: a folder read the engine could not finish, as the bridge sends it, makes Move and Copy download everything, and the question says so", () => {
  for (const code of ["folder_size_timeout", "folder_size_too_large", "folder_size_unreadable"]) {
    const unfinished = { kind: "ok", value: { bytes: null, size_problem: code, placeholders: 0, github: null, worktrees: 0, worktree_paths: [],
      movable: true, git_link: "none", copyable: true, git: true, same_volume: true } };
    assert.deepEqual(view.moveOverrides([], false, unfinished), ["download_everything"], code);
    assert.deepEqual(view.moveOverrides(["worktrees"], false, unfinished), ["worktrees", "download_everything"], code);
    for (const way of ["move", "copy"]) {
      const page = renderToStaticMarkup(createElement(view.MoveHomeDialog, { open: true, name: "TessyAI", folder: "/Users/o/Desktop/TessyAI",
        home: "/Users/o/CrazyAgent", way, size: unfinished, busy: false, problem: "", everything: false, onEverything: idle, onWay: idle, onConfirm: idle, onCancel: idle }));
      assert.match(page, /CrazyAgent couldn(&#x27;|')t read this folder\. Anything in it only in iCloud is downloaded first/, `${code} ${way}`);
      assert.doesNotMatch(page, /role="switch"/, `${code} ${way}`);
    }
  }
  const measured = { kind: "ok", value: { bytes: 2048, placeholders: 0, github: null, worktrees: 0, worktree_paths: [], movable: true,
    git_link: "none", copyable: true, git: true, same_volume: true } };
  assert.deepEqual(view.moveOverrides([], false, measured), []);
});

test("the move question says the team stops first and work not landed follows the move", () => {
  const dialog = (patch = {}) => renderToStaticMarkup(createElement(view.MoveHomeDialog, {
    open: true, name: "TessyAI", folder: "/Users/o/Desktop/TessyAI", home: "/Users/o/CrazyAgent", way: "move",
    size: { kind: "ok", value: { bytes: 2048, same_volume: true } }, busy: false, problem: "", onWay: idle, onConfirm: idle, onCancel: idle, ...patch,
  }));
  const move = dialog();
  assert.match(move, /Move TessyAI into your CrazyAgent folder\. The team stops first; work not landed yet stays in the agents(&#x27;|') copies, which follow the move\./);
  assert.match(move, /Stop the team and move/);
  assert.match(move, /Keep it here/);
  assert.match(move, /quick/);
  const copy = dialog({ way: "copy" });
  assert.match(copy, /Stop the team and copy/);
  assert.match(copy, /The copy becomes the project\. The old folder stays where it is/);
  const reading = dialog({ size: { kind: "loading" } });
  assert.match(reading, /Reading the folder/);
  assert.match(reading, /<button[^>]*disabled=""[^>]*>[^<]*(<[^>]*>[^<]*)*Stop the team and move/);
  assert.match(dialog({ problem: "The team is still stopping. Move again in a few seconds." }), /role="alert"[^>]*>The team is still stopping/);
  assert.equal(dialog({ open: false }), "");
});

test("each refusal of a project move reads as a plain sentence", () => {
  const engine = read("../../crates/crazyagent/src/projects_home/move_project.rs");
  const codes = [...new Set([...engine.matchAll(/"(move_project_[a-z_]+)"/g)].map((match) => match[1]))]
    .filter((code) => !["move_project_copy_not_repointed", "move_project_record_invalid"].includes(code));
  assert.ok(codes.length >= 6, `only ${codes.length} codes read from the engine`);
  for (const code of [...codes, "home_move_project", "move_project_interrupted", "move_project_old_folder_left"]) {
    const line = view.moveCopy(code);
    assert.match(line, /^[A-Z][^_]*\.$/, code);
    assert.ok(!line.includes(code), code);
  }
  assert.equal(view.moveCopy("move_project_busy"), "The team is still stopping, or research or a change is still running. Move again in a few seconds.");
  assert.match(view.moveCopy("home_icloud_not_downloaded"), /iCloud/);
});

test("a move is the engine's job, followed on Home; the dialog only starts it after the team stopped (DEC-177)", () => {
  const source = read("src/screens/project/move-home.tsx");
  assert.match(source, /session\?\.refreshIntake\(result\.value\)/, "the started job shows on Home at once");
  assert.match(source, /Follow it on Home; you can keep working\./);
  assert.doesNotMatch(source, /ICloudDialog|downloadFromIcloud/, "the job downloads what is only in iCloud first (DEC-177 answer 5)");
  assert.doesNotMatch(source, /nextGeneration|receiptState/, "the move is not a pending change of the window");
  assert.match(source, /data\.reload\(\)/, "the settings page reads the project again once its move ended");
  const state = read("src/state.ts");
  assert.match(state, /kind === "move_project"[^;]*refreshProjects\(\)/, "a move that ended reads the projects again");
});

test("the move command is wired from the settings page to the engine's job", () => {
  assert.match(read("src/api-projects.ts"), /call<IntakeJob>\("desktop_move_project_home", \{ id, mode, overrides \}\)/);
  assert.match(read("src/engine-client.ts"), /invoke<IntakeJob>\("desktop_move_project_home", \{ id, mode, overrides \}\)/);
  const lib = read("src-tauri/src/lib.rs");
  const start = lib.indexOf("fn desktop_move_project_home(");
  assert.ok(start >= 0);
  const signature = lib.slice(start, lib.indexOf(")", start));
  for (const param of ["id: String", "mode: AddMode", "overrides: Vec<AddOverride>"]) assert.ok(signature.includes(param), param);
  assert.ok(!signature.includes("generation"), "a job start carries no window generation");
  assert.match(read("src-tauri/src/bridge/intake.rs"), /JobSubject::MoveProject \{ project_id \}/);
  assert.doesNotMatch(read("src-tauri/src/bridge/project_setup.rs"), /OwnerMutation::MoveProjectHome/);
  const settings = read("src/screens/project/settings.tsx");
  assert.match(settings, /<MoveHome data=\{data\} setup=\{setup\} \/>\}\s*<RemoveSection/);
});
