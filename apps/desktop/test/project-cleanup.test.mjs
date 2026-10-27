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
  asked: null, pending: false, note: "", harness: (id) => ({ claude: "Claude Code", codex: "Codex" })[id] ?? id,
  onOpen() {}, onCancel() {}, onConfirm() {}, onReview() {}, ...props,
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
  const dialog = text(render({ list: { kind: "ok", value: finished }, asked: finished }));
  assert.match(dialog, /Remove 2 finished agents\?/);
  assert.match(dialog, /Each agent's working copy and folder are deleted\. Their landed work stays in your project\./);
  assert.match(dialog, /Ada \(@login-page\) Codex 1 GB/);
  assert.match(dialog, /Grace \(@signup\) Codex 512 MB/);
  assert.match(dialog, /Keep them Remove and free 1\.5 GB/);
});

test("nothing shows with no finished agent, while loading or without cleanup; a failed read says why", () => {
  assert.equal(render({ list: { kind: "ok", value: [] }, asked: [] }), "");
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

test("the dialog keeps the list it opened with and a changed read asks again before anything is sent", () => {
  const late = [...finished, agent(5, "Lin (@docs)", 1024)];
  assert.deepEqual(view.cleanupRequest(finished, { kind: "ok", value: finished }), [3, 2]);
  assert.deepEqual(view.cleanupRequest(finished, { kind: "ok", value: [finished[1], finished[0]] }), [3, 2], "the same agents in another order are the same list");
  assert.equal(view.cleanupRequest(finished, { kind: "ok", value: late }), null, "a list change behind the open dialog sends nothing");
  assert.equal(view.cleanupRequest(finished, { kind: "ok", value: [finished[0]] }), null);
  assert.deepEqual(view.cleanupRequest(finished, { kind: "error", code: "cleanup_unavailable", message: "" }), [3, 2], "a failed read keeps what the owner saw; the engine checks each one again");
  assert.equal(view.cleanupRequest(null, { kind: "ok", value: finished }), null);

  const changed = text(render({ list: { kind: "ok", value: late }, asked: finished }));
  assert.match(changed, /Remove 2 finished agents\?/);
  assert.match(changed, /Ada \(@login-page\) Codex 1 GB/);
  assert.doesNotMatch(changed, /Lin \(@docs\)/);
  assert.match(changed, /The list changed\. Review it again before anything is removed\./);
  assert.match(changed, /Keep them Review the new list/);
  assert.doesNotMatch(changed, /Remove and free/);

  const same = text(render({ list: { kind: "ok", value: finished }, asked: finished }));
  assert.doesNotMatch(same, /The list changed/);
  assert.match(same, /Keep them Remove and free 1\.5 GB/);
});
