import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const model = await import("../src/screens/changes/model.ts");
const view = await import("../src/screens/changes/index.tsx");
const client = await import("../src/api-changes.ts");

const NOW = Date.UTC(2026, 9, 26, 18, 0);
const set = (extra = {}) => ({
  id: "cs-1", agent: { kind: "sub", id: "team-1", label: "agent-1" }, state: "pending", base: "a".repeat(40), head: "b".repeat(40),
  files: 3, added: 12, removed: 4, last_outcome: "succeeded", checked: false, created_ms: NOW - 60_000, updated_ms: NOW - 1000,
  restorable_until_ms: null, conflicts: [], regenerated: [], message: "Add a file", waiting: false, ...extra,
});
const file = (extra = {}) => ({ path: "src/app.ts", old_path: null, status: "modified", kind: "text", marks: [], ...extra });

test("a numbered change set is named by its number, agent, subject and files wherever it is named (AL17)", () => {
  const wren = set({ number: 16, agent: { kind: "sub", id: "team-2", label: "Wren" }, message: "checkout UI\n\nThe form and its summary.", files: 7 });
  assert.equal(model.changeSetName(wren), "Change set #16 (Wren: checkout UI, 7 files)");
  assert.equal(model.changeSetName(set({ number: 1, agent: { kind: "master", id: "s-1", label: "master" }, message: "", files: 1 })), "Change set #1 (Master, 1 file)");
  assert.equal(model.changeSetName(set({ number: 2, message: "x".repeat(80) })), `Change set #2 (agent-1: ${"x".repeat(45)}..., 3 files)`);
  assert.equal(model.changeSetName(set({ number: null })), "agent-1", "a set the engine has not numbered yet is named by its agent");
  assert.equal(model.changeSetName(set()), "agent-1", "an engine without numbers sends none");
  const row = renderToStaticMarkup(createElement(view.ChangeRow, { set: wren, active: false, onSelect() {} }));
  assert.match(row, /<strong>Change set #16 \(Wren: checkout UI, 7 files\)<\/strong>/u);
});

test("a change set reads as its agent, a state word, its size and that CrazyAgent did not check it", () => {
  assert.equal(model.agentName(set()), "agent-1");
  assert.equal(model.agentName(set({ agent: { kind: "sub", id: "7", label: "Ada (@onboarding-fix) \u00b7 Claude Code" } })), "Ada (@onboarding-fix) \u00b7 Claude Code");
  assert.equal(model.agentName(set({ agent: { kind: "master", id: "s-1", label: "master" } })), "Master");
  assert.equal(model.sizeLine(set()), "3 files, 12 lines added, 4 removed");
  assert.equal(model.sizeLine(set({ files: 1, added: 1, removed: 0 })), "1 file, 1 line added, 0 removed");
  assert.deepEqual(model.stateBadge(set()), { word: "Ready to review", tone: "info" });
  assert.deepEqual(model.stateBadge(set({ state: "conflict" })), { word: "Conflicts with your branch", tone: "warn" });
  assert.deepEqual(model.stateBadge(set({ state: "updating" })), { word: "Agent is updating", tone: "info" });
  assert.deepEqual(model.stateBadge(set({ state: "accepted" })), { word: "Accepted", tone: "ok" });
  assert.deepEqual(model.stateBadge(set({ state: "discarded" })), { word: "Discarded", tone: "neutral" });
  assert.equal(model.checkedNote(set()), "Not checked by CrazyAgent. Any test results in it are reported by the agent.");
  assert.equal(model.checkedNote(set({ checked: true })), null);
  assert.equal(model.outcomeNote(set()), null);
  assert.equal(model.outcomeNote(set({ last_outcome: "failed" })), "The agent's last turn failed. These are the changes it made before it stopped.");
  assert.equal(model.outcomeNote(set({ last_outcome: "cancelled" })), "The agent's last turn was stopped. These are the changes it made before it stopped.");
  assert.equal(model.outcomeNote(set({ last_outcome: "failed", state: "accepted", landing_result: "landed" })), "Last turn: failed. Landing: landed. How the turn ended does not change what landed.", "OA CS#035688c8: a landed set whose turn failed says both facts (F23)");
  assert.equal(model.outcomeNote(set({ last_outcome: "cancelled", state: "accepted" })), "Last turn: stopped. Landing: landed. How the turn ended does not change what landed.");
  assert.equal(model.landingNote(set({ landing_result: "refused" })), "Landing: the last Accept was refused, so nothing landed yet.");
  assert.equal(model.landingNote(set({ landing_result: "refused", state: "accepted" })), null, "a closed set says its state instead");
  assert.equal(model.landingNote(set()), null, "an engine without landing results sends none");
});

test("a set its agent is fixing for conflicts with main says so in Changes", () => {
  assert.deepEqual(model.stateBadge(set({ state: "updating", conflicts: ["README"] })), { word: "Agent is fixing conflicts with main", tone: "info" });
  assert.deepEqual(model.stateBadge(set({ state: "updating", conflicts: ["README"], waiting: true })), { word: "Updated, waiting for the agent", tone: "warn" });
  assert.deepEqual(model.stateBadge(set({ state: "updating" })), { word: "Agent is updating", tone: "info" });
});

test("each state offers only the actions the engine can take", () => {
  assert.deepEqual(model.actions(set(), NOW), { accept: true, discard: true, update: false, restore: false });
  assert.deepEqual(model.actions(set({ state: "conflict" }), NOW), { accept: true, discard: true, update: true, restore: false });
  assert.deepEqual(model.actions(set({ state: "updating" }), NOW), { accept: false, discard: false, update: false, restore: false });
  assert.deepEqual(model.actions(set({ state: "accepted" }), NOW), { accept: false, discard: false, update: false, restore: false });
  const discarded = set({ state: "discarded", restorable_until_ms: NOW + 3_600_000 });
  assert.deepEqual(model.actions(discarded, NOW), { accept: false, discard: false, update: false, restore: true });
  assert.deepEqual(model.actions({ ...discarded, restorable_until_ms: NOW - 1 }, NOW), { accept: false, discard: false, update: false, restore: false });
  assert.deepEqual(model.actions({ ...discarded, restorable_until_ms: null }, NOW), { accept: false, discard: false, update: false, restore: false });
  assert.match(model.restoreNote(discarded, NOW), /^You can restore these changes until [A-Z][a-z]{2} \d{1,2}, \d{2}:\d{2}\.$/);
  assert.equal(model.restoreNote({ ...discarded, restorable_until_ms: NOW - 1 }, NOW), "These changes can no longer be restored.");
  assert.equal(model.restoreNote(set(), NOW), null);
  assert.equal(model.updatingNote(set({ state: "updating" })), "The agent is resolving the conflicts with your branch. Review again when it finishes.");
  assert.equal(model.conflictNote(set({ state: "conflict", conflicts: ["a.ts", "b.ts"] })), "These files also changed on your branch: a.ts, b.ts. Ask the agent to update, or discard.");
  assert.equal(model.conflictNote(set({ state: "conflict" })), "These changes conflict with your branch. Ask the agent to update, or discard.");
  assert.equal(model.conflictNote(set({ conflicts: ["a.ts"] })), null);
  assert.equal(model.acceptNote({ kind: "landed", commit: "c".repeat(40), branch: "main" }), "Accepted as ccccccc on main.");
  assert.equal(model.acceptNote({ kind: "conflict", files: ["src/app.ts"] }), null, "a conflict is told once, by the change set's own note");
});

test("every file says how it changed and marks what can change more than its text", () => {
  assert.equal(model.fileLine(file()), "Modified src/app.ts");
  assert.equal(model.fileLine(file({ status: "added" })), "Added src/app.ts");
  assert.equal(model.fileLine(file({ status: "deleted" })), "Deleted src/app.ts");
  assert.equal(model.fileLine(file({ status: "renamed", old_path: "src/old.ts" })), "Renamed src/old.ts to src/app.ts");
  assert.deepEqual(model.fileMarks(file()), []);
  assert.deepEqual(model.fileMarks(file({ kind: "binary" })), ["Binary file, no text diff"]);
  assert.deepEqual(model.fileMarks(file({ kind: "symlink", marks: ["symlink"] })), ["Symbolic link: points to another path"]);
  assert.deepEqual(model.fileMarks(file({ kind: "gitlink" })), ["Submodule commit"]);
  assert.deepEqual(model.fileMarks(file({ marks: ["executable", "mode"] })), ["Can run as a program", "File mode changed"]);
  assert.deepEqual(model.fileMarks(file({ path: ".gitattributes", marks: ["attributes"] })), [".gitattributes: changes how Git treats files"]);
  assert.deepEqual(model.fileMarks(file({ path: ".gitmodules", marks: ["modules"] })), [".gitmodules: changes the project's submodules"]);
  assert.deepEqual(model.fileMarks(file({ path: "AGENTS.md", marks: ["instructions"] })), ["Instruction file: changes what every agent of this project is told"]);
  for (const path of ["claude.md", "web/Agents.md", "AGENTS.override.md", ".claude/commands/x.md"]) {
    assert.deepEqual(model.fileMarks(file({ path, marks: ["instructions"] })), ["Instruction file: changes what every agent of this project is told"], path);
  }
  assert.equal(model.fileMarks(file({ marks: ["new_mark"] })).length, 1, "a mark this build does not know is still shown");
  assert.equal(model.fileMarks(file({ marks: ["new_mark"] }))[0], "Marked new mark");
  assert.equal(model.fileLine(file({ path: "evil\u001b[31m.ts" })), "Modified evil\\u001b[31m.ts");
});

test("diff text is shown as plain text: markup stays text and control characters are visible", () => {
  const text = "+<img src=x onerror=alert(1)>\n-\u001b[31mred\u001b[0m\n\tkept tab";
  assert.equal(model.diffText(text), "+<img src=x onerror=alert(1)>\n-\\u001b[31mred\\u001b[0m\n\tkept tab");
  const html = renderToStaticMarkup(createElement(view.DiffText, { text }));
  assert.ok(html.startsWith("<pre"), html);
  assert.ok(html.includes("+&lt;img src=x onerror=alert(1)&gt;"), html);
  assert.ok(!html.includes("<img"), html);
  assert.ok(!html.includes("\u001b"), html);
});

test("every refusal reads as a plain sentence, and a conflict names its files", () => {
  assert.equal(client.changesRefusal({ code: "changes_owner_dirty" }), "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again.");
  assert.equal(client.changesRefusal({ code: "changes_detached_head" }), "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again.");
  assert.equal(client.changesRefusal({ code: "changes_git_busy" }), "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again.");
  assert.equal(client.changesRefusal({ code: "changes_conflict", files: ["a.ts", "b.ts"] }), "These files also changed on your branch: a.ts, b.ts. Ask the agent to update, or discard.");
  assert.equal(client.changesRefusal({ code: "changes_conflict", files: ["1", "2", "3", "4", "5", "6", "7"] }), "These files also changed on your branch: 1, 2, 3, 4, 5 and 2 more. Ask the agent to update, or discard.");
  assert.equal(client.changesRefusal({ code: "changes_conflict", files: [] }), "These changes conflict with your branch. Ask the agent to update, or discard.");
  assert.equal(client.changesRefusal({ code: "changes_stale" }), "These changes moved on since you opened them. Review them again, then accept.");
  assert.equal(client.changesRefusal({ code: "changes_branch_moved" }), "Your branch moved while these changes were landing, so nothing changed. Review and accept again.");
  assert.equal(client.changesRefusal({ code: "changes_git_unsupported" }), "The Git on this computer can't land changes. Update Git, then try again.");
  assert.equal(client.changesRefusal({ code: "changes_restore_expired" }), "These changes can no longer be restored. Discarded changes are kept for 7 days.");
  assert.equal(client.changesRefusal({ code: "changes_agent_has_pending" }), "This agent has newer changes waiting. Accept or discard those first.");
  assert.equal(client.changesRefusal({ code: "changes_agent_busy" }), "The agent is working right now. Try again when it finishes.");
  assert.equal(client.changesRefusal({ code: "changes_git_failed" }), "Git couldn't finish this step in your project folder. Try again; if it keeps failing, check the folder with Git.");
  assert.equal(client.changesRefusal({ code: "changes_not_found" }), "These changes are no longer there. The list shows what is left.");
  assert.equal(client.changesRefusal({ code: "changes_outcome_unknown" }), "CrazyAgent couldn't confirm what happened. Check the list before you try again.");
  assert.equal(client.changesRefusal({ code: "changes_owner_identity_missing" }), "Git has no name and email for you in your project folder. Set user.name and user.email, then accept again.");
  assert.equal(client.changesRefusal({ code: "changes_owner_unavailable" }), "Your project folder couldn't be read. Check that it is still there, then try again.");
  assert.equal(client.changesRefusal({ code: "changes_too_large" }), "These changes are larger than CrazyAgent can show or land. Ask the agent to split them up.");
  assert.equal(client.changesRefusal({ code: "changes_bare_repository" }), "These changes add a folder Git would open as a repository: a HEAD file beside objects and refs. Ask the agent to remove it.");
  assert.equal(client.changesRefusal({ code: "changes_link_outside" }), "These changes add a link that points outside the project or into an agent's folder. Ask the agent to remove it.");
  assert.equal(client.changesRefusal({ code: "changes_message_invalid" }), "The commit message is empty, too long or has characters Git can't keep. Edit it, then accept again.");
  assert.equal(client.changesRefusal({ code: "changes_workspace_invalid" }), "The agent's working copy was moved or changed outside CrazyAgent, so these changes can't land from it.");
  assert.equal(client.changesRefusal({ code: "changes_master_tools_off" }), "This master has tools off, so it can't edit files to update these changes. Turn tools on for the master, or discard.");
  assert.equal(client.changesRefusal({ code: "changes_master_closed" }), "The master conversation these changes came from is closed, so it can't update them. Discard them, or start a new conversation.");
  assert.equal(client.changesRefusal({ code: "changes_master_not_started" }), "Your branch is merged into the master's copy, but the master couldn't start resolving it yet. It starts after the master's next turn ends.");
  assert.equal(client.changesRefusal({ code: "made_up_code" }), "Something went wrong. Try again. (made_up_code)");
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/changes.rs", import.meta.url), "utf8");
  const codes = engine.slice(engine.indexOf("pub const CHANGES_CODES"), engine.indexOf("];", engine.indexOf("pub const CHANGES_CODES")));
  const own = [...codes.matchAll(/"(changes_[a-z_]+)"/g)].map((match) => match[1]);
  assert.ok(own.length >= 20, `engine changes codes: ${own.length}`);
  for (const code of own.filter((code) => !["changes_request_bounds", "changes_reply_bounds", "changes_request_invalid"].includes(code))) {
    assert.ok(client.CHANGES_REFUSALS.includes(code), `${code} has its own sentence`);
  }
  const team = [...codes.matchAll(/"(team_[a-z_]+)"/g)].map((match) => match[1]);
  assert.deepEqual(team, ["team_agent_not_found", "team_conversation_full", "team_task_invalid"], "an Update's team refusals reach the window as themselves");
  for (const code of team) assert.doesNotMatch(client.changesRefusal({ code }), /Something went wrong|_/, `${code} reads as its own sentence`);
  assert.equal(client.changesRefusal({ code: "team_agent_not_found" }), "The agent that made these changes is no longer on the team, so it can't update them. Discard them instead.");
  for (const code of client.CHANGES_REFUSALS) {
    const line = client.changesRefusal({ code, files: ["x"] });
    assert.match(line, /^[A-Z].*\.$/, code);
    assert.doesNotMatch(line, /_/, `${code} reads as a sentence: ${line}`);
  }
  assert.ok(client.CHANGES_REFUSALS.length >= 11);
});

test("Changes says Not ready yet when the engine or this build cannot land changes", () => {
  assert.equal(client.notReady({ kind: "not_ready" }), true);
  assert.equal(client.notReady({ kind: "error", code: "changes_unavailable", message: "" }), true);
  assert.equal(client.notReady({ kind: "error", code: "changes_owner_dirty", message: "" }), false);
  assert.equal(client.notReady({ kind: "ok", value: [] }), false);
  const html = renderToStaticMarkup(createElement(view.ChangesNotReady));
  assert.match(html, /Not ready yet: /);
});

test("Accept shows the proposed commit message to edit and sends only a usable one", () => {
  assert.equal(model.commitMessage("  Owner title\r\n\r\nWhy it changed\n"), "Owner title\n\nWhy it changed");
  assert.equal(model.commitMessage(" \r\n\t "), null);
  assert.equal(model.commitMessage("x".repeat(4096)), "x".repeat(4096));
  assert.equal(model.commitMessage("\u00e9".repeat(2049)), null);
  const proposed = set({ message: "Add a main\n\n- Add the <fourth> line" });
  const html = renderToStaticMarkup(createElement(view.AcceptDialog, { set: proposed, pending: false, onCancel() {}, onAccept() {} }));
  assert.match(html, /Accept these changes\?/);
  assert.match(html, /under your own Git name\. CrazyAgent did not check them\./);
  assert.match(html, /<label for="[^"]+">Commit message<\/label>/);
  assert.match(html, /<textarea[^>]*>Add a main\n\n- Add the &lt;fourth&gt; line<\/textarea>/);
  assert.doesNotMatch(html, /Write a commit message/);
  assert.doesNotMatch(html, /<button[^>]*disabled=""[^>]*>Accept/);
  const blank = renderToStaticMarkup(createElement(view.AcceptDialog, { set: set({ message: "" }), pending: false, onCancel() {}, onAccept() {} }));
  assert.match(blank, /Write a commit message of up to 4096 bytes, with no hidden characters, to accept\./);
  assert.match(blank, /<button[^>]*disabled=""[^>]*>Accept<\/button>/);
});

test("Accept lands on the team's Land on branch unless the owner picks another, and says where and whether it is checked out", () => {
  const listed = { branches: ["feat/x", "main", "stage"], current: "feat/x", land_on: "stage" };
  const away = "They land as one commit on stage, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are. CrazyAgent did not check them.";
  assert.deepEqual(model.acceptTarget(listed, "stage"), { picked: null, text: away });
  assert.deepEqual(model.acceptTarget(listed, "feat/x"), { picked: "feat/x", text: "They land as one commit on feat/x, the branch checked out in your project folder, under your own Git name. CrazyAgent did not check them." });
  assert.equal(model.acceptTarget(listed, "main").picked, "main");
  assert.deepEqual(model.acceptTarget({ branches: ["main"], current: "main", land_on: null }, "main"),
    { picked: null, text: "They land as one commit on main, the branch checked out in your project folder, under your own Git name. CrazyAgent did not check them." },
    "with no Land on branch the engine lands on the checked-out one");
  assert.deepEqual(model.acceptTarget(null, ""), { picked: null, text: "They land as one commit on your project's Land on branch, under your own Git name. CrazyAgent did not check them." });
  assert.deepEqual(listed.branches.map((name) => model.branchLabel(listed, name)), ["feat/x (checked out)", "main", "stage (Land on)"]);
  assert.equal(model.branchLabel({ branches: ["main"], current: "main", land_on: "main" }, "main"), "main (Land on, checked out)");
});

test("Accept with a missing Land on branch names it, offers no default and waits for a picked branch", () => {
  const missing = "Your Land on branch stage is not a local branch of this project. Pick a branch, or change Land on in Team settings.";
  const listed = { branches: ["feat/x", "main"], current: "feat/x", land_on: null, land_on_problem: missing };
  assert.deepEqual(model.acceptTarget(listed, ""), { picked: null, text: missing });
  assert.equal(model.acceptBlocked(listed, ""), true);
  assert.deepEqual(model.acceptTarget(listed, "feat/x"), { picked: "feat/x", text: "They land as one commit on feat/x, the branch checked out in your project folder, under your own Git name. CrazyAgent did not check them." });
  assert.equal(model.acceptBlocked(listed, "feat/x"), false);
  assert.equal(model.acceptTarget(listed, "main").picked, "main");
  assert.equal(model.acceptBlocked({ branches: ["main"], current: "main", land_on: "main" }, "main"), false);
  assert.equal(model.acceptBlocked(null, ""), false);
});

test("Accept shows hidden characters in a proposed message as escapes and never sends them", () => {
  assert.equal(model.commitMessage("Owner title\u{e0041}\u{e0042}"), null);
  assert.equal(model.commitMessage("Zero\u200bwidth"), null);
  assert.equal(model.commitMessage("Ship it \u{1f468}\u200d\u{1f4bb}"), "Ship it \u{1f468}\u200d\u{1f4bb}");
  const hidden = set({ message: "Formats changed files.\u{e0041}\u{e0042}" });
  const html = renderToStaticMarkup(createElement(view.AcceptDialog, { set: hidden, pending: false, onCancel() {}, onAccept() {} }));
  assert.match(html, /<textarea[^>]*>Formats changed files\.\\u\{e0041\}\\u\{e0042\}<\/textarea>/);
  assert.doesNotMatch(html, /[\u{e0000}-\u{e0fff}]/u);
  assert.match(html, /The proposed message held characters you cannot see\. They are shown as escapes: check it before you accept\./);
  assert.doesNotMatch(html, /<button[^>]*disabled=""[^>]*>Accept/);
  const plain = renderToStaticMarkup(createElement(view.AcceptDialog, { set: set({ message: "Add a file" }), pending: false, onCancel() {}, onAccept() {} }));
  assert.doesNotMatch(plain, /characters you cannot see/);
});

test("a set merged but not yet handed to its agent says so and can be asked again or discarded", () => {
  const waiting = set({ state: "updating", waiting: true });
  assert.deepEqual(model.stateBadge(waiting), { word: "Updated, waiting for the agent", tone: "warn" });
  assert.equal(model.updatingNote(waiting), "Your branch is merged into the agent's copy, but the agent wasn't handed the work to resolve it. Ask it to update again, or discard.");
  assert.deepEqual(model.actions(waiting, NOW), { accept: false, discard: true, update: true, restore: false });
  assert.deepEqual(model.actions(set({ state: "updating" }), NOW), { accept: false, discard: false, update: false, restore: false });
  assert.deepEqual(model.stateBadge(set({ state: "pending", waiting: true })), { word: "Ready to review", tone: "info" });
  assert.equal(client.changesRefusal({ code: "team_agent_not_found" }), "The agent that made these changes is no longer on the team, so it can't update them. Discard them instead.");
});

test("a lockfile the project's tools rebuilt is named as their work while its set waits for review", () => {
  const rebuilt = set({ regenerated: ["Cargo.lock", "web/package-lock.json"] });
  assert.equal(model.regeneratedNote?.(rebuilt), "Rebuilt with this project's own tools to fit your branch: Cargo.lock, web/package-lock.json. They ran on the agent's work, so review the result like the agent's own edits.");
  assert.equal(model.regeneratedNote?.(set()), null);
  assert.equal(model.regeneratedNote?.({ ...rebuilt, state: "accepted" }), null);
  assert.match(readFileSync(new URL("../src/screens/changes/index.tsx", import.meta.url), "utf8"), /regeneratedNote\(set\)/);
});

test("a change set that moved on while open goes back to review: Accept and Discard wait for its last page and send the head reviewed", () => {
  const opened = model.reviewOf(null, set());
  assert.deepEqual(opened, { id: "cs-1", head: "b".repeat(40), moved: false, seenAll: false });
  assert.equal(model.reviewed(opened), true);
  assert.equal(model.movedNote(opened), null);
  assert.equal(model.reviewOf(opened, set()), opened, "a reread with the same head keeps the review");

  const moved = model.reviewOf(opened, set({ head: "c".repeat(40) }));
  assert.deepEqual(moved, { id: "cs-1", head: "c".repeat(40), moved: true, seenAll: false });
  assert.equal(model.reviewed(moved), false);
  assert.match(model.movedNote(moved), /moved on since you opened them/);
  assert.equal(model.lastPageSeen(moved, "b".repeat(40)), moved, "a page of the old head does not count");

  const seen = model.lastPageSeen(moved, "c".repeat(40));
  assert.equal(model.reviewed(seen), true);
  assert.equal(model.movedNote(seen), null);
  assert.equal(seen.head, "c".repeat(40));

  const again = model.reviewOf(seen, set({ head: "d".repeat(40) }));
  assert.equal(model.reviewed(again), false, "a second move needs a second read");
  const other = model.reviewOf(again, set({ id: "cs-2", head: "e".repeat(40) }));
  assert.deepEqual(other, { id: "cs-2", head: "e".repeat(40), moved: false, seenAll: false });

  const source = readFileSync(new URL("../src/screens/changes/index.tsx", import.meta.url), "utf8");
  assert.match(source, /disabled=\{busy \|\| !settled\} onClick=\{\(\) => onAsk\(\{ kind: "accept", set: asked \}\)\}/);
  assert.match(source, /disabled=\{busy \|\| !settled\} onClick=\{\(\) => onAsk\(\{ kind: "discard", set: asked \}\)\}/);
  assert.match(source, /const asked = \{ \.\.\.set, head \};/);
});

test("a change set closed outside the app says how, keeps its note and offers no action", () => {
  const landed = set({ state: "accepted", outside: true });
  const removed = set({ state: "discarded", outside: true, restorable_until_ms: null });
  assert.deepEqual(model.stateBadge(landed), { word: "Landed outside the app", tone: "ok" });
  assert.deepEqual(model.stateBadge(removed), { word: "Removed outside the app", tone: "neutral" });
  assert.equal(model.restoreNote(landed, NOW), "Your branch already has these changes. They were landed outside the app.");
  assert.equal(model.restoreNote(removed, NOW), "The agent's copy of these changes was removed outside the app, so they cannot be restored.");
  const none = { accept: false, discard: false, update: false, restore: false };
  assert.deepEqual(model.actions(landed, NOW), none);
  assert.deepEqual(model.actions(removed, NOW), none);
});
test("every diff page is read at the head under review, and a page refused as moved on rereads the list (RV-DS-13)", () => {
  assert.equal(client.loadChangeDiff.length, 3, "the diff read names the change set, the head and the page");
  const api = readFileSync(new URL("../src/api-changes.ts", import.meta.url), "utf8");
  assert.match(api, /call<ChangeDiff>\("desktop_change_diff", \{ id, head, page \}\)/);
  const source = readFileSync(new URL("../src/screens/changes/index.tsx", import.meta.url), "utf8");
  assert.match(source, /load = loadChangeDiff \}/, "Detail reads pages from the engine unless a test hands it a reader");
  assert.match(source, /void load\(set\.id, head, page\)/);
  assert.match(source, /if \(result\.kind === "error" && result\.code === "changes_stale"\) \{\n\s+setStaleAt\(set\);\n\s+setReview\(movedOn\);\n\s+onMoved\(\);/);
  assert.match(source, /const \[list, reread\] = useChangeList\(data\.id\);/);
  assert.match(source, /onMoved=\{reread\}/);
});
