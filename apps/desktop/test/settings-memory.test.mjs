import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const { MemoryView, SuggestionEditor, memorySource, scopeOptions } = await import("../src/screens/settings/memory.tsx");

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const names = new Map([["project-harbor", "Harbor Notes"]]);
const saved = (patch = {}) => ({ id: "memory-1", text: "The owner prefers Rust for engines.", scope: "global", project_id: null, origin: "written", from_project_id: null, ...patch });
const suggestion = (patch = {}) => ({ project_id: "project-harbor", project_name: "Harbor Notes", id: "entry-1", version: 3, text: "Competitors charge per seat.", ...patch });
const props = (patch = {}) => ({ saved: { kind: "ok", value: { items: [saved()], next: null } }, suggested: { kind: "ok", value: [] }, scope: "", projects: [{ id: "project-harbor", name: "Harbor Notes" }],
  search: "", failure: "", busy: null, onSearch: forbidden, onScope: forbidden, onAdd: forbidden, onSave: forbidden, onSaveEdited: forbidden, onDismiss: forbidden, onDelete: forbidden, onMore: forbidden, onRetry: forbidden, ...patch });
const render = (patch) => renderToStaticMarkup(createElement(MemoryView, props(patch)));

test("each saved memory says where it applies and where it came from, in plain words", () => {
  assert.equal(memorySource(saved(), names), "App-wide");
  assert.equal(memorySource(saved({ origin: "promoted", from_project_id: "project-harbor" }), names), "App-wide, from Harbor Notes");
  assert.equal(memorySource(saved({ origin: "promoted", from_project_id: "project-gone" }), names), "App-wide, from a removed project");
  assert.equal(memorySource(saved({ scope: "project", project_id: "project-harbor" }), names), "Harbor Notes only");
});

test("the scope filter offers app-wide memory and one entry per project", () => {
  assert.deepEqual(scopeOptions([{ id: "project-harbor", name: "Harbor Notes" }]), [{ value: "", label: "App-wide" }, { value: "project-harbor", label: "Harbor Notes" }]);
});

test("suggestions from project research offer Save to memory and Dismiss with their project", () => {
  const markup = render({ suggested: { kind: "ok", value: [suggestion()] } });
  assert.match(markup, /Suggested/);
  assert.match(markup, /Competitors charge per seat\./);
  assert.match(markup, /From Harbor Notes/);
  assert.match(markup, /Save to memory/);
  assert.match(markup, />Edit</);
  assert.match(markup, /Dismiss/);
  assert.doesNotMatch(markup, /entry-1|project-harbor/);
});

test("saved memories list with search, add and delete; empty and not ready states stay plain", () => {
  const markup = render();
  assert.match(markup, /The owner prefers Rust for engines\./);
  assert.match(markup, /App-wide/);
  assert.match(markup, /Search memory/);
  assert.match(markup, /Add memory/);
  assert.match(markup, />Delete</);
  assert.doesNotMatch(markup, /memory-1/);
  assert.match(render({ saved: { kind: "ok", value: { items: [], next: null } } }), /No memories yet/);
  assert.match(render({ saved: { kind: "ok", value: { items: [], next: null } }, search: "rust" }), /No memories match/);
  assert.match(render({ saved: { kind: "not_ready" } }), /Not ready yet/);
  assert.match(render({ saved: { kind: "ok", value: { items: [saved()], next: 41 } } }), /Show older/);
  assert.doesNotMatch(render({ suggested: { kind: "not_ready" } }), /Suggested/);
});

test("project research suggestions are the unreviewed provisional memories only", () => {
  const items = [
    { id: "a", version: 1, text: "new", status: "Provisional", human_reviewed: false },
    { id: "b", version: 2, text: "kept", status: "Confirmed", human_reviewed: true },
    { id: "c", version: 2, text: "rejected", status: "Rejected", human_reviewed: true },
  ];
  assert.deepEqual(api.suggestionsFrom({ id: "project-harbor", name: "Harbor Notes" }, items),
    [{ project_id: "project-harbor", project_name: "Harbor Notes", id: "a", version: 1, text: "new" }]);
});

test("memory refusals read as the engine's plain sentences", () => {
  assert.equal(api.plainError("memory_not_kept"), "Only memories you kept can be made global.");
  assert.equal(api.plainError("memory_note_too_long"), "Keep the memory under 2000 characters.");
  assert.equal(api.plainError("memory_already_global"), "This memory is already in global memory.");
});

test("memory calls outside the desktop shell report the engine as unreachable", async () => {
  assert.equal((await api.savedMemories(null, "")).code, "engine_unreachable");
  assert.equal((await api.saveSuggestion(suggestion())).code, "engine_unreachable");
});

test("a suggestion opens for editing with its own text before it is saved", () => {
  const markup = renderToStaticMarkup(createElement(SuggestionEditor, { suggestion: suggestion(), pending: false, onSave: forbidden, onCancel: forbidden }));
  assert.match(markup, /Edit before saving \(from Harbor Notes\)/);
  assert.match(markup, /<textarea[^>]*>Competitors charge per seat\.<\/textarea>/);
  assert.match(markup, /maxlength="4096"/);
  assert.match(markup, />Save edited</);
  assert.match(markup, />Cancel</);
  assert.doesNotMatch(markup, /entry-1/);
  const saving = renderToStaticMarkup(createElement(SuggestionEditor, { suggestion: suggestion(), pending: true, onSave: forbidden, onCancel: forbidden }));
  assert.match(saving, /Saving\.\.\./);
});

test("an edited memory follows the engine's text rules", () => {
  assert.equal(api.MEMORY_TEXT_BYTES_MAX, 4096);
  assert.deepEqual(api.memoryEdit("  Competitors charge per team.  "), { kind: "ok", text: "Competitors charge per team." });
  assert.deepEqual(api.memoryEdit("   "), { kind: "invalid", reason: "Write something to remember." });
  assert.deepEqual(api.memoryEdit("a\0b"), { kind: "invalid", reason: "Remove the empty characters from the memory." });
  assert.equal(api.memoryEdit("x".repeat(4096)).kind, "ok");
  assert.deepEqual(api.memoryEdit("x".repeat(4097)), { kind: "invalid", reason: "Too long: 4097 of 4096 bytes." });
  assert.deepEqual(api.memoryEdit("\u00e9".repeat(2049)), { kind: "invalid", reason: "Too long: 4098 of 4096 bytes." });
  assert.equal(api.plainError("invalid_knowledge_text"), "Write the memory as text, up to 4096 bytes.");
});

test("saving an edited suggestion refuses bad text before the engine and needs the desktop shell", async () => {
  assert.deepEqual(await api.saveEditedSuggestion(suggestion(), " "), { kind: "error", code: "invalid_knowledge_text", message: "Write something to remember." });
  assert.equal((await api.saveEditedSuggestion(suggestion(), "Competitors charge per team.")).code, "engine_unreachable");
});
