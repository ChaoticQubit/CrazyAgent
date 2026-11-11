import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement, isValidElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { ConflictView, keepMine, writerName } = await import("../src/screens/conflict.tsx");
const { Button } = await import("../src/ui/index.ts");
const view = await import("../src/screens/process.tsx");

const words = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const never = (what) => () => { throw new Error(`${what} ran during render`); };
const stale = (named, text, writer) => ({ kind: "stale", version: named, current: { text, version: named + 1 }, writer });

function elements(node, found = []) {
  if (Array.isArray(node)) {
    for (const child of node) elements(child, found);
  } else if (isValidElement(node)) {
    found.push(node);
    elements(node.props.children, found);
  }
  return found;
}

function button(tree, label) {
  const matches = elements(tree).filter((element) => element.type === Button && element.props.children === label);
  assert.equal(matches.length, 1, `one ${label} button`);
  return matches[0];
}

function processPage(extra = {}) {
  return {
    scope: { kind: "app" }, saved: { kind: "ok", value: "Old rule." }, draft: "Mine. Old rule.", skills: [], failure: "", busy: null,
    preview: null, conflict: null, onDraft: never("onDraft"), onSave: never("onSave"), onImport: never("onImport"), onDefault: never("onDefault"),
    onUsePreview: never("onUsePreview"), onClosePreview: never("onClosePreview"), onRetry: never("onRetry"),
    onReload: never("onReload"), onKeepMine: never("onKeepMine"), ...extra,
  };
}

test("a stale save shows who changed the text since it was opened, the text that landed and two choices, with the draft kept", () => {
  const shown = view.conflictOf(stale(4, "Desktop rule.", "owner"));
  assert.deepEqual(shown, { opened: 4, version: 5, current: "Desktop rule.", writer: "owner" });
  const markup = renderToStaticMarkup(createElement(view.ProcessView, processPage({ conflict: shown })));
  const text = words(markup);
  assert.match(text, /Changed by you in another window since you opened it\./);
  assert.match(text, /Desktop rule\./);
  assert.match(text, /Reload/);
  assert.match(text, /Keep mine/);
  assert.match(markup, /Mine\. Old rule\./, "the owner's draft stays in the editor");
  const save = markup.match(/<button[^>]*type="submit"[^>]*>/)?.[0];
  assert.ok(save, "the page has a Save button");
  assert.match(save, /disabled/, "a plain Save is not a third choice while the conflict shows");
  assert.doesNotMatch(text, /Discard edits/);
  assert.doesNotMatch(words(renderToStaticMarkup(createElement(view.ProcessView, processPage()))), /Changed by|Keep mine/);
});

test("Reload and Keep mine are the only paths: Keep mine names the version shown and the one opened, and a refusal shows the newer text", () => {
  const shown = view.conflictOf(stale(4, "Agent rule.", { agent: "team-3" }));
  assert.equal(writerName(shown.writer), "Agent 3");
  assert.equal(writerName("owner"), "you in another window");
  assert.equal(writerName("nobody"), "another save");
  assert.equal(writerName({ agent: "research-1" }), "research-1");

  const chosen = [];
  const panel = ConflictView({ conflict: shown, busy: false, onReload: () => chosen.push("reload"), onKeepMine: () => chosen.push("keep"), children: "Agent rule." });
  assert.match(words(renderToStaticMarkup(panel)), /Changed by Agent 3 since you opened it\..*Agent rule\./);
  button(panel, "Reload").props.onClick();
  button(panel, "Keep mine").props.onClick();
  assert.deepEqual(chosen, ["reload", "keep"]);
  assert.deepEqual(keepMine(shown), { version: 5, opened: 4 }, "Keep mine saves over the version shown and names the one opened, so the engine records it");

  const again = view.conflictOf(stale(5, "Newer rule.", "owner"));
  assert.deepEqual(keepMine(again), { version: 6, opened: 5 });
  const waiting = ConflictView({ conflict: again, busy: true, onReload: never("onReload"), onKeepMine: never("onKeepMine"), children: "Newer rule." });
  assert.equal(button(waiting, "Reload").props.disabled, true);
  assert.equal(button(waiting, "Keep mine").props.disabled, true);

  const onReload = () => {};
  const onKeepMine = () => {};
  const page = elements(view.ProcessView(processPage({ conflict: again, onReload, onKeepMine })));
  const placed = page.filter((element) => element.type === ConflictView);
  assert.equal(placed.length, 1);
  assert.equal(placed[0].props.conflict, again);
  assert.equal(placed[0].props.onReload, onReload);
  assert.equal(placed[0].props.onKeepMine, onKeepMine);
});
