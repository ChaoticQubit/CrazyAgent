import "../test/load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { PoolEditorView } = await import("../src/screens/settings/pools.tsx");

const account = (id, name) => ({ id, name, provider: "local", provider_name: "Local models (Ollama)", method: "url", method_label: "Connect a URL",
  url: "http://127.0.0.1:11434/v1", model: `${id}-model`, status: "set_up", status_label: "Set up" });
const forbidden = () => { throw new Error("static rendering must not save or navigate"); };

test("the pool editor shows saved accounts as explicit checkboxes and keeps members the list does not show", () => {
  const pool = { id: "retained-pool", name: "Retained", summary: "2 accounts", project_count: 0, members: [
    { id: "off-list", kind: "url", provider: "local", status: "set_up", status_label: "Set up" },
    { id: "first", kind: "url", provider: "local", status: "set_up", status_label: "Set up" }] };
  const markup = renderToStaticMarkup(createElement(PoolEditorView, { pool, accounts: { kind: "ok", value: [account("first", "First"), account("second", "Second")] },
    onSave: forbidden, onCancel: forbidden }));
  const rows = [...markup.matchAll(/<label class="pick"[^>]*>([\s\S]*?)<\/label>/g)].map((match) => match[1]);
  const row = (name) => rows.find((text) => text.includes(`>${name}<`));
  assert.match(row("First"), /<input type="checkbox" checked=""/);
  assert.match(row("Second"), /<input type="checkbox"/);
  assert.doesNotMatch(row("Second"), /checked=""/);
  for (const text of rows) assert.doesNotMatch(text, /\sdisabled=""/);
  assert.match(markup, /2 of 15/);
  assert.equal((markup.match(/class="member"/g) ?? []).length, 2, "a member missing from the account list stays in the pool");
  assert.doesNotMatch(markup, /(?:Signed in as|Account authenticated|Model ready)/);
  assert.deepEqual(pool.members.map((member) => member.id), ["off-list", "first"]);
  assert.ok(Buffer.byteLength(markup, "utf8") < 32768);
});
