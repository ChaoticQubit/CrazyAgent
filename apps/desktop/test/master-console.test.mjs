import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { consoleLines } = await import("../src/screens/project/conversation.tsx");

const turn = (patch) => ({ id: "t1", ordinal: 1, phase: "succeeded", reason: null, reply: null, ...patch });

test("master turns become console lines: the owner's words, replies, engine and team lines in order", () => {
  const turns = [
    turn({ id: "t1", reply: { text: "I handed it to Ada." }, tasks: [{ task: 8, note: null }], changes: "Its edits wait in Changes." }),
    turn({ id: "t2", ordinal: 2, phase: "failed", reason: "quota_exhausted" }),
    turn({ id: "t3", ordinal: 3, phase: "succeeded", reply: { text: "Done." } }),
  ];
  const lines = consoleLines(turns, { t1: "Ask @page to fix the header", t2: "Again" }, {}, 0);
  assert.deepEqual(lines.map((line) => [line.id, line.kind]), [
    ["t1-owner", "owner"], ["t1-reply", "reply"], ["t1-task-0", "delegate"], ["t1-changes", "tool"],
    ["t2-owner", "owner"], ["t2-status", "error"],
    ["t3-owner", "engine"], ["t3-reply", "reply"],
  ]);
  assert.equal(lines[0].text, "Ask @page to fix the header");
  assert.equal(lines[2].text, "Task 8 is still with a sub-agent.");
  assert.equal(lines[6].text, "Your message 3 (its text stays in the window that sent it)");
});

test("a message this window sent shows with a thinking line until its turn arrives", () => {
  const lines = consoleLines([], { op9: "Plan the page" }, { op9: 1000 }, 4000);
  assert.deepEqual(lines.map((line) => [line.id, line.kind]), [["op9-owner", "owner"], ["op9-status", "engine"]]);
  assert.match(lines[1].text, /^Thinking/);
});

test("a master reply in the console renders as Markdown with its links, and the owner's text stays plain", async () => {
  const { Console } = await import("../src/screens/project/console.tsx");
  const lines = consoleLines([turn({ reply: { text: "Done:\n\n- **header** fixed\n- see [the brief](https://example.com/brief)" } })], { t1: "**not bold**" }, {}, 0);
  const html = renderToStaticMarkup(createElement(Console, { projectId: "p-1", lines, agents: [], onSend: () => {}, onStop: () => {}, onNavigate: () => {} }));
  assert.match(html, /<strong>header<\/strong>/, html);
  assert.match(html, /<li>/, html);
  assert.match(html, /href="https:\/\/example\.com\/brief"/, html);
  assert.match(html, /\*\*not bold\*\*/, "the owner's line is not Markdown");
});
