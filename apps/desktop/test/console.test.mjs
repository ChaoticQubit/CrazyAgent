import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { bundle, withPage } from "./browser.mjs";

const { Console } = await import("../src/screens/project/console.tsx");
const { agentMentions } = await import("../src/screens/project/console-model.ts");
const fixture = await bundle(new URL("./fixtures/console.mjs", import.meta.url).pathname);
const tick = "new Promise((done) => setTimeout(done, 0))";
const CTRL = 2;
const SHIFT = 8;

const graphNode = (id, label) => ({ id, kind: "agent", parent: "g", label, harness: "codex", status: "working", status_label: null, account: null, agents: null, terminal: null });

test("the console draws a terminal transcript: > owner lines with tags, Markdown replies, dim engine lines and prefixed tool and delegate lines", () => {
  const html = renderToStaticMarkup(createElement(Console, {
    projectId: "p-1",
    lines: [
      { id: "1", kind: "owner", text: "Ask @onboarding-fix and @nobody <b>now</b>" },
      { id: "2", kind: "reply", text: "Done.\nTwo lines." },
      { id: "3", kind: "engine", text: "The master started." },
      { id: "4", kind: "tool", text: "cargo test" },
      { id: "5", kind: "delegate", text: "Ada (@onboarding-fix): Fix the login" },
      { id: "6", kind: "error", text: "The master stopped." },
    ],
    agents: agentMentions([graphNode("team-1", "Ada (@onboarding-fix)")]), files: null, status: "Thinking...",
    onSend() {}, onStop() {}, onNavigate() {},
  }));
  assert.match(html, /<section class="cs" aria-label="Master console">/);
  assert.match(html, /<p class="cs-line cs-owner"><span class="cs-mark" aria-hidden="true">&gt; <\/span>Ask <span class="cs-tag">@onboarding-fix<\/span> and @nobody &lt;b&gt;now&lt;\/b&gt;<\/p>/);
  assert.match(html, /<div class="cs-line cs-reply"><div class="markdown"><p>Done\.\nTwo lines\.<\/p><\/div><\/div>/);
  assert.match(html, /<p class="cs-line cs-engine">The master started\.<\/p>/);
  assert.match(html, /<p class="cs-line cs-tool"><span class="cs-mark" aria-hidden="true">\$ <\/span>cargo test<\/p>/);
  assert.match(html, /<p class="cs-line cs-delegate"><span class="cs-mark" aria-hidden="true">→ <\/span>Ada \(<span class="cs-tag">@onboarding-fix<\/span>\): Fix the login<\/p>/);
  assert.match(html, /<p class="cs-line cs-error"><span class="cs-mark" aria-hidden="true">! <\/span>The master stopped\.<\/p>/);
  assert.match(html, /role="log"/);
  assert.match(html, /<textarea[^>]*aria-label="Message the master"[^>]*rows="1"/);
  assert.match(html, /aria-expanded="false"/);
  assert.doesNotMatch(html, /cs-menu/);
  assert.match(html, /<p class="cs-status" role="status">Thinking\.\.\.<\/p>/);
  assert.doesNotMatch(html, /pj-msg|bubble|card/, "no chat bubbles or cards");
});

