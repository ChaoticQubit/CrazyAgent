import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const { MemorySuggestion } = await import("../src/screens/project/memory.tsx");

const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const item = (patch = {}) => ({ id: "entry-7", version: 2, text: "Competitors charge per seat.", status: "Provisional", human_reviewed: false, ...patch });
const props = (patch = {}) => ({ projectId: "project-harbor", item: item(), editing: false, busy: false, onOpenLink: forbidden,
  onSave: forbidden, onDismiss: forbidden, onEdit: forbidden, onSaveEdited: forbidden, onCancel: forbidden, ...patch });
const render = (patch) => renderToStaticMarkup(createElement(MemorySuggestion, props(patch)));

test("a project research memory offers Save to memory, Edit first and Dismiss, like the TUI's y, e and n", () => {
  const markup = render();
  assert.match(markup, /Competitors charge per seat\./);
  assert.match(markup, />Suggested</);
  assert.match(markup, />Save to memory</);
  assert.match(markup, />Edit first</);
  assert.match(markup, />Dismiss</);
  assert.doesNotMatch(markup, /<textarea/);
  assert.doesNotMatch(markup, /entry-7|project-harbor/);
});

test("Edit first opens the memory's own text in an editor with Save edited and Cancel", () => {
  const markup = render({ editing: true });
  assert.match(markup, /<label[^>]*>Edit before saving<\/label>/);
  assert.match(markup, /<textarea[^>]*>Competitors charge per seat\.<\/textarea>/);
  assert.match(markup, /maxlength="4096"/);
  assert.match(markup, />Save edited</);
  assert.match(markup, />Cancel</);
  assert.doesNotMatch(markup, />Edit first</);
  assert.match(render({ editing: true, busy: true }), /Saving\.\.\./);
});

test("an edited project memory is refused before the engine when the text breaks its rules", async () => {
  assert.deepEqual(await api.editProjectMemory("project-harbor", "entry-7", 2, "   ", 1),
    { kind: "error", code: "invalid_knowledge_text", message: "Write something to remember." });
  assert.deepEqual(await api.editProjectMemory("project-harbor", "entry-7", 2, "x".repeat(4097), 1),
    { kind: "error", code: "invalid_knowledge_text", message: "Too long: 4097 of 4096 bytes." });
  assert.equal((await api.editProjectMemory("project-harbor", "entry-7", 2, "Competitors charge per team.", 1)).code, "engine_unreachable");
});
