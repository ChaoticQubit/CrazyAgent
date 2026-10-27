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
  restorable_until_ms: null, conflicts: [], ...extra,
});
const file = (extra = {}) => ({ path: "src/app.ts", old_path: null, status: "modified", kind: "text", marks: [], ...extra });

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