test("a master reply's Markdown is formatted terminal-style, and HTML or images in it stay plain text", () => {
  const reply = [
    "## Plan",
    "",
    "- **Add** the `cart` route",
    "- Test it _twice_",
    "",
    "1. Build",
    "2. Ship",
    "",
    "```ts",
    "const tag = '<b>x</b>';",
    "```",
    "",
    "See [the docs](https://example.com/docs) or [bad](javascript:alert(1)).",
    "<script>alert(1)</script> <img src=x onerror=alert(1)> ![cat](https://example.com/cat.png)",
  ].join("\n");
  const html = renderToStaticMarkup(createElement(Console, {
    projectId: "p-1", lines: [{ id: "1", kind: "reply", text: reply }], agents: [], files: null,
    onSend() {}, onStop() {}, onNavigate() {},
  }));
  assert.match(html, /<div class="markdown-heading"><h3>Plan<\/h3><\/div>/, "a heading is a bold line");
  assert.match(html, /<ul><li><strong>Add<\/strong> the <code>cart<\/code> route<\/li><li>Test it <em>twice<\/em><\/li><\/ul>/);
  assert.match(html, /<ol><li>Build<\/li><li>Ship<\/li><\/ol>/);
  assert.match(html, /<pre><code>const tag = &#x27;&lt;b&gt;x&lt;\/b&gt;&#x27;;<\/code><\/pre>/, "a fenced block keeps its text in a monospace box");
  assert.match(html, /<a href="https:\/\/example\.com\/docs" title="https:\/\/example\.com\/docs" target="_blank" rel="noopener noreferrer">the docs<\/a>/);
  assert.doesNotMatch(html, /href="javascript/i, "only http and https links become links");
  assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt; &lt;img src=x onerror=alert\(1\)&gt;/, "raw HTML shows as text");
  assert.doesNotMatch(html, /<script|<img/i, "no element comes from the reply's HTML or image syntax");
});

async function key(send, keyName, code, vk, modifiers = 0, text) {
  for (const type of ["keyDown", "keyUp"]) {
    await send("Input.dispatchKeyEvent", { type, key: keyName, code, windowsVirtualKeyCode: vk, modifiers, ...(type === "keyDown" && text ? { text } : {}) });
  }
}

test("in a real page: @ inserts a tag, Shift+Enter and Ctrl+J add lines, IME Enter never sends, Enter sends and Up recalls it", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    const id = await evaluate("Fixture.mount()");
    await evaluate(`document.getElementById(${JSON.stringify(id)}).focus()`);
    const read = async () => { await evaluate(tick); return evaluate("Fixture.state()"); };
    const steps = {};
    await send("Input.insertText", { text: "ask @" });
    steps.menu = await read();
    await key(send, "ArrowDown", "ArrowDown", 40);
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.tagged = await read();
    await send("Input.insertText", { text: "now" });
    await key(send, "Enter", "Enter", 13, SHIFT, "\r");
    await send("Input.insertText", { text: "second" });
    await key(send, "j", "KeyJ", 74, CTRL);
    await send("Input.insertText", { text: "third" });
    steps.lines = await read();
    await send("Input.imeSetComposition", { text: "かな", selectionStart: 2, selectionEnd: 2 });
    steps.composingPrevented = await evaluate(`Fixture.syntheticEnter(${JSON.stringify(id)}, { isComposing: true })`);
    steps.webkitPrevented = await evaluate(`Fixture.syntheticEnter(${JSON.stringify(id)}, { keyCode: 229 })`);
    await send("Input.insertText", { text: "かな" });
    steps.composed = await read();
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.sent = await read();
    await key(send, "ArrowUp", "ArrowUp", 38);
    steps.recalled = await read();
    await key(send, "ArrowDown", "ArrowDown", 40);
    steps.forward = await read();
    await send("Input.insertText", { text: "draft" });
    await key(send, "Escape", "Escape", 27);
    steps.cleared = await read();
    return steps;
  });
  assert.deepEqual(seen.menu.options, ["@masterMaster", "@onboarding-fixAda (@onboarding-fix)", "@graceGrace"]);
  assert.equal(seen.menu.expanded, "true");
  assert.equal(seen.menu.status, "@ completes agents only: this project's file list is not available.");
  assert.deepEqual(seen.menu.queries, [null, ""], "the parent hears the tag text so it can read matching files");
  assert.equal(seen.tagged.value, "ask @onboarding-fix ");
  assert.equal(seen.tagged.queries.at(-1), null, "a finished tag ends the query");
  assert.deepEqual(seen.tagged.options, []);
  assert.equal(seen.lines.value, "ask @onboarding-fix now\nsecond\nthird");
  assert.equal(seen.lines.rows, 3, "the prompt grows with its lines");
  assert.deepEqual(seen.lines.sent, []);
  assert.equal(seen.composingPrevented, false, "the console leaves the composing Enter alone");
  assert.equal(seen.webkitPrevented, false);
  assert.deepEqual(seen.composed.sent, [], "nothing is sent while the input method composes");
  assert.deepEqual(seen.sent.sent, ["ask @onboarding-fix now\nsecond\nthirdかな"]);
  assert.equal(seen.sent.value, "");
  assert.equal(seen.recalled.value, "ask @onboarding-fix now\nsecond\nthirdかな");
  assert.equal(seen.forward.value, "");
  assert.equal(seen.cleared.value, "");
});

test("in a real page: / opens the command menu, Enter runs the chosen command, unknown commands say so and /clear empties the view", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    const id = await evaluate("Fixture.mount()");
    await evaluate(`document.getElementById(${JSON.stringify(id)}).focus()`);
    const read = async () => { await evaluate(tick); return evaluate("Fixture.state()"); };
    const steps = {};
    await send("Input.insertText", { text: "/" });
    steps.menu = await read();
    await send("Input.insertText", { text: "ag" });
    steps.filtered = await read();
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.navigated = await read();
    await send("Input.insertText", { text: "/stop" });
    await key(send, "Escape", "Escape", 27);
    steps.closed = await read();
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.stopped = await read();
    await send("Input.insertText", { text: "/deploy prod" });
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.unknown = await read();
    await send("Input.insertText", { text: "/help" });
    await key(send, "Escape", "Escape", 27);
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.help = await read();
    await send("Input.insertText", { text: "/cl" });
    await key(send, "Tab", "Tab", 9);
    steps.completed = await read();
    await key(send, "Enter", "Enter", 13, 0, "\r");
    steps.cleared = await read();
    return steps;
  });
  assert.deepEqual(seen.menu.options.map((option) => option.split(" ")[0].replace(/[A-Z].*$/, "")), ["/help", "/clear", "/agents", "/changes", "/usage", "/process", "/stop"]);
  assert.equal(seen.menu.selected?.startsWith("/help"), true);
  assert.deepEqual(seen.filtered.options, ["/agentsOpen the Agents tab"]);
  assert.deepEqual(seen.navigated.routes, [{ name: "project", id: "p-1", tab: "agents" }]);
  assert.equal(seen.navigated.value, "");
  assert.deepEqual(seen.closed.options, [], "Esc closes the menu and keeps the text");
  assert.equal(seen.closed.value, "/stop");
  assert.equal(seen.stopped.stops, 1);
  assert.deepEqual(seen.stopped.sent, [], "a command is never sent to the master");
  assert.equal(seen.unknown.lines.at(-1), "Unknown command /deploy. Type /help to see the commands.");
  assert.deepEqual(seen.unknown.sent, []);
  assert.match(seen.help.lines.join("\n"), /\/stop\s+Stop the master's turn/);
  assert.equal(seen.completed.value, "/clear ");
  assert.equal(seen.cleared.lines.length, 0, "/clear empties the view");
  assert.deepEqual(seen.cleared.sent, []);
});
