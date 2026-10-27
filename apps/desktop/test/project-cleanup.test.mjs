import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const view = await import("../src/screens/project/cleanup.tsx");
const api = await import("../src/api-cleanup.ts");

const agent = (id, label, bytes, countedAll = true) => ({
  id, role: id === 1 ? "master" : "sub", label, harness: id === 1 ? "claude" : "codex", stopped_ms: 1_000, bytes, counted_all: countedAll,
});
const finished = [agent(3, "Ada (@login-page)", 1024 * 1024 * 1024), agent(2, "Grace (@signup)", 512 * 1024 * 1024)];
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const render = (props) => renderToStaticMarkup(createElement(view.CleanupView, {
  open: false, pending: false, note: "", harness: (id) => ({ claude: "Claude Code", codex: "Codex" })[id] ?? id,
  onOpen() {}, onCancel() {}, onConfirm() {}, ...props,
}));

test("the button names how many finished agents there are and the space they hold", () => {
  assert.equal(view.cleanupLabel(finished), "Clean up 2 finished agents (1.5 GB)");
  assert.equal(view.cleanupLabel([finished[1]]), "Clean up 1 finished agent (512 MB)");
  assert.equal(view.cleanupLabel([agent(4, "Lin", 2048, false), finished[1]]), "Clean up 2 finished agents (at least 512 MB)");
  const row = text(render({ list: { kind: "ok", value: finished } }));
  assert.match(row, /Finished agents keep their working copy until you remove it\. Clean up 2 finished agents \(1\.5 GB\)/);
  assert.doesNotMatch(row, /Remove 2 finished agents\?/);
});

test("the confirm dialog lists every agent with its harness and size before anything is removed", () => {
  const dialog = text(render({ list: { kind: "ok", value: finished }, open: true }));
  assert.match(dialog, /Remove 2 finished agents\?/);
  assert.match(dialog, /Each agent's working copy and folder are deleted\. Their landed work stays in your project\./);
  assert.match(dialog, /Ada \(@login-page\) Codex 1 GB/);
  assert.match(dialog, /Grace \(@signup\) Codex 512 MB/);
  assert.match(dialog, /Keep them Remove and free 1\.5 GB/);
});

test("nothing shows with no finished agent, while loading or without cleanup; a failed read says why", () => {
  assert.equal(render({ list: { kind: "ok", value: [] }, open: true }), "");
  assert.equal(render({ list: { kind: "loading" } }), "");
  assert.equal(render({ list: { kind: "not_ready" } }), "");
  assert.equal(render({ list: { kind: "error", code: "cleanup_unavailable", message: "" } }), "");
  const failed = text(render({ list: { kind: "error", code: "cleanup_worktree_dirty", message: "" } }));
  assert.match(failed, /Couldn't read finished agents\. An agent's working copy has changes that aren't saved anywhere else\./);
  assert.match(text(render({ list: { kind: "ok", value: [] }, note: "Removed 2 finished agents and freed 1.5 GB." })), /Removed 2 finished agents and freed 1\.5 GB\./);
});

test("every cleanup code the engine can answer with has its own sentence or the shared copy", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/cleanup.rs", import.meta.url), "utf8");
  const listed = source.slice(source.indexOf("CLEANUP_CODES"), source.indexOf("];")).match(/"[a-z_]+"/gu).map((code) => code.slice(1, -1));
  const own = listed.filter((code) => code.startsWith("cleanup_") && !["cleanup_reply_bounds", "cleanup_request_bounds"].includes(code));
  assert.ok(own.length >= 12);
  for (const code of own) assert.ok(api.CLEANUP_REFUSALS.includes(code), code);
  assert.match(api.cleanupRefusal({ code: "cleanup_reply_bounds" }), /cleanup_reply_bounds/);
});
